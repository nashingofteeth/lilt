use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, limit: usize) -> usize {
        if limit == 0 {
            0
        } else {
            (self.next_u64() % limit as u64) as usize
        }
    }

    fn pick<'a, T>(&mut self, values: &'a [T]) -> &'a T {
        &values[self.below(values.len())]
    }
}

fn unique_base() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let base = std::env::temp_dir().join(format!("music-cli-fuzz-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&base).unwrap();
    base
}

#[test]
fn fuzz_safe_cli_execution_never_panics() {
    let cases = std::env::var("FUZZ_CASES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(200usize)
        .min(2_000);
    let exe = env!("CARGO_BIN_EXE_music");
    let base = unique_base();
    let music_dir = base.join("Music");
    let runtime = base.join("runtime");
    let cache = base.join("cache");
    let config = base.join("config");
    for dir in [&music_dir, &runtime, &cache, &config] {
        std::fs::create_dir_all(dir).unwrap();
    }
    std::fs::create_dir_all(config.join("music")).unwrap();
    std::fs::write(
        config.join("music/config.toml"),
        format!(
            "music_dir = {:?}\nremote = \"test:\"\n",
            music_dir.display().to_string()
        ),
    )
    .unwrap();

    // Commands that fail fast without starting mpv/rclone or touching a mount.
    let commands = [
        "status",
        "toggle",
        "pause",
        "resume",
        "next",
        "prev",
        "stop",
        "queue",
        "list",
        "mute",
        "jump",
        "first",
        "last",
        "volume",
        "seek",
        "shuffle",
        "repeat",
        "quit",
        "logs",
        "unmount",
        "restart",
        "help",
        "--help",
        "--version",
    ];
    let values = [
        "", "0", "-5", "+5", "50%", "NaN", "inf", "on", "off", "all", "one", "🎵", "a b", "--",
        "-", "...", "~", "\\", "..",
    ];

    let mut rng = Rng::new(0xF00D);
    for _ in 0..cases {
        let mut args = vec![rng.pick(&commands).to_string()];
        for _ in 0..rng.below(3) {
            args.push(rng.pick(&values).to_string());
        }

        let output = Command::new(exe)
            .args(&args)
            .env("MUSIC_DIR", &music_dir)
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("XDG_CACHE_HOME", &cache)
            .env("XDG_CONFIG_HOME", &config)
            .output()
            .expect("spawn music");

        assert_ne!(
            output.status.code(),
            Some(101),
            "music panicked for args {args:?}"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !stderr.contains("panicked"),
            "music panicked for args {args:?}:\n{stderr}"
        );
    }

    let _ = std::fs::remove_dir_all(&base);
}
