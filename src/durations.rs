use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use lofty::file::AudioFile;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Config;
use crate::mpv::PlaylistEntry;

/// Durations learned for queue entries. Local files are probed once; URL
/// entries are filled from yt-dlp metadata or as mpv reports them playing.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Cache {
    #[serde(default)]
    entries: HashMap<String, f64>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Totals {
    pub seconds: f64,
    pub missing: usize,
}

impl Totals {
    pub fn complete(&self) -> bool {
        self.missing == 0
    }

    /// Formatted total, with `+` when some entries have no known duration.
    pub fn label(&self) -> String {
        if self.seconds <= 0.0 {
            return "--:--".to_string();
        }
        let mut label = crate::player::format_time(Some(self.seconds));
        if self.missing > 0 {
            label.push('+');
        }
        label
    }
}

pub fn totals(values: &[Option<f64>]) -> Totals {
    let mut totals = Totals::default();
    for value in values {
        match value {
            Some(seconds) if seconds.is_finite() && *seconds > 0.0 => totals.seconds += *seconds,
            _ => totals.missing += 1,
        }
    }
    totals
}

/// Seconds already played in the queue up to the current position, plus the
/// number of earlier entries whose duration is unknown.
pub fn elapsed(
    values: &[Option<f64>],
    position: i64,
    time_pos: Option<f64>,
) -> Option<(f64, usize)> {
    let index = usize::try_from(position).ok()?;
    if index >= values.len() {
        return None;
    }
    let mut seconds = 0.0;
    let mut missing = 0;
    for value in &values[..index] {
        match value {
            Some(seconds_known) if seconds_known.is_finite() && *seconds_known > 0.0 => {
                seconds += *seconds_known;
            }
            _ => missing += 1,
        }
    }
    Some((seconds + time_pos.unwrap_or(0.0), missing))
}

/// Stable signature of the queue's filenames, used to avoid re-probing a
/// playlist that only changed position.
pub fn signature(entries: &[PlaylistEntry]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for entry in entries {
        for byte in entry.filename.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash ^= u64::from(b'\n');
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Resolve durations for a queue, reusing the on-disk cache. With
/// `probe_network`, URL entries that are still unknown are queried through
/// yt-dlp (one call per source playlist). Never blocks on missing tools.
pub fn resolve(
    config: &Config,
    entries: &[PlaylistEntry],
    probe_network: bool,
) -> Vec<Option<f64>> {
    let mut cache = read_cache(config);
    let mut changed = false;
    let mut values: Vec<Option<f64>> = entries
        .iter()
        .map(|entry| cache.entries.get(&entry.filename).copied())
        .collect();

    // Probe uncached local files in parallel: each one is a header read over
    // the rclone mount, so a large queue benefits from overlapping I/O.
    let pending: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(index, entry)| values[*index].is_none() && !crate::queue::is_url(&entry.filename))
        .map(|(index, _)| index)
        .collect();
    for (index, seconds) in probe_local(entries, &pending) {
        if let Some(seconds) = seconds {
            values[index] = Some(seconds);
            cache
                .entries
                .insert(entries[index].filename.clone(), seconds);
            changed = true;
        }
    }

    if probe_network && values.iter().any(Option::is_none) {
        changed |= probe_urls(entries, &mut values, &mut cache);
    }
    if changed {
        write_cache(config, &cache);
    }
    values
}

/// Remember a duration reported by mpv. Live streams change length with every
/// show, so they are never cached.
pub fn remember(config: &Config, path: &str, seconds: Option<f64>) {
    let Some(seconds) = seconds.filter(|value| value.is_finite() && *value > 0.0) else {
        return;
    };
    if path.is_empty() || crate::nts::is_live_stream(path) {
        return;
    }
    let mut cache = read_cache(config);
    if let Some(existing) = cache.entries.get(path)
        && (existing - seconds).abs() < 1.0
    {
        return;
    }
    cache.entries.insert(path.to_string(), seconds);
    write_cache(config, &cache);
}

fn probe_local(entries: &[PlaylistEntry], indexes: &[usize]) -> Vec<(usize, Option<f64>)> {
    let workers = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(4)
        .clamp(1, 8);
    if indexes.len() <= 1 || workers == 1 {
        return indexes
            .iter()
            .map(|&index| (index, local_duration(Path::new(&entries[index].filename))))
            .collect();
    }

    let chunk = indexes.len().div_ceil(workers);
    std::thread::scope(|scope| {
        let handles: Vec<_> = indexes
            .chunks(chunk)
            .map(|chunk| {
                scope.spawn(move || {
                    chunk
                        .iter()
                        .map(|&index| (index, local_duration(Path::new(&entries[index].filename))))
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap_or_default())
            .collect()
    })
}

fn local_duration(path: &Path) -> Option<f64> {
    if let Ok(probe) = lofty::probe::Probe::open(path)
        && let Ok(tagged) = probe.read()
    {
        let seconds = tagged.properties().duration().as_secs_f64();
        if seconds.is_finite() && seconds > 0.0 {
            return Some(seconds);
        }
    }
    ffprobe_duration(path)
}

fn ffprobe_duration(path: &Path) -> Option<f64> {
    let output = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let seconds = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<f64>()
        .ok()?;
    (seconds.is_finite() && seconds > 0.0).then_some(seconds)
}

/// Probe URL entries in groups that share a source playlist. A whole album is
/// one yt-dlp call rather than one request per track.
fn probe_urls(entries: &[PlaylistEntry], values: &mut [Option<f64>], cache: &mut Cache) -> bool {
    // Bound the work a single queue listing can trigger. Albums usually need
    // one group; queues of many unrelated single-track URLs degrade to a
    // partial total instead of a long stall.
    const MAX_GROUPS: usize = 4;

    let mut groups: Vec<(String, Vec<usize>)> = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        if values[index].is_some() || !crate::queue::is_url(&entry.filename) {
            continue;
        }
        if crate::nts::is_live_stream(&entry.filename) {
            continue;
        }
        let source = entry
            .playlist_path
            .clone()
            .unwrap_or_else(|| entry.filename.clone());
        match groups.iter_mut().find(|(known, _)| *known == source) {
            Some((_, indexes)) => indexes.push(index),
            None => groups.push((source, vec![index])),
        }
    }

    let mut changed = false;
    for (source, indexes) in groups.into_iter().take(MAX_GROUPS) {
        let Some(found) = ytdl_durations(&source) else {
            continue;
        };
        for (position, index) in indexes.iter().copied().enumerate() {
            let filename = entries[index].filename.clone();
            let seconds = found
                .by_url
                .get(filename.trim_end_matches('/'))
                .copied()
                .or_else(|| {
                    (found.order.len() == indexes.len())
                        .then(|| found.order[position])
                        .flatten()
                });
            let Some(seconds) = seconds.filter(|value| value.is_finite() && *value > 0.0) else {
                continue;
            };
            values[index] = Some(seconds);
            cache.entries.insert(filename, seconds);
            changed = true;
        }
    }
    changed
}

#[derive(Debug, Default)]
struct Found {
    by_url: HashMap<String, f64>,
    order: Vec<Option<f64>>,
}

fn ytdl_durations(source: &str) -> Option<Found> {
    let output = Command::new("yt-dlp")
        .args([
            "-J",
            "--no-warnings",
            "--socket-timeout",
            "10",
            "--retries",
            "1",
        ])
        .arg(source)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value: Value = serde_json::from_slice(&output.stdout).ok()?;
    let mut found = Found::default();
    match value.get("entries").and_then(Value::as_array) {
        Some(entries) => {
            for entry in entries {
                add_duration(&mut found, entry);
            }
        }
        None => add_duration(&mut found, &value),
    }
    Some(found)
}

fn add_duration(found: &mut Found, item: &Value) {
    let seconds = item
        .get("duration")
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value > 0.0);
    if let Some(seconds) = seconds {
        for key in ["webpage_url", "url", "original_url"] {
            if let Some(url) = item.get(key).and_then(Value::as_str) {
                found
                    .by_url
                    .insert(url.trim_end_matches('/').to_string(), seconds);
            }
        }
    }
    found.order.push(seconds);
}

fn cache_path(config: &Config) -> PathBuf {
    config.cache_dir.join("durations.json")
}

fn read_cache(config: &Config) -> Cache {
    std::fs::read(cache_path(config))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_cache(config: &Config, cache: &Cache) {
    if std::fs::create_dir_all(&config.cache_dir).is_err() {
        return;
    }
    let path = cache_path(config);
    let temporary = path.with_extension("json.tmp");
    let Ok(text) = serde_json::to_vec(cache) else {
        return;
    };
    if std::fs::write(&temporary, text).is_ok() {
        let _ = std::fs::rename(&temporary, &path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{test_config, unique_base};

    fn entry(name: &str) -> PlaylistEntry {
        PlaylistEntry {
            filename: name.to_string(),
            title: None,
            playlist_path: None,
        }
    }

    #[test]
    fn totals_include_known_durations_and_count_missing() {
        let mixed = totals(&[Some(60.0), None, Some(90.0)]);
        assert_eq!(mixed.seconds, 150.0);
        assert_eq!(mixed.missing, 1);
        assert_eq!(mixed.label(), "2:30+");

        assert_eq!(totals(&[Some(60.0)]).label(), "1:00");
        assert!(totals(&[Some(60.0)]).complete());
        assert_eq!(totals(&[None]).label(), "--:--");
    }

    #[test]
    fn elapsed_adds_completed_tracks_and_reports_unknowns() {
        let values = [Some(60.0), None, Some(90.0)];
        assert_eq!(elapsed(&values, 0, Some(12.0)), Some((12.0, 0)));
        assert_eq!(elapsed(&values, 2, Some(12.0)), Some((72.0, 1)));
        assert_eq!(elapsed(&values, 3, Some(12.0)), None);
        assert_eq!(elapsed(&values, -1, Some(12.0)), None);
    }

    #[test]
    fn signature_changes_with_queue_contents() {
        let first = [entry("a"), entry("b")];
        let same = [entry("a"), entry("b")];
        let different = [entry("a"), entry("c")];
        assert_eq!(signature(&first), signature(&same));
        assert_ne!(signature(&first), signature(&different));
        assert_ne!(signature(&first), signature(&[]));
    }

    #[test]
    fn reuses_cached_durations() {
        let base = unique_base("durations-cache");
        let config = test_config(base.join("Music"), &base);
        remember(&config, "https://example.com/one", Some(61.0));

        let resolved = resolve(&config, &[entry("https://example.com/one")], false);
        assert_eq!(resolved, vec![Some(61.0)]);

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn never_remembers_live_stream_lengths() {
        let base = unique_base("durations-live");
        let config = test_config(base.join("Music"), &base);
        for url in [
            "https://stream-relay-geo.ntslive.net/stream?client=direct",
            "https://stream-relay-geo.ntslive.net/stream2?client=direct",
        ] {
            remember(&config, url, Some(3000.0));
        }
        assert!(resolve(&config, &[], false).is_empty());
        assert!(!cache_path(&config).is_file());

        let _ = std::fs::remove_dir_all(&base);
    }
}
