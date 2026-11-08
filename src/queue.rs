use anyhow::{Context, Result, bail};
use std::io::Write;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::config::Config;

pub const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "m4a", "aac", "ogg", "opus", "wav", "aif", "aiff", "alac", "wma", "ape", "wv",
    "dsf", "dff",
];

pub fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| {
            let ext = ext.to_ascii_lowercase();
            AUDIO_EXTENSIONS.iter().any(|known| *known == ext)
        })
        .unwrap_or(false)
}

/// Build an m3u from explicit files and directories (audio allowlist only).
/// Returns the playlist path and the number of entries.
pub fn build_playlist(config: &Config, paths: &[PathBuf]) -> Result<(PathBuf, usize)> {
    config.ensure_dirs()?;
    let path = config.runtime_dir.join(format!(
        "queue-{}-{}.m3u",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    let mut playlist = std::fs::File::create(&path)
        .with_context(|| format!("creating playlist {}", path.display()))?;
    writeln!(playlist, "#EXTM3U")?;

    let mut count = 0usize;
    for input in paths {
        if input.is_dir() {
            let mut files: Vec<PathBuf> = WalkDir::new(input)
                .follow_links(false)
                .into_iter()
                .filter_entry(|entry| {
                    entry.depth() == 0 || !entry.file_name().to_string_lossy().starts_with('.')
                })
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_file() && is_audio(entry.path()))
                .map(|entry| entry.into_path())
                .collect();
            files.sort();
            for audio in files {
                writeln!(playlist, "{}", audio.display())?;
                count += 1;
            }
        } else if input.is_file() {
            writeln!(playlist, "{}", input.display())?;
            count += 1;
        }
    }

    if count == 0 {
        let _ = std::fs::remove_file(&path);
        bail!("no audio files found in the selected path(s)");
    }
    Ok((path, count))
}

/// Resolve a CLI path: absolute/relative to cwd, relative to the music dir,
/// or a case-insensitive match under the music dir.
pub fn resolve_path(config: &Config, input: &str) -> Result<PathBuf> {
    let direct = Path::new(input);
    if direct.exists() {
        return Ok(direct
            .canonicalize()
            .unwrap_or_else(|_| direct.to_path_buf()));
    }

    let joined = config.music_dir.join(input);
    if joined.exists() {
        return Ok(joined.canonicalize().unwrap_or(joined));
    }

    let wanted = input.trim_matches('/').to_lowercase();
    for entry in WalkDir::new(&config.music_dir)
        .max_depth(4)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        let Ok(relative) = entry.path().strip_prefix(&config.music_dir) else {
            continue;
        };
        if relative.to_string_lossy().to_lowercase() == wanted {
            return Ok(entry.into_path());
        }
    }

    bail!(
        "no such path: {} (looked in {})",
        input,
        config.music_dir.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_audio_extensions_case_insensitively() {
        assert!(is_audio(Path::new("track.flac")));
        assert!(is_audio(Path::new("track.FLAC")));
        assert!(is_audio(Path::new("track.wav")));
        assert!(!is_audio(Path::new("cover.jpg")));
        assert!(!is_audio(Path::new("album.nfo")));
        assert!(!is_audio(Path::new("playlist.m3u")));
    }
}

#[cfg(test)]
mod fuzz_tests {
    use super::*;
    use crate::test_support::{Rng, cases, test_config, unique_base};
    use std::fs;
    use std::path::Path;

    const NAMES: &[&str] = &[
        "track", "Track", "01 song", "._apple", ".hidden", "é", "日", "a b", "x", "",
    ];
    const EXTS: &[&str] = &[
        "flac", "FLAC", "mp3", "wav", "jpg", "nfo", "cue", "m3u", "txt", "",
    ];

    #[test]
    fn fuzz_audio_paths_never_panic() {
        let mut rng = Rng::new(9);
        for _ in 0..cases(5_000) {
            let name = rng.string(40);
            let _ = is_audio(Path::new(&name));
        }
    }

    #[test]
    fn fuzz_build_playlist_random_tree() {
        let mut rng = Rng::new(42);
        let iterations = cases(100).min(500);

        for case in 0..iterations {
            let base = unique_base(&format!("queue-{case}"));
            let root = base.join("music");
            fs::create_dir_all(&root).unwrap();
            fs::write(root.join("valid.flac"), b"x").unwrap();

            for _ in 0..rng.below(15) {
                let name = *rng.pick(NAMES);
                let ext = *rng.pick(EXTS);
                let filename = if ext.is_empty() {
                    name.to_string()
                } else {
                    format!("{name}.{ext}")
                };
                if filename.is_empty() {
                    continue;
                }
                fs::write(root.join(&filename), b"x").unwrap();
            }

            for _ in 0..rng.below(3) {
                let dir = root.join(format!("sub{}", rng.below(100)));
                fs::create_dir_all(&dir).unwrap();
                fs::write(dir.join("nested.flac"), b"x").unwrap();
                fs::write(dir.join("cover.jpg"), b"x").unwrap();
            }

            let hidden = root.join(format!(".hidden-{case}"));
            fs::create_dir_all(&hidden).unwrap();
            fs::write(hidden.join("hidden.flac"), b"x").unwrap();

            let config = test_config(root.clone(), &base);
            let (playlist, count) = build_playlist(&config, std::slice::from_ref(&root))
                .expect("valid audio file should produce a playlist");
            assert!(count >= 1);

            let text = fs::read_to_string(&playlist).unwrap();
            for line in text.lines().skip(1) {
                let path = Path::new(line);
                assert!(is_audio(path), "non-audio entry {line}");
                let relative = path.strip_prefix(&root).unwrap_or(path);
                assert!(
                    !relative
                        .components()
                        .any(|component| component.as_os_str().to_string_lossy().starts_with('.')),
                    "hidden entry {line}"
                );
            }
            let _ = fs::remove_file(&playlist);

            for _ in 0..10 {
                let _ = resolve_path(&config, &rng.string(50));
            }

            let _ = fs::remove_dir_all(&base);
        }
    }
}
