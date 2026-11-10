//! Platform-specific process and mount helpers.
//!
//! Linux relies on util-linux (`mountpoint`, `fusermount3`) and procfs.
//! macOS uses `statfs`, `umount`/`diskutil`, and `ps`.

use anyhow::{Context, Result, bail};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Whether a PID is still running.
///
/// `kill(pid, 0)` also reports `EPERM` for processes owned by another user,
/// which still means the process exists.
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    if unsafe { libc::kill(pid as libc::pid_t, 0) } == 0 {
        return true;
    }
    matches!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::EPERM)
    )
}

/// The command line of a running process, with arguments separated by spaces.
pub fn process_command(pid: u32) -> Option<String> {
    if !pid_alive(pid) {
        return None;
    }

    #[cfg(target_os = "linux")]
    {
        let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        Some(String::from_utf8_lossy(&bytes).replace('\0', " "))
    }

    #[cfg(target_os = "macos")]
    {
        let output = Command::new("ps")
            .args(["-p", &pid.to_string(), "-o", "command="])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if text.is_empty() { None } else { Some(text) }
    }
}

#[cfg(target_os = "linux")]
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

#[cfg(target_os = "macos")]
pub fn is_mounted(dir: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;

    let path = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let mut stats: libc::statfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statfs(c_path.as_ptr(), &mut stats) } != 0 {
        return false;
    }
    // `f_mntonname` is the root of the mounted filesystem that contains the
    // path. It only equals `dir` when `dir` itself is the mount point.
    let mount_point: Vec<u8> = stats
        .f_mntonname
        .iter()
        .map(|byte| *byte as u8)
        .take_while(|byte| *byte != 0)
        .collect();
    mount_point == path.as_os_str().as_bytes()
}

#[cfg(target_os = "linux")]
pub fn unmount(dir: &Path) -> Result<()> {
    for _ in 0..10 {
        if !is_mounted(dir) {
            return Ok(());
        }
        let done = Command::new("fusermount3")
            .arg("-u")
            .arg(dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if done {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    println!("music: normal unmount failed, trying lazy unmount");
    let status = Command::new("fusermount3")
        .arg("-uz")
        .arg(dir)
        .status()
        .context("running fusermount3 -uz")?;
    if !status.success() {
        bail!("failed to unmount {}", dir.display());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
pub fn unmount(dir: &Path) -> Result<()> {
    for _ in 0..10 {
        if !is_mounted(dir) {
            return Ok(());
        }
        let done = Command::new("umount")
            .arg(dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        if done {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(200));
    }

    println!("music: normal unmount failed, trying force unmount");
    let status = Command::new("diskutil")
        .args(["unmount", "force"])
        .arg(dir)
        .status()
        .context("running diskutil unmount force")?;
    if !status.success() {
        bail!("failed to unmount {}", dir.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_is_alive() {
        assert!(pid_alive(std::process::id()));
    }

    #[test]
    fn impossible_pids_are_not_alive() {
        assert!(!pid_alive(0));
        assert!(!pid_alive(i32::MAX as u32));
    }

    #[test]
    fn current_process_command_is_visible() {
        let command = process_command(std::process::id()).expect("own command line");
        assert!(!command.is_empty());
    }

    #[test]
    fn plain_directory_is_not_a_mount_point() {
        let base = crate::test_support::unique_base("platform");
        std::fs::create_dir_all(&base).unwrap();
        assert!(!is_mounted(&base));
        assert!(unmount(&base).is_ok());
        let _ = std::fs::remove_dir_all(&base);
    }
}
