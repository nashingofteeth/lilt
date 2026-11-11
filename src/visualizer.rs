use std::io::Read;
#[cfg(target_os = "macos")]
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use crate::config::Config;

const SAMPLE_RATE: usize = 48_000;
const CHANNELS: usize = 2;
const FFT_SIZE: usize = 2_048;
const BAND_COUNT: usize = 64;
const MIN_FREQ: f32 = 30.0;
const MAX_FREQ: f32 = 16_000.0;

/// Captures the music player's output (PulseAudio sink input on Linux, an
/// audia-tap process tap on macOS) and publishes log-spaced spectrum bars.
/// All work happens off the UI thread.
pub struct Visualizer {
    bands: Arc<Mutex<Vec<f32>>>,
    available: Arc<AtomicBool>,
    child: Option<Child>,
    reader: Option<JoinHandle<()>>,
}

impl Visualizer {
    pub fn start(config: &Config) -> Self {
        let bands = Arc::new(Mutex::new(vec![0.0; BAND_COUNT]));
        let available = Arc::new(AtomicBool::new(false));

        let Some(mut command) = capture_command(config) else {
            return Self {
                bands,
                available,
                child: None,
                reader: None,
            };
        };

        let mut child = match command
            .stdout(Stdio::piped())
            .stderr(capture_stderr(config))
            .spawn()
        {
            Ok(child) => child,
            Err(_) => {
                return Self {
                    bands,
                    available,
                    child: None,
                    reader: None,
                };
            }
        };

        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Self {
                bands,
                available,
                child: None,
                reader: None,
            };
        };

        available.store(true, Ordering::Relaxed);
        let reader_bands = Arc::clone(&bands);
        let reader_available = Arc::clone(&available);
        let reader = thread::spawn(move || {
            read_loop(stdout, reader_bands, reader_available);
        });

        Self {
            bands,
            available,
            child: Some(child),
            reader: Some(reader),
        }
    }

    pub fn bands(&self) -> Vec<f32> {
        self.bands
            .lock()
            .map(|bands| bands.clone())
            .unwrap_or_else(|_| vec![0.0; BAND_COUNT])
    }

    pub fn available(&self) -> bool {
        self.available.load(Ordering::Relaxed)
    }
}

fn capture_stderr(config: &Config) -> Stdio {
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&config.capture_log)
    {
        Ok(file) => Stdio::from(file),
        Err(_) => Stdio::null(),
    }
}

#[cfg(target_os = "linux")]
fn capture_command(config: &Config) -> Option<Command> {
    let mut command = Command::new("parec");
    if let Some(device) = config.capture_device.as_deref() {
        // Explicit override, e.g. a monitor source for system-wide capture.
        command.arg(format!("--device={device}"));
    } else {
        command.arg(format!("--monitor-stream={}", mpv_sink_input(config)?));
    }
    command.args([
        "--format=float32le",
        "--rate=48000",
        "--channels=2",
        "--latency-msec=20",
        "--raw",
    ]);
    Some(command)
}

#[cfg(target_os = "linux")]
fn mpv_sink_input(config: &Config) -> Option<u32> {
    let pid = crate::mpv::daemon_pid(config);
    let output = Command::new("pactl")
        .args(["list", "sink-inputs"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    parse_sink_input_index(
        &String::from_utf8_lossy(&output.stdout),
        pid,
        crate::mpv::AUDIO_CLIENT_NAME,
    )
}

/// Find the mpv PulseAudio sink-input in `pactl list sink-inputs` output.
/// Prefer the daemon PID; fall back to mpv's unique audio client name because
/// PipeWire does not always expose `application.process.id`.
#[cfg(any(target_os = "linux", test))]
fn parse_sink_input_index(listing: &str, pid: Option<u32>, client_name: &str) -> Option<u32> {
    let wanted_pid = pid.map(|pid| pid.to_string());
    let mut name_match = None;

    for block in listing.split("Sink Input #").skip(1) {
        let mut lines = block.lines();
        let Some(index) = lines
            .next()
            .and_then(|line| line.split_whitespace().next())
            .and_then(|value| value.parse().ok())
        else {
            continue;
        };

        let mut matches_pid = false;
        let mut matches_name = false;
        for line in lines {
            let Some((key, value)) = line.trim().split_once('=') else {
                continue;
            };
            let value = value.trim().trim_matches('"');
            match key.trim() {
                "application.process.id" => {
                    matches_pid = wanted_pid.as_deref() == Some(value);
                }
                "application.name" | "application.id" | "node.name" => {
                    matches_name |= value == client_name;
                }
                _ => {}
            }
        }

        if matches_pid {
            return Some(index);
        }
        if matches_name && name_match.is_none() {
            name_match = Some(index);
        }
    }

    name_match
}

#[cfg(target_os = "macos")]
fn capture_command(config: &Config) -> Option<Command> {
    // An explicit capture_device opts into the ffmpeg/BlackHole fallback.
    if let Some(device) = config.capture_device.as_deref() {
        return blackhole_command(device);
    }
    audia_tap_command(crate::mpv::daemon_pid(config)?)
}

#[cfg(target_os = "macos")]
fn audia_tap_command(pid: u32) -> Option<Command> {
    let mut command = Command::new("audia-tap");
    command.args([
        "--pid",
        &pid.to_string(),
        "--format",
        "f32",
        "--sample-rate",
        "48000",
        "--channels",
        "2",
    ]);
    command.stdin(Stdio::null());
    // The CLI looks for audia-tap.app in the working directory to start its
    // permission agent. Resolve the symlink and run from the app's parent.
    if let Some(directory) = audia_tap_app_dir() {
        command.current_dir(directory);
    }
    Some(command)
}

#[cfg(target_os = "macos")]
fn audia_tap_app_dir() -> Option<PathBuf> {
    let output = Command::new("which").arg("audia-tap").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let path = path.canonicalize().ok()?;
    let app = path.parent()?.parent()?.parent()?;
    if app.extension().is_some_and(|extension| extension == "app") {
        app.parent().map(Path::to_path_buf)
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn blackhole_command(device: &str) -> Option<Command> {
    let index = audio_device_index(&ffmpeg_device_list()?, Some(device))?;
    let mut command = Command::new("ffmpeg");
    command.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-f",
        "avfoundation",
    ]);
    command.arg("-i").arg(format!(":{index}"));
    command.args(["-ac", "2", "-ar", "48000", "-f", "f32le", "-"]);
    command.stdin(Stdio::null());
    Some(command)
}

#[cfg(target_os = "macos")]
fn ffmpeg_device_list() -> Option<String> {
    let output = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-f",
            "avfoundation",
            "-list_devices",
            "true",
            "-i",
            "",
        ])
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&output.stderr).into_owned())
}

/// Find the AVFoundation audio index for the requested device. `None` picks
/// the first BlackHole device; a query may be a device name or an index.
#[cfg(any(target_os = "macos", test))]
fn audio_device_index(listing: &str, query: Option<&str>) -> Option<usize> {
    let audio = listing.split("AVFoundation audio devices:").nth(1)?;
    for line in audio.lines() {
        let Some((index, name)) = parse_device_line(line) else {
            continue;
        };
        match query {
            Some(query) => {
                if query.parse::<usize>().ok() == Some(index) || name.eq_ignore_ascii_case(query) {
                    return Some(index);
                }
            }
            None => {
                if name.to_ascii_lowercase().contains("blackhole") {
                    return Some(index);
                }
            }
        }
    }
    None
}

#[cfg(any(target_os = "macos", test))]
fn parse_device_line(line: &str) -> Option<(usize, &str)> {
    // "[AVFoundation indev @ 0x...] [1] BlackHole 2ch"
    let open = line.rfind('[')?;
    let close = line[open..].find(']')? + open;
    let index = line[open + 1..close].trim().parse().ok()?;
    let name = line[close + 1..].trim();
    (!name.is_empty()).then_some((index, name))
}

impl Drop for Visualizer {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn read_loop(
    mut stdout: std::process::ChildStdout,
    bands: Arc<Mutex<Vec<f32>>>,
    available: Arc<AtomicBool>,
) {
    let frame_bytes = FFT_SIZE * CHANNELS * std::mem::size_of::<f32>();
    let mut raw = vec![0u8; frame_bytes];
    let window = hann_window(FFT_SIZE);
    let mut smoothed = vec![0.0f32; BAND_COUNT];

    loop {
        if stdout.read_exact(&mut raw).is_err() {
            break;
        }

        let mut mono = vec![0.0f32; FFT_SIZE];
        for (index, slot) in mono.iter_mut().enumerate() {
            let offset = index * CHANNELS * 4;
            let left = f32::from_le_bytes([
                raw[offset],
                raw[offset + 1],
                raw[offset + 2],
                raw[offset + 3],
            ]);
            let right = f32::from_le_bytes([
                raw[offset + 4],
                raw[offset + 5],
                raw[offset + 6],
                raw[offset + 7],
            ]);
            *slot = (left + right) * 0.5;
        }

        let values = analyze(&mono, &window, &mut smoothed);
        if let Ok(mut current) = bands.lock() {
            *current = values;
        }
    }

    available.store(false, Ordering::Relaxed);
    if let Ok(mut current) = bands.lock() {
        *current = vec![0.0; BAND_COUNT];
    }
}

fn hann_window(size: usize) -> Vec<f32> {
    (0..size)
        .map(|index| {
            let x = std::f32::consts::TAU * index as f32 / (size - 1) as f32;
            0.5 - 0.5 * x.cos()
        })
        .collect()
}

/// Pure spectrum analysis, kept separate so it can be tested without audio.
fn analyze(samples: &[f32], window: &[f32], smoothed: &mut [f32]) -> Vec<f32> {
    let mut buffer: Vec<Complex> = samples
        .iter()
        .zip(window)
        .map(|(sample, window)| Complex {
            re: sample * window,
            im: 0.0,
        })
        .collect();

    fft(&mut buffer);

    let bin_count = FFT_SIZE / 2;
    let mut magnitudes = vec![0.0f32; bin_count];
    for (index, slot) in magnitudes.iter_mut().enumerate() {
        let value = buffer[index + 1];
        *slot = (value.re * value.re + value.im * value.im).sqrt() / FFT_SIZE as f32;
    }

    for (band, slot) in smoothed.iter_mut().enumerate() {
        let low = band_edge(band);
        let high = band_edge(band + 1);
        let start = frequency_to_bin(low).max(1);
        let end = frequency_to_bin(high).max(start + 1).min(bin_count);
        let peak = magnitudes[start..end]
            .iter()
            .copied()
            .fold(0.0f32, f32::max);
        let db = 20.0 * (peak + 1e-9).log10();
        let value = ((db + 70.0) / 70.0).clamp(0.0, 1.0);
        *slot = if value > *slot { value } else { *slot * 0.82 };
    }

    smoothed.to_vec()
}

fn band_edge(band: usize) -> f32 {
    let ratio = band as f32 / BAND_COUNT as f32;
    MIN_FREQ * (MAX_FREQ / MIN_FREQ).powf(ratio)
}

fn frequency_to_bin(frequency: f32) -> usize {
    ((frequency * FFT_SIZE as f32 / SAMPLE_RATE as f32).round() as usize).max(1)
}

#[derive(Clone, Copy, Default)]
struct Complex {
    re: f32,
    im: f32,
}

impl Complex {
    fn mul(self, other: Self) -> Self {
        Self {
            re: self.re * other.re - self.im * other.im,
            im: self.re * other.im + self.im * other.re,
        }
    }
}

fn fft(buffer: &mut [Complex]) {
    let size = buffer.len();
    if size <= 1 {
        return;
    }

    // Bit-reversal permutation.
    let mut j = 0usize;
    for i in 1..size {
        let mut bit = size >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            buffer.swap(i, j);
        }
    }

    let mut length = 2usize;
    while length <= size {
        let angle = -std::f32::consts::TAU / length as f32;
        let step = Complex {
            re: angle.cos(),
            im: angle.sin(),
        };
        for start in (0..size).step_by(length) {
            let mut factor = Complex { re: 1.0, im: 0.0 };
            for offset in 0..length / 2 {
                let left = buffer[start + offset];
                let right = buffer[start + offset + length / 2].mul(factor);
                buffer[start + offset] = Complex {
                    re: left.re + right.re,
                    im: left.im + right.im,
                };
                buffer[start + offset + length / 2] = Complex {
                    re: left.re - right.re,
                    im: left.im - right.im,
                };
                factor = factor.mul(step);
            }
        }
        length <<= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::Rng;

    fn sine(frequency: f32) -> Vec<f32> {
        (0..FFT_SIZE)
            .map(|index| {
                (std::f32::consts::TAU * frequency * index as f32 / SAMPLE_RATE as f32).sin()
            })
            .collect()
    }

    fn smooth(bands: Vec<f32>) -> Vec<f32> {
        let window = hann_window(FFT_SIZE);
        let mut smoothed = vec![0.0f32; BAND_COUNT];
        analyze(&bands, &window, &mut smoothed)
    }

    #[test]
    fn silence_produces_zero_bands() {
        let bands = smooth(vec![0.0; FFT_SIZE]);
        assert!(bands.iter().all(|value| *value < 1e-6));
    }

    #[test]
    fn sine_wave_peaks_in_expected_band() {
        let bands = smooth(sine(440.0));
        let peak = bands
            .iter()
            .enumerate()
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap())
            .map(|(index, _)| index)
            .unwrap();
        let expected =
            ((440.0f32 / MIN_FREQ).ln() / (MAX_FREQ / MIN_FREQ).ln() * BAND_COUNT as f32) as usize;
        assert!(
            peak.abs_diff(expected) <= 2,
            "peak {peak}, expected {expected}"
        );
    }

    #[test]
    fn random_pcm_stays_finite() {
        let mut rng = Rng::new(0x5EED);
        let window = hann_window(FFT_SIZE);
        let mut smoothed = vec![0.0f32; BAND_COUNT];
        for _ in 0..200 {
            let samples: Vec<f32> = (0..FFT_SIZE)
                .map(|_| (rng.next_u64() as f32 / u64::MAX as f32) * 2.0 - 1.0)
                .collect();
            let bands = analyze(&samples, &window, &mut smoothed);
            assert!(bands.iter().all(|value| value.is_finite()));
            assert!(bands.iter().all(|value| (0.0..=1.0).contains(value)));
        }
    }

    const SINK_INPUTS: &str = "\
Sink Input #41
\tProperties:
\t\tapplication.name = \"mpv\"
\t\tapplication.process.id = \"1234\"
Sink Input #42
\tProperties:
\t\tapplication.name = \"firefox\"
\t\tapplication.process.id = \"999\"
";

    #[test]
    fn finds_sink_input_for_process() {
        assert_eq!(
            parse_sink_input_index(SINK_INPUTS, Some(1234), "music"),
            Some(41)
        );
        assert_eq!(
            parse_sink_input_index(SINK_INPUTS, Some(999), "music"),
            Some(42)
        );
        assert_eq!(parse_sink_input_index(SINK_INPUTS, Some(1), "music"), None);
    }

    #[test]
    fn accepts_unquoted_sink_input_pid() {
        let listing = "Sink Input #7\napplication.process.id = 55\n";
        assert_eq!(parse_sink_input_index(listing, Some(55), "music"), Some(7));
    }

    #[test]
    fn falls_back_to_audio_client_name() {
        let listing = "\
Sink Input #9
\tProperties:
\t\tapplication.name = \"music\"
\t\tapplication.id = \"music\"
";
        assert_eq!(parse_sink_input_index(listing, None, "music"), Some(9));
        assert_eq!(
            parse_sink_input_index(listing, Some(1234), "music"),
            Some(9)
        );
        assert_eq!(parse_sink_input_index(listing, None, "mpv"), None);
    }

    #[test]
    fn process_id_takes_priority_over_name() {
        let listing = "\
Sink Input #8
\tProperties:
\t\tapplication.name = \"music\"
Sink Input #9
\tProperties:
\t\tapplication.name = \"music\"
\t\tapplication.process.id = \"1234\"
";
        assert_eq!(
            parse_sink_input_index(listing, Some(1234), "music"),
            Some(9)
        );
    }

    const DEVICE_LIST: &str = "\
[AVFoundation indev @ 0x1] AVFoundation video devices:
[AVFoundation indev @ 0x1] [0] FaceTime HD Camera
[AVFoundation indev @ 0x1] [1] BlackHole Video
[AVFoundation indev @ 0x1] AVFoundation audio devices:
[AVFoundation indev @ 0x1] [0] iPhone Microphone
[AVFoundation indev @ 0x1] [1] BlackHole 2ch
[AVFoundation indev @ 0x1] [2] MacBook Pro Microphone
[in#0 @ 0x2] Error opening input: Input/output error
";

    #[test]
    fn finds_blackhole_in_audio_devices() {
        assert_eq!(audio_device_index(DEVICE_LIST, None), Some(1));
    }

    #[test]
    fn matches_configured_device_name_case_insensitively() {
        assert_eq!(
            audio_device_index(DEVICE_LIST, Some("macbook pro microphone")),
            Some(2)
        );
    }

    #[test]
    fn configured_device_can_be_an_index() {
        assert_eq!(audio_device_index(DEVICE_LIST, Some("0")), Some(0));
    }

    #[test]
    fn unknown_device_is_not_found() {
        assert_eq!(audio_device_index(DEVICE_LIST, Some("USB Interface")), None);
    }

    #[test]
    fn video_section_is_ignored() {
        let listing = "\
[AVFoundation indev @ 0x1] AVFoundation video devices:
[AVFoundation indev @ 0x1] [0] BlackHole Camera
[AVFoundation indev @ 0x1] AVFoundation audio devices:
[AVFoundation indev @ 0x1] [0] MacBook Pro Microphone
";
        assert_eq!(audio_device_index(listing, None), None);
    }

    #[test]
    fn missing_audio_section_is_not_found() {
        assert_eq!(
            audio_device_index("AVFoundation video devices:", None),
            None
        );
    }
}
