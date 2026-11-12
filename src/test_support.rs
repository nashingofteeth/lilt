#![allow(dead_code)]

use std::env;
use std::path::{Path, PathBuf};

use crate::config::Config;

/// Small deterministic xorshift PRNG so fuzz tests need no extra crates.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn below(&mut self, limit: usize) -> usize {
        if limit == 0 {
            0
        } else {
            (self.next_u64() % limit as u64) as usize
        }
    }

    pub fn pick<'a, T>(&mut self, values: &'a [T]) -> &'a T {
        &values[self.below(values.len())]
    }

    pub fn boolean(&mut self) -> bool {
        self.next_u64() & 1 == 0
    }

    pub fn string(&mut self, max_len: usize) -> String {
        const POOL: &[char] = &[
            'a', 'b', 'c', 'z', '0', '1', '9', ' ', '\t', '\n', '\0', '-', '_', '.', '/', '\\',
            '~', ':', '%', 'é', '日', '🎵', '\'', '"', '$', '{', '}', '#', '?', '*', '[', ']',
        ];
        let length = self.below(max_len + 1);
        (0..length).map(|_| *self.pick(POOL)).collect()
    }

    pub fn bytes(&mut self, max_len: usize) -> Vec<u8> {
        let length = self.below(max_len + 1);
        (0..length)
            .map(|_| (self.next_u64() & 0xff) as u8)
            .collect()
    }

    pub fn f64_bits(&mut self) -> f64 {
        f64::from_bits(self.next_u64())
    }
}

pub fn cases(default: usize) -> usize {
    env::var("FUZZ_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

pub fn unique_base(label: &str) -> PathBuf {
    // macOS TMPDIR points under /var/folders/..., which is long enough that
    // narrow TUI tests truncate the path before it can be asserted on.
    let root = if Path::new("/tmp").is_dir() {
        PathBuf::from("/tmp")
    } else {
        env::temp_dir()
    };
    let base = root.join(format!(
        "lilt-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    std::fs::create_dir_all(&base).unwrap();
    base
}

pub fn test_config(music_dir: PathBuf, base: &std::path::Path) -> Config {
    Config {
        music_dir,
        remote: "test:".to_string(),
        runtime_dir: base.join("rt"),
        socket: base.join("rt/mpv.sock"),
        cache_dir: base.join("cache"),
        rclone_log: base.join("cache/rclone.log"),
        mpv_log: base.join("cache/mpv.log"),
        capture_log: base.join("cache/capture.log"),
        rclone_pid: base.join("cache/rclone.pid"),
        mpv_pid: base.join("cache/mpv.pid"),
        mpv_extra_args: Vec::new(),
        capture_device: None,
    }
}
