use anyhow::{Context, Result, bail};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::mpv;
use crate::platform;

pub fn is_mounted(dir: &Path) -> bool {
    platform::is_mounted(dir)
}

fn rclone_args(config: &Config) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> = vec![
        "mount".into(),
        config.remote.clone().into(),
        config.music_dir.clone().into_os_string(),
        "--read-only".into(),
        "--dir-cache-time=10m".into(),
    ];
    // Avoid AppleDouble sidecar traffic; the library is read-only anyway.
    if cfg!(target_os = "macos") {
        args.push("--noappledouble".into());
    }
    args
}

pub fn mount(config: &Config) -> Result<()> {
    if is_mounted(&config.music_dir) {
        println!("already mounted at {}", config.music_dir.display());
        return Ok(());
    }
    ensure_mount(config)?;
    println!("mounted {}", config.music_dir.display());
    Ok(())
}

pub fn ensure_mount(config: &Config) -> Result<()> {
    if is_mounted(&config.music_dir) {
        return Ok(());
    }

    config.ensure_dirs()?;
    std::fs::create_dir_all(&config.music_dir)
        .with_context(|| format!("creating {}", config.music_dir.display()))?;

    let mut child_pid = None;
    if let Some(pid) = read_pid(&config.rclone_pid) {
        if pid_is_our_rclone(pid, config) {
            child_pid = Some(pid);
        } else {
            let _ = std::fs::remove_file(&config.rclone_pid);
        }
    }

    if child_pid.is_none() {
        let log = mpv::open_log_for_append(&config.rclone_log)?;
        let mut command = Command::new("rclone");
        command.args(rclone_args(config));
        command.stdout(Stdio::from(log.try_clone()?));
        command.stderr(Stdio::from(log));
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = command.spawn().context("starting rclone mount")?;
        let pid = child.id();
        std::fs::write(&config.rclone_pid, pid.to_string())
            .with_context(|| format!("writing {}", config.rclone_pid.display()))?;
        child_pid = Some(pid);
    }

    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(15) {
        if is_mounted(&config.music_dir) {
            return Ok(());
        }
        if let Some(pid) = child_pid
            && !pid_alive(pid)
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    if is_mounted(&config.music_dir) {
        return Ok(());
    }
    bail!(
        "failed to mount {} (see {})",
        config.music_dir.display(),
        config.rclone_log.display()
    )
}

pub fn unmount(config: &Config) -> Result<()> {
    // Local playback and queued library tracks can keep FUSE handles open.
    // URL-only playback does not need the mount, so leave mpv running then.
    if uses_library(config) {
        let _ = mpv::quit(config, true);
    }

    if is_mounted(&config.music_dir) {
        platform::unmount(&config.music_dir)?;
        println!("unmounted {}", config.music_dir.display());
    } else {
        println!("not mounted: {}", config.music_dir.display());
    }

    // Some FUSE backends return before rclone notices. Stop our own process.
    if let Some(pid) = read_pid(&config.rclone_pid)
        && platform::pid_alive(pid)
        && pid_is_our_rclone(pid, config)
    {
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }

    let _ = std::fs::remove_file(&config.rclone_pid);
    Ok(())
}

fn read_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| text.trim().parse().ok())
}

fn pid_alive(pid: u32) -> bool {
    platform::pid_alive(pid)
}

fn pid_is_our_rclone(pid: u32, config: &Config) -> bool {
    platform::process_command(pid)
        .map(|text| {
            text.contains("rclone")
                && text.contains("mount")
                && text.contains(&config.music_dir.display().to_string())
        })
        .unwrap_or(false)
}

/// Whether mpv is playing, or has queued, anything from the music mount.
fn uses_library(config: &Config) -> bool {
    let Ok(mut client) = mpv::Mpv::connect(&config.socket) else {
        return false;
    };
    if let Ok(status) = client.status()
        && is_library_path(config, &status.path)
    {
        return true;
    }
    client
        .playlist()
        .map(|entries| {
            entries
                .iter()
                .any(|entry| is_library_path(config, &entry.filename))
        })
        .unwrap_or(false)
}

fn is_library_path(config: &Config, path: &str) -> bool {
    if path.is_empty() || crate::queue::is_url(path) {
        return false;
    }
    let candidate = Path::new(path);
    if candidate.starts_with(&config.music_dir) {
        return true;
    }
    // Playlist entries can be canonical paths while the configured mount is
    // not (or vice versa), so compare canonical forms as a fallback.
    let Ok(candidate) = std::fs::canonicalize(candidate) else {
        return false;
    };
    std::fs::canonicalize(&config.music_dir)
        .map(|root| candidate.starts_with(root))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_local_library_paths_only() {
        let base = crate::test_support::unique_base("mount-paths");
        let music = base.join("Music");
        std::fs::create_dir_all(&music).unwrap();
        let config = crate::test_support::test_config(music.clone(), &base);

        let track = music.join("Artist/song.flac");
        std::fs::create_dir_all(track.parent().unwrap()).unwrap();
        std::fs::write(&track, b"test").unwrap();
        assert!(is_library_path(&config, &track.display().to_string()));
        assert!(is_library_path(
            &config,
            &music.join("./Artist/song.flac").display().to_string()
        ));
        assert!(!is_library_path(&config, "/tmp/outside.flac"));
        assert!(!is_library_path(
            &config,
            "https://stream-relay-geo.ntslive.net/stream"
        ));
        assert!(!is_library_path(&config, ""));

        let _ = std::fs::remove_dir_all(&base);
    }
}
