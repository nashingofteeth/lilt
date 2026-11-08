use image::DynamicImage;
use lofty::file::TaggedFileExt;
use lofty::probe::Probe;
use std::path::{Path, PathBuf};

/// Keep decoded art small enough to encode quickly while still looking sharp
/// on high-DPI terminal graphics protocols.
const MAX_DIMENSION: u32 = 1200;

const FOLDER_NAMES: [&str; 6] = ["cover", "folder", "front", "album", "artwork", "art"];
const IMAGE_EXTENSIONS: [&str; 4] = ["jpg", "jpeg", "png", "webp"];

/// Load cover art for a track: prefer art embedded in the audio file, then a
/// conventional image next to it.
pub fn load(dir: &Path, track: &Path) -> Option<DynamicImage> {
    let bytes = embedded_picture(track).or_else(|| folder_picture(dir))?;
    let image = image::load_from_memory(&bytes).ok()?;
    Some(limit_size(image))
}

fn embedded_picture(track: &Path) -> Option<Vec<u8>> {
    let tagged = Probe::open(track).ok()?.read().ok()?;
    tagged.tags().iter().find_map(|tag| {
        tag.pictures()
            .first()
            .map(|picture| picture.data().to_vec())
    })
}

fn folder_picture(dir: &Path) -> Option<Vec<u8>> {
    let path = folder_picture_path(dir)?;
    std::fs::read(path).ok()
}

fn folder_picture_path(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?.to_str()?.to_ascii_lowercase();
            let (stem, extension) = name.rsplit_once('.')?;
            let name_rank = FOLDER_NAMES
                .iter()
                .position(|candidate| *candidate == stem)?;
            let extension_rank = IMAGE_EXTENSIONS
                .iter()
                .position(|candidate| *candidate == extension)?;
            Some((name_rank * IMAGE_EXTENSIONS.len() + extension_rank, path))
        })
        .min_by_key(|(rank, _)| *rank)
        .map(|(_, path)| path)
}

fn limit_size(image: DynamicImage) -> DynamicImage {
    if image.width() > MAX_DIMENSION || image.height() > MAX_DIMENSION {
        image.thumbnail(MAX_DIMENSION, MAX_DIMENSION)
    } else {
        image
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::unique_base;

    #[test]
    fn finds_folder_art_case_insensitively() {
        let base = unique_base("cover");
        std::fs::create_dir_all(&base).unwrap();
        let image = image::RgbImage::from_pixel(8, 8, image::Rgb([10, 20, 30]));
        image
            .save(base.join("FRONT.PNG"))
            .expect("write test cover");

        let track = base.join("song.flac");
        let cover = load(&base, &track).expect("folder cover");
        assert_eq!((cover.width(), cover.height()), (8, 8));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn prefers_cover_name_and_jpeg_extension() {
        let base = unique_base("cover-priority");
        std::fs::create_dir_all(&base).unwrap();
        let red = image::RgbImage::from_pixel(4, 4, image::Rgb([255, 0, 0]));
        let green = image::RgbImage::from_pixel(4, 4, image::Rgb([0, 255, 0]));
        red.save(base.join("front.jpg")).unwrap();
        green.save(base.join("cover.png")).unwrap();

        let path = folder_picture_path(&base).expect("cover path");
        assert_eq!(path.file_name().unwrap(), "cover.png");

        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn returns_none_without_art() {
        let base = unique_base("cover-none");
        std::fs::create_dir_all(&base).unwrap();
        assert!(load(&base, &base.join("song.flac")).is_none());
        let _ = std::fs::remove_dir_all(&base);
    }
}
