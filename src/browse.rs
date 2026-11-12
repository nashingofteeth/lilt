use anyhow::{Context, Result, bail};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

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
    Nts(Vec<String>),
}

pub fn browse(config: &Config) -> Result<()> {
    match pick(config, true)? {
        Some(Selection::Local(paths)) => play_paths(config, &paths),
        Some(Selection::Nts(targets)) => nts::play_many(config, &targets),
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
    } else {
        choices.push(LOCAL_ENTRY.to_string());
    }

    let prompt = format!("--prompt={}> ", if include_all { "Play" } else { "Add" });
    let picked = fzf(
        &choices,
        &[
            "--reverse",
            "--height=100%",
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
    if !artist_path.is_dir() {
        bail!("no such artist: {choice}");
    }
    let albums = list_dirs(&artist_path)?;
    if albums.is_empty() {
        return Ok(Some(Selection::Local(vec![artist_path])));
    }

    let mut album_choices = vec!["[whole artist]".to_string()];
    album_choices.extend(albums.iter().filter_map(|path| {
        path.file_name()
            .map(|name| name.to_string_lossy().into_owned())
    }));

    let picks = fzf(
        &album_choices,
        &[
            "--reverse",
            "--height=100%",
            "--multi",
            &format!("--prompt={choice} > "),
            "--header=Tab: multi-select · Enter: play · Esc: cancel",
        ],
    )?;
    if picks.is_empty() {
        return Ok(None);
    }

    let paths: Vec<PathBuf> = picks
        .into_iter()
        .map(|choice| {
            if choice == "[whole artist]" {
                artist_path.clone()
            } else {
                artist_path.join(choice)
            }
        })
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

fn list_dirs(path: &std::path::Path) -> Result<Vec<PathBuf>> {
    let mut dirs = Vec::new();
    for entry in std::fs::read_dir(path)
        .with_context(|| format!("reading {}", path.display()))?
        .filter_map(Result::ok)
    {
        if entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false) {
            dirs.push(entry.path());
        }
    }
    dirs.sort();
    Ok(dirs)
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
