use anyhow::{Context, Result, bail};
use std::process::Command;

use crate::config::Config;

pub fn show(config: &Config, follow: bool) -> Result<()> {
    let files: Vec<&std::path::Path> = [&config.rclone_log, &config.mpv_log, &config.capture_log]
        .into_iter()
        .filter(|path| path.exists())
        .map(|path| path.as_path())
        .collect();

    if files.is_empty() {
        bail!("no logs yet in {}", config.cache_dir.display());
    }

    let mut command = Command::new("tail");
    command.args(["-n", "100"]);
    if follow {
        command.arg("-F");
    }
    for file in files {
        command.arg(file);
    }

    let status = command.status().context("running tail")?;
    if !status.success() {
        bail!("tail exited with {status}");
    }
    Ok(())
}
