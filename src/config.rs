use anyhow::{Context, Result};
use serde::Deserialize;
use std::env;
use std::path::{Path, PathBuf};

/// Runtime configuration, derived from ~/.config/music/config.toml plus
/// MUSIC_DIR / MUSIC_REMOTE environment overrides.
#[derive(Debug, Clone)]
pub struct Config {
    pub music_dir: PathBuf,
    pub remote: String,
    pub runtime_dir: PathBuf,
    pub socket: PathBuf,
    pub cache_dir: PathBuf,
    pub rclone_log: PathBuf,
    pub mpv_log: PathBuf,
    pub rclone_pid: PathBuf,
    pub mpv_pid: PathBuf,
    pub mpv_extra_args: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    music_dir: Option<String>,
    remote: Option<String>,
    #[serde(default)]
    mpv_extra_args: Vec<String>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let home = env::var_os("HOME")
            .map(PathBuf::from)
            .context("HOME is not set")?;
        let config_home = env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"));
        let config_path = config_home.join("music").join("config.toml");

        let file = if config_path.is_file() {
            let text = std::fs::read_to_string(&config_path)
                .with_context(|| format!("reading {}", config_path.display()))?;
            toml::from_str::<FileConfig>(&text)
                .with_context(|| format!("parsing {}", config_path.display()))?
        } else {
            FileConfig::default()
        };

        let music_dir = env::var("MUSIC_DIR")
            .ok()
            .or(file.music_dir)
            .map(|s| expand_tilde(&s, &home))
            .unwrap_or_else(|| home.join("Music"));

        let remote = env::var("MUSIC_REMOTE")
            .ok()
            .or(file.remote)
            .unwrap_or_else(|| "sam:/Volumes/NASH_SYNC/music/library".to_string());

        let runtime_base = env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"));
        let uid = unsafe { libc::getuid() };
        let runtime_dir = runtime_base.join(format!("music-mount-{uid}"));

        let cache_base = env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cache"));
        let cache_dir = cache_base.join("music");

        Ok(Self {
            music_dir,
            remote,
            socket: runtime_dir.join("mpv.sock"),
            rclone_log: cache_dir.join("rclone.log"),
            mpv_log: cache_dir.join("mpv.log"),
            rclone_pid: cache_dir.join("rclone.pid"),
            mpv_pid: cache_dir.join("mpv.pid"),
            runtime_dir,
            cache_dir,
            mpv_extra_args: file.mpv_extra_args,
        })
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        std::fs::create_dir_all(&self.runtime_dir)
            .with_context(|| format!("creating {}", self.runtime_dir.display()))?;
        std::fs::create_dir_all(&self.cache_dir)
            .with_context(|| format!("creating {}", self.cache_dir.display()))?;
        Ok(())
    }
}

fn expand_tilde(value: &str, home: &Path) -> PathBuf {
    if value == "~" {
        home.to_path_buf()
    } else if let Some(rest) = value.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(value)
    }
}
