use anyhow::{Context, Result, bail};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::SystemTime;

use crate::config::Config;
use crate::mount;
use crate::nts;
use crate::player;

const NTS_ENTRY: &str = "[NTS Radio]";
const LOCAL_ENTRY: &str = "[local library]";
const SHUFFLE_ENTRY: &str = "[shuffle all]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    Local(Vec<PathBuf>),
    Nts(Vec<nts::Station>),
}

pub fn browse(config: &Config) -> Result<()> {
    match pick(config, true)? {
        Some(Selection::Local(paths)) => play_paths(config, &paths),
        Some(Selection::Nts(stations)) => nts::play_many(config, &stations),
        None => Ok(()),
    }
}

/// Picker shared by the CLI and the TUI. NTS is always available; the local
/// library is listed when mounted, or mounted on demand via its entry.
/// `include_all` adds the "[shuffle all]" entry (play mode, not add mode).
pub fn pick(config: &Config, include_all: bool) -> Result<Option<Selection>> {
    let mut choices = vec![NTS_ENTRY.to_string()];
    if include_all {
        choices.push(SHUFFLE_ENTRY.to_string());
    }
    if mount::is_mounted(&config.music_dir) {
        let artists = list_dirs(&config.music_dir)?;
        choices.extend(artists.iter().filter_map(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        }));
        choices.extend(
            list_audio_files(&config.music_dir)?
                .iter()
                .filter_map(|path| {
                    path.file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                }),
        );
    } else {
        choices.push(LOCAL_ENTRY.to_string());
    }

    let prompt = format!("--prompt={}> ", if include_all { "Play" } else { "Add" });
    let picked = fzf(
        &choices,
        &[
            "--reverse",
            "--height=100%",
            "--no-sort",
            &prompt,
            "--header=Enter: choose · Esc: cancel",
        ],
    )?;
    let Some(choice) = picked.into_iter().next() else {
        return Ok(None);
    };

    if choice == NTS_ENTRY {
        return Ok(nts::pick(true)?.map(Selection::Nts));
    }
    if choice == SHUFFLE_ENTRY {
        mount::ensure_mount(config)?;
        return Ok(Some(Selection::Local(vec![config.music_dir.clone()])));
    }
    if choice == LOCAL_ENTRY {
        mount::ensure_mount(config)?;
        return pick(config, include_all);
    }

    mount::ensure_mount(config)?;
    let artist_path = config.music_dir.join(&choice);
    if artist_path.is_file() {
        return Ok(Some(Selection::Local(vec![artist_path])));
    }
    if !artist_path.is_dir() {
        bail!("no such artist: {choice}");
    }
    let albums = list_dirs(&artist_path)?;
    let loose = list_audio_files(&artist_path)?;
    if albums.is_empty() && loose.is_empty() {
        return Ok(Some(Selection::Local(vec![artist_path])));
    }

    let mut album_choices = vec!["[whole artist]".to_string()];
    album_choices.extend(albums.iter().filter_map(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
    }));
    album_choices.extend(loose.iter().filter_map(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
    }));

    let picks = fzf(
        &album_choices,
        &[
            "--reverse",
            "--height=100%",
            "--multi",
            "--no-sort",
            &format!("--prompt={choice} > "),
            "--header=Tab: multi-select · Enter: play · Esc: cancel",
        ],
    )?;
    if picks.is_empty() {
        return Ok(None);
    }

    if picks.iter().any(|choice| choice == "[whole artist]") {
        return Ok(Some(Selection::Local(artist_inputs(&artist_path, albums))));
    }

    let paths: Vec<PathBuf> = picks
        .into_iter()
        .map(|choice| artist_path.join(choice))
        .collect();

    Ok(Some(Selection::Local(paths)))
}

/// Play a previous picker selection. A single music_dir means "shuffle all".
pub fn play_paths(config: &Config, paths: &[PathBuf]) -> Result<()> {
    if paths.len() == 1 && paths[0] == config.music_dir {
        player::load_directory(config, &config.music_dir, true)
    } else {
        player::load_replace(config, paths, false)
    }
}

fn list_dirs(path: &Path) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    for entry in std::fs::read_dir(path)
        .with_context(|| format!("reading {}", path.display()))?
        .filter_map(Result::ok)
    {
        if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            dirs.push(entry.path());
        }
    }
    Ok(sort_dirs_by_added(dirs))
}

fn list_audio_files(path: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in std::fs::read_dir(path)
        .with_context(|| format!("reading {}", path.display()))?
        .flatten()
    {
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if entry
            .file_type()
            .map(|kind| kind.is_file())
            .unwrap_or(false)
            && crate::queue::is_audio(&entry.path())
        {
            files.push(entry.path());
        }
    }
    files.sort();
    Ok(files)
}

fn sort_dirs_by_added(dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    // Directory mtimes update when entries are added. Using the direct mtime
    // avoids recursively walking every album/disc just to sort the parent.
    let mut ranked: Vec<(SystemTime, PathBuf)> = dirs
        .into_iter()
        .map(|path| (modified(&path), path))
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    ranked.into_iter().map(|(_, path)| path).collect()
}

fn modified(path: &Path) -> SystemTime {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// Expand a whole artist in the same newest-first album order shown by the
/// picker. Loose audio files in the artist directory are appended in name order.
fn artist_inputs(artist: &Path, mut albums: Vec<PathBuf>) -> Vec<PathBuf> {
    albums.extend(list_audio_files(artist).unwrap_or_default());
    if albums.is_empty() {
        vec![artist.to_path_buf()]
    } else {
        albums
    }
}

pub(crate) fn fzf(lines: &[String], args: &[&str]) -> Result<Vec<String>> {
    let mut child = Command::new("fzf")
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .context("starting fzf")?;

    {
        let stdin = child.stdin.as_mut().context("opening fzf stdin")?;
        let mut write_failed = false;
        for line in lines {
            if writeln!(stdin, "{line}").is_err() {
                write_failed = true;
                break;
            }
        }
        if write_failed {
            let _ = child.wait();
            return Ok(Vec::new());
        }
    }

    let output = child.wait_with_output().context("waiting for fzf")?;
    if !output.status.success() {
        // Esc / no selection.
        return Ok(Vec::new());
    }

    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set_mtime(path: &Path, seconds: i64) {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let path = CString::new(path.as_os_str().as_bytes()).unwrap();
        let time = libc::timespec {
            tv_sec: seconds as libc::time_t,
            tv_nsec: 0,
        };
        let times = [time, time];
        let result = unsafe { libc::utimensat(libc::AT_FDCWD, path.as_ptr(), times.as_ptr(), 0) };
        assert_eq!(result, 0);
    }

    #[test]
    fn lists_directories_newest_first() {
        let base = crate::test_support::unique_base("browse-order");
        let artist = base.join("Artist");
        for (name, seconds) in [("A", 1_000), ("B", 3_000), ("C", 2_000)] {
            let path = artist.join(name);
            std::fs::create_dir_all(&path).unwrap();
            set_mtime(&path, seconds);
        }

        let names: Vec<String> = list_dirs(&artist)
            .unwrap()
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["B", "C", "A"]);

        // A disc added later makes its album the newest.
        let disc = artist.join("A/disc1");
        std::fs::create_dir_all(&disc).unwrap();
        set_mtime(&disc, 4_000);
        let names: Vec<String> = list_dirs(&artist)
            .unwrap()
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["A", "B", "C"]);
        assert!(modified(&artist.join("A")) > SystemTime::UNIX_EPOCH);

        // Whole-artist expansion follows the same album order, then loose files.
        let loose = artist.join("loose.flac");
        std::fs::write(&loose, b"").unwrap();
        std::fs::write(artist.join("cover.jpg"), b"").unwrap();
        std::fs::write(artist.join(".hidden.flac"), b"").unwrap();
        assert_eq!(list_audio_files(&artist).unwrap(), vec![loose.clone()]);
        let albums = list_dirs(&artist).unwrap();
        let inputs: Vec<PathBuf> = artist_inputs(&artist, albums);
        assert_eq!(inputs.last(), Some(&loose));
        assert_eq!(inputs.len(), 4);

        let _ = std::fs::remove_dir_all(&base);
    }
}
