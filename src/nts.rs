use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crate::config::Config;
use crate::player;

const LIVE_1: &str = "https://stream-relay-geo.ntslive.net/stream?client=direct";
const LIVE_2: &str = "https://stream-relay-geo.ntslive.net/stream2?client=direct";
const LIVE_API: &str = "https://www.nts.live/api/v2/live";
const MIXTAPES_API: &str = "https://www.nts.live/api/v2/mixtapes";

/// NTS station metadata persisted between the CLI and the control panel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Station {
    pub key: String,
    pub title: String,
    pub subtitle: String,
    pub artist: String,
    pub album: String,
    pub stream_url: String,
    pub cover_url: Option<String>,
    pub cover_path: Option<PathBuf>,
    pub live: bool,
}

impl Station {
    fn label(&self) -> String {
        if self.live {
            format!("{} — {}", self.artist, self.title)
        } else {
            format!("{} — {}", self.album, self.title)
        }
    }
}

pub fn play(config: &Config, target: Option<&str>) -> Result<()> {
    let station = resolve(target)?;
    let cover_path = station
        .cover_url
        .as_deref()
        .and_then(|url| download_cover(config, url));
    let station = Station {
        cover_path,
        ..station
    };
    save_current(config, &station)?;
    player::play(config, std::slice::from_ref(&station.stream_url), false)
}

pub fn resolve(target: Option<&str>) -> Result<Station> {
    let target = target.unwrap_or("1").trim();
    match target.to_ascii_lowercase().as_str() {
        "" | "1" | "nts1" | "nts 1" | "nts-1" => Ok(live_or_static(1)),
        "2" | "nts2" | "nts 2" | "nts-2" => Ok(live_or_static(2)),
        "list" => bail!("usage: lilt nts [1|2|list|ALIAS]"),
        _ => mixtape(target),
    }
}

pub fn list() -> Result<()> {
    for station in stations()? {
        println!("{:<20}  {}", station.key, station.label());
    }
    Ok(())
}

pub fn pick() -> Result<Option<String>> {
    let lines: Vec<String> = stations()?
        .iter()
        .map(|station| format!("{}\t{}", station.key, station.label()))
        .collect();
    let picked = crate::browse::fzf(
        &lines,
        &[
            "--delimiter=\t",
            "--with-nth=2",
            "--reverse",
            "--height=100%",
            "--prompt=NTS> ",
            "--header=Enter: play · Esc: cancel",
        ],
    )?;
    let Some(line) = picked.into_iter().next() else {
        return Ok(None);
    };
    Ok(line
        .split('\t')
        .next()
        .filter(|key| !key.is_empty())
        .map(str::to_string))
}

pub fn load_current(config: &Config) -> Option<Station> {
    let text = std::fs::read_to_string(current_path(config)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn current(config: &Config, path: &str) -> Option<Station> {
    let station = load_current(config)?;
    (station.stream_url == path).then_some(station)
}

/// Replace the mpv-reported stream URL with the NTS station identity.
pub fn apply_status(config: &Config, status: &mut crate::mpv::Status) {
    if let Some(station) = current(config, &status.path) {
        status.media_title = station.title;
        status.artist = station.artist;
        status.album = station.album;
        status.has_title_tag = true;
    }
}

pub fn cover(config: &Config, path: &str) -> Option<image::DynamicImage> {
    let station = current(config, path)?;
    let cover_path = station.cover_path?;
    crate::cover::load_path(&cover_path)
}

fn current_path(config: &Config) -> PathBuf {
    config.cache_dir.join("nts.json")
}

fn save_current(config: &Config, station: &Station) -> Result<()> {
    config.ensure_dirs()?;
    let text = serde_json::to_vec_pretty(station)?;
    std::fs::write(current_path(config), text).context("writing NTS metadata")
}

fn download_cover(config: &Config, url: &str) -> Option<PathBuf> {
    let covers = config.cache_dir.join("covers");
    std::fs::create_dir_all(&covers).ok()?;
    let hash = crate::cover::url_hash(url);
    let path = covers.join(format!("nts-{hash}.jpg"));
    if path.is_file() {
        return Some(path);
    }

    let status = Command::new("curl")
        .args(["-fsSL", "--max-time", "15", "-o"])
        .arg(&path)
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    if !status.success() {
        let _ = std::fs::remove_file(&path);
        return None;
    }
    Some(path)
}

fn curl_json(url: &str) -> Result<Value> {
    let output = Command::new("curl")
        .args(["-fsSL", "--max-time", "15", "-A", "lilt/0.1", url])
        .output()
        .context("running curl (needed for NTS metadata)")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("NTS request failed: {url}\n{}", stderr.trim());
    }
    serde_json::from_slice(&output.stdout)
        .with_context(|| format!("parsing NTS response from {url}"))
}

fn live_or_static(channel: u8) -> Station {
    live(channel).unwrap_or_else(|_| static_live(channel))
}

fn live(channel: u8) -> Result<Station> {
    let data = curl_json(LIVE_API)?;
    live_station(channel, &data).with_context(|| format!("NTS {channel} not in the live response"))
}

fn live_station(channel: u8, data: &Value) -> Option<Station> {
    let channel_name = channel.to_string();
    let result = data.get("results")?.as_array()?.iter().find(|result| {
        result.get("channel_name").and_then(Value::as_str) == Some(channel_name.as_str())
    })?;
    let now = result.get("now")?;
    let details = now.get("embeds").and_then(|embeds| embeds.get("details"));
    let title = details
        .and_then(|details| details.get("name"))
        .and_then(Value::as_str)
        .or_else(|| now.get("broadcast_title").and_then(Value::as_str))
        .unwrap_or("NTS");
    let subtitle = details
        .and_then(|details| details.get("description"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let cover_url = details
        .and_then(|details| details.get("media"))
        .and_then(|media| media.get("picture_medium"))
        .and_then(Value::as_str)
        .or_else(|| {
            details
                .and_then(|details| details.get("media"))
                .and_then(|media| media.get("picture_large"))
                .and_then(Value::as_str)
        });

    Some(Station {
        key: channel_name,
        title: decode_html(title),
        subtitle: decode_html(subtitle),
        artist: format!("NTS {channel}"),
        album: "NTS Radio".to_string(),
        stream_url: if channel == 1 { LIVE_1 } else { LIVE_2 }.to_string(),
        cover_url: cover_url.map(str::to_string),
        cover_path: None,
        live: true,
    })
}

fn static_live(channel: u8) -> Station {
    Station {
        key: channel.to_string(),
        title: format!("NTS {channel}"),
        subtitle: String::new(),
        artist: format!("NTS {channel}"),
        album: "NTS Radio".to_string(),
        stream_url: if channel == 1 { LIVE_1 } else { LIVE_2 }.to_string(),
        cover_url: None,
        cover_path: None,
        live: true,
    }
}

fn mixtapes() -> Result<Vec<Station>> {
    let data = curl_json(MIXTAPES_API)?;
    let results = data
        .get("results")
        .and_then(Value::as_array)
        .context("unexpected NTS mixtapes response")?;
    Ok(results.iter().filter_map(mixtape_from_value).collect())
}

fn mixtape(query: &str) -> Result<Station> {
    let stations = mixtapes()?;
    let wanted = query.trim().to_ascii_lowercase();
    if wanted.is_empty() {
        bail!("usage: lilt nts [1|2|list|ALIAS]");
    }
    stations
        .iter()
        .find(|station| {
            station.key.eq_ignore_ascii_case(&wanted)
                || station.title.to_ascii_lowercase() == wanted
                || station.title.to_ascii_lowercase().replace(' ', "-") == wanted
                || station.title.to_ascii_lowercase().contains(&wanted)
        })
        .cloned()
        .with_context(|| format!("unknown NTS mixtape: {query} (try `lilt nts list`)"))
}

fn mixtape_from_value(value: &Value) -> Option<Station> {
    let key = value.get("mixtape_alias")?.as_str()?.to_string();
    let title = decode_html(value.get("title")?.as_str()?);
    let subtitle = decode_html(
        value
            .get("subtitle")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    );
    let stream_url = value.get("audio_stream_endpoint")?.as_str()?.to_string();
    let cover_url = value
        .get("media")
        .and_then(|media| media.get("picture_medium"))
        .and_then(Value::as_str)
        .or_else(|| {
            value
                .get("media")
                .and_then(|media| media.get("picture_large"))
                .and_then(Value::as_str)
        });

    Some(Station {
        key,
        title,
        subtitle,
        artist: "NTS".to_string(),
        album: "Infinite Mixtapes".to_string(),
        stream_url,
        cover_url: cover_url.map(str::to_string),
        cover_path: None,
        live: false,
    })
}

fn stations() -> Result<Vec<Station>> {
    let mut stations = Vec::new();
    match curl_json(LIVE_API) {
        Ok(data) => {
            for channel in [1, 2] {
                stations.push(live_station(channel, &data).unwrap_or_else(|| static_live(channel)));
            }
        }
        Err(_) => {
            stations.push(static_live(1));
            stations.push(static_live(2));
        }
    }
    stations.extend(mixtapes()?);
    Ok(stations)
}

fn decode_html(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&#39;", "'")
        .replace("&quot;", "\"")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&nbsp;", " ")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_live_station_metadata() {
        let data = json!({
            "results": [{
                "channel_name": "1",
                "now": {
                    "broadcast_title": "Show &amp; Tell",
                    "embeds": {
                        "details": {
                            "name": "Show &amp; Tell",
                            "description": "A show",
                            "media": {"picture_medium": "https://example.com/show.jpg"}
                        }
                    }
                }
            }]
        });
        let station = live_station(1, &data).expect("live station");
        assert_eq!(station.title, "Show & Tell");
        assert_eq!(station.artist, "NTS 1");
        assert_eq!(station.album, "NTS Radio");
        assert_eq!(station.stream_url, LIVE_1);
        assert_eq!(
            station.cover_url.as_deref(),
            Some("https://example.com/show.jpg")
        );
        assert!(station.live);
    }

    #[test]
    fn parses_mixtape_metadata() {
        let value = json!({
            "mixtape_alias": "slow-focus",
            "title": "Slow Focus",
            "subtitle": "Meditative and beatless",
            "audio_stream_endpoint": "https://example.com/mixtape",
            "media": {"picture_medium": "https://example.com/mixtape.jpg"}
        });
        let station = mixtape_from_value(&value).expect("mixtape");
        assert_eq!(station.title, "Slow Focus");
        assert_eq!(station.artist, "NTS");
        assert_eq!(station.album, "Infinite Mixtapes");
        assert_eq!(station.stream_url, "https://example.com/mixtape");
        assert_eq!(
            station.cover_url.as_deref(),
            Some("https://example.com/mixtape.jpg")
        );
    }

    #[test]
    fn decodes_basic_html_entities() {
        assert_eq!(decode_html("A &amp; B &#39;live&#39;"), "A & B 'live'");
    }
}
