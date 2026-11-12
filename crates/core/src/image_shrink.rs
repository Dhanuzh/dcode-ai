//! Images sized for the model before they're sent. Claude bills an image by
//! its pixel area and scales anything bigger than ~1568px on the long edge
//! (or ~1.15 megapixels) down itself — so sending a full-size screenshot
//! costs upload time and tokens for nothing. Images are scaled to those
//! limits once, and the result is cached per file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use image::{GenericImageView, ImageFormat, imageops::FilterType};

/// Longest edge sent.
pub const MAX_EDGE: u32 = 1568;
/// Most pixels sent.
pub const MAX_PIXELS: u64 = 1_150_000;
/// A PNG still bigger than this after scaling goes as JPEG.
const MAX_PNG_BYTES: usize = 1_500_000;

/// The size to scale `(w, h)` to, or `None` when it already fits.
pub fn target_size(w: u32, h: u32) -> Option<(u32, u32)> {
    if w == 0 || h == 0 {
        return None;
    }
    let edge = f64::from(MAX_EDGE) / f64::from(w.max(h));
    let area = (MAX_PIXELS as f64 / (u64::from(w) * u64::from(h)) as f64).sqrt();
    let s = edge.min(area);
    if s >= 1.0 {
        return None;
    }
    Some((
        ((f64::from(w) * s).floor() as u32).max(1),
        ((f64::from(h) * s).floor() as u32).max(1),
    ))
}

/// `bytes` (of `media_type`) fitted for the model: the same bytes when they
/// already fit or can't be read as an image; else scaled, as PNG — or JPEG
/// when the PNG is still large.
pub fn shrink(bytes: Vec<u8>, media_type: &str) -> (Vec<u8>, String) {
    let Ok(img) = image::load_from_memory(&bytes) else {
        return (bytes, media_type.to_string());
    };
    let (w, h) = img.dimensions();
    let Some((nw, nh)) = target_size(w, h) else {
        return (bytes, media_type.to_string());
    };
    let small = img.resize_exact(nw, nh, FilterType::Triangle);
    let mut png = std::io::Cursor::new(Vec::new());
    if small.write_to(&mut png, ImageFormat::Png).is_ok() && png.get_ref().len() <= MAX_PNG_BYTES {
        return (png.into_inner(), "image/png".into());
    }
    let mut jpg = std::io::Cursor::new(Vec::new());
    let rgb = image::DynamicImage::ImageRgb8(small.to_rgb8());
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpg, 85);
    if enc.encode_image(&rgb).is_ok() {
        return (jpg.into_inner(), "image/jpeg".into());
    }
    (bytes, media_type.to_string())
}

type Cache = HashMap<PathBuf, (Option<SystemTime>, Vec<u8>, String)>;
static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

/// The file's bytes as the model should get them (see [`shrink`]); scaled
/// once per file version, then served from memory.
pub fn read_for_model(path: &Path, media_type: &str) -> std::io::Result<(Vec<u8>, String)> {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    if let Ok(cache) = CACHE.lock()
        && let Some((m, bytes, media)) = cache.as_ref().and_then(|c| c.get(path))
        && *m == modified
    {
        return Ok((bytes.clone(), media.clone()));
    }
    let raw = std::fs::read(path)?;
    let (bytes, media) = shrink(raw, media_type);
    if let Ok(mut cache) = CACHE.lock() {
        let c = cache.get_or_insert_with(HashMap::new);
        if c.len() > 64 {
            c.clear();
        }
        c.insert(path.to_path_buf(), (modified, bytes.clone(), media.clone()));
    }
    Ok((bytes, media))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut out, ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn sizes_fit_the_model() {
        assert_eq!(target_size(800, 600), None);
        // A 4K screenshot: the long edge and the area both limit it.
        let (w, h) = target_size(3840, 2160).unwrap();
        assert!(
            w <= MAX_EDGE && u64::from(w) * u64::from(h) <= MAX_PIXELS,
            "{w}x{h}"
        );
        assert!((f64::from(w) / f64::from(h) - 3840.0 / 2160.0).abs() < 0.01);
        // A tall page.
        let (w, h) = target_size(1000, 6000).unwrap();
        assert!(h <= MAX_EDGE && w >= 1, "{w}x{h}");
    }

    #[test]
    fn big_images_shrink_small_ones_pass_through() {
        let small = png(400, 300);
        let (out, media) = shrink(small.clone(), "image/png");
        assert_eq!(out, small);
        assert_eq!(media, "image/png");

        let (out, _) = shrink(png(3000, 2000), "image/png");
        let img = image::load_from_memory(&out).unwrap();
        let (w, h) = img.dimensions();
        assert!(
            w <= MAX_EDGE && u64::from(w) * u64::from(h) <= MAX_PIXELS,
            "{w}x{h}"
        );

        // Not an image: left alone.
        let (out, media) = shrink(b"not an image".to_vec(), "image/png");
        assert_eq!(
            (out.as_slice(), media.as_str()),
            (&b"not an image"[..], "image/png")
        );
    }

    #[test]
    fn reads_are_cached_per_file_version() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("shot.png");
        std::fs::write(&p, png(2400, 1600)).unwrap();
        let (a, _) = read_for_model(&p, "image/png").unwrap();
        let (b, _) = read_for_model(&p, "image/png").unwrap();
        assert_eq!(a, b);
        let (w, _) = image::load_from_memory(&a).unwrap().dimensions();
        assert!(w <= MAX_EDGE);
    }
}
