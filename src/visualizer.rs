#[cfg(target_os = "linux")]
use std::io::Read;
use std::process::Child;
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
#[cfg(target_os = "linux")]
use std::thread;
use std::thread::JoinHandle;

#[cfg(any(target_os = "linux", test))]
const SAMPLE_RATE: usize = 48_000;
#[cfg(target_os = "linux")]
const CHANNELS: usize = 2;
#[cfg(any(target_os = "linux", test))]
const FFT_SIZE: usize = 2_048;
const BAND_COUNT: usize = 64;
#[cfg(any(target_os = "linux", test))]
const MIN_FREQ: f32 = 30.0;
#[cfg(any(target_os = "linux", test))]
const MAX_FREQ: f32 = 16_000.0;

/// Captures the system output monitor with parec and publishes log-spaced
/// spectrum bars. All work happens off the UI thread.
pub struct Visualizer {
    bands: Arc<Mutex<Vec<f32>>>,
    available: Arc<AtomicBool>,
    child: Option<Child>,
    reader: Option<JoinHandle<()>>,
}

impl Visualizer {
    /// macOS has no PulseAudio monitor source out of the box, so the pane
    /// reports itself unavailable instead of spawning a backend.
    #[cfg(not(target_os = "linux"))]
    pub fn start() -> Self {
        Self {
            bands: Arc::new(Mutex::new(vec![0.0; BAND_COUNT])),
            available: Arc::new(AtomicBool::new(false)),
            child: None,
            reader: None,
        }
    }

    #[cfg(target_os = "linux")]
    pub fn start() -> Self {
        let bands = Arc::new(Mutex::new(vec![0.0; BAND_COUNT]));
        let available = Arc::new(AtomicBool::new(false));

        let child = Command::new("parec")
            .args([
                "--device=@DEFAULT_MONITOR@",
                "--format=float32le",
                "--rate=48000",
                "--channels=2",
                "--latency-msec=20",
                "--raw",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();

        let mut child = match child {
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

#[cfg(target_os = "linux")]
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

#[cfg(any(target_os = "linux", test))]
fn hann_window(size: usize) -> Vec<f32> {
    (0..size)
        .map(|index| {
            let x = std::f32::consts::TAU * index as f32 / (size - 1) as f32;
            0.5 - 0.5 * x.cos()
        })
        .collect()
}

/// Pure spectrum analysis, kept separate so it can be tested without audio.
#[cfg(any(target_os = "linux", test))]
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

#[cfg(any(target_os = "linux", test))]
fn band_edge(band: usize) -> f32 {
    let ratio = band as f32 / BAND_COUNT as f32;
    MIN_FREQ * (MAX_FREQ / MIN_FREQ).powf(ratio)
}

#[cfg(any(target_os = "linux", test))]
fn frequency_to_bin(frequency: f32) -> usize {
    ((frequency * FFT_SIZE as f32 / SAMPLE_RATE as f32).round() as usize).max(1)
}

#[derive(Clone, Copy, Default)]
#[cfg(any(target_os = "linux", test))]
struct Complex {
    re: f32,
    im: f32,
}

#[cfg(any(target_os = "linux", test))]
impl Complex {
    fn mul(self, other: Self) -> Self {
        Self {
            re: self.re * other.re - self.im * other.im,
            im: self.re * other.im + self.im * other.re,
        }
    }
}

#[cfg(any(target_os = "linux", test))]
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
}
