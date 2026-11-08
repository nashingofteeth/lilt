use anyhow::{Context, Result, bail};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::config::Config;
use crate::mpv;

pub fn is_mounted(dir: &Path) -> bool {
    Command::new("mountpoint")
        .arg("-q")
        .arg(dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn rclone_args(config: &Config) -> Vec<std::ffi::OsString> {
    vec![
        "mount".into(),
        config.remote.clone().into(),
        config.music_dir.clone().into_os_string(),
        "--read-only".into(),
        "--dir-cache-time=10m".into(),
    ]
}

pub fn mount_foreground(config: &Config) -> Result<()> {
    if is_mounted(&config.music_dir) {
        println!("already mounted at {}", config.music_dir.display());
        return Ok(());
    }
    std::fs::create_dir_all(&config.music_dir)
        .with_context(|| format!("creating {}", config.music_dir.display()))?;

    let mut command = Command::new("rclone");
    command.args(rclone_args(config));
    let err = command.exec();
    Err(anyhow::Error::new(err).context(format!(
        "starting rclone mount of {} at {}",
        config.remote,
        config.music_dir.display()
    )))
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
    // Quit mpv first: prefetched/demuxer handles can keep the FUSE mount busy.
    if mpv::alive(config) {
        let _ = mpv::quit(config, true);
    }

    if is_mounted(&config.music_dir) {
        let mut ok = false;
        for _ in 0..10 {
            if !is_mounted(&config.music_dir) {
                ok = true;
                break;
            }
            let status = Command::new("fusermount3")
                .arg("-u")
                .arg(&config.music_dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
            if status.map(|s| s.success()).unwrap_or(false) {
                ok = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(200));
        }

        if !ok {
            println!("music: normal unmount failed, trying lazy unmount");
            let status = Command::new("fusermount3")
                .arg("-uz")
                .arg(&config.music_dir)
                .status()
                .context("running fusermount3 -uz")?;
            if !status.success() {
                bail!("failed to unmount {}", config.music_dir.display());
            }
        }
        println!("unmounted {}", config.music_dir.display());
    } else {
        println!("not mounted: {}", config.music_dir.display());
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
    Path::new("/proc").join(pid.to_string()).exists()
}

fn pid_is_our_rclone(pid: u32, config: &Config) -> bool {
    let Ok(bytes) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
        return false;
    };
    let text = String::from_utf8_lossy(&bytes).replace('\0', " ");
    text.contains("rclone")
        && text.contains("mount")
        && text.contains(&config.music_dir.display().to_string())
}
