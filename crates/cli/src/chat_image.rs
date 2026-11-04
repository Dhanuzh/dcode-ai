//! Pictures in team chat: shrunk and encoded before sending, decoded to show
//! and to open. People only — the agent never sees them.

use base64::Engine as _;
use dcode_ai_common::event::{ChatImage, MAX_CHAT_IMAGE_BASE64};
use image::{DynamicImage, GenericImageView, ImageFormat, imageops::FilterType};
use std::io::Cursor;
use std::path::{Path, PathBuf};

/// Longest side sent.
const MAX_SIDE: u32 = 1280;

fn encode(img: &DynamicImage, format: ImageFormat) -> Result<Vec<u8>, String> {
    let mut out = Cursor::new(Vec::new());
    match format {
        ImageFormat::Jpeg => {
            let rgb = DynamicImage::ImageRgb8(img.to_rgb8());
            let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 80);
            enc.encode_image(&rgb).map_err(|e| e.to_string())?;
        }
        _ => img
            .write_to(&mut out, ImageFormat::Png)
            .map_err(|e| e.to_string())?,
    }
    Ok(out.into_inner())
}

/// A picture ready to send: at most [`MAX_SIDE`] pixels on its longest
/// side, PNG when that's small enough, else JPEG, shrunk again if needed.
pub fn prepare(bytes: &[u8], name: &str) -> Result<ChatImage, String> {
    let img =
        image::load_from_memory(bytes).map_err(|e| format!("not a picture I can read: {e}"))?;
    let mut img = shrink(img, MAX_SIDE);
    let b64 = base64::engine::general_purpose::STANDARD;
    for _ in 0..4 {
        let png = encode(&img, ImageFormat::Png)?;
        let (data, mime) = if b64.encode(&png).len() <= MAX_CHAT_IMAGE_BASE64 {
            (png, "image/png")
        } else {
            (encode(&img, ImageFormat::Jpeg)?, "image/jpeg")
        };
        let data = b64.encode(&data);
        if data.len() <= MAX_CHAT_IMAGE_BASE64 {
            let (w, h) = img.dimensions();
            let stem = Path::new(name)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "picture".into());
            let ext = if mime == "image/png" { "png" } else { "jpg" };
            return Ok(ChatImage {
                name: format!("{}.{ext}", stem.chars().take(60).collect::<String>()),
                mime: mime.into(),
                w,
                h,
                data,
            });
        }
        let (w, h) = img.dimensions();
        img = shrink(img, (w.max(h) * 3 / 4).max(64));
    }
    Err("the picture is too big to send".into())
}

fn shrink(img: DynamicImage, max_side: u32) -> DynamicImage {
    let (w, h) = img.dimensions();
    if w.max(h) <= max_side {
        return img;
    }
    img.resize(max_side, max_side, FilterType::Triangle)
}

/// The picture's bytes.
pub fn bytes(img: &ChatImage) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD
        .decode(&img.data)
        .ok()
}

/// Written to a temporary file (to open in the system viewer).
pub fn to_temp_file(img: &ChatImage) -> std::io::Result<PathBuf> {
    use sha2::{Digest, Sha256};
    let data = bytes(img).ok_or_else(|| std::io::Error::other("unreadable picture"))?;
    let hash: String = Sha256::digest(&data)
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect();
    let ext = if img.mime == "image/png" {
        "png"
    } else {
        "jpg"
    };
    let dir = std::env::temp_dir().join("dcode-ai-chat");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{hash}.{ext}"));
    if !path.exists() {
        std::fs::write(&path, data)?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_big_screenshot_is_shrunk_and_round_trips() {
        let big = DynamicImage::ImageRgb8(image::RgbImage::from_fn(2560, 1440, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        }));
        let png = encode(&big, ImageFormat::Png).unwrap();
        let img = prepare(&png, "Screenshot 2026.png").unwrap();
        assert_eq!((img.w, img.h), (1280, 720));
        assert!(img.is_acceptable());
        assert!(img.name.starts_with("Screenshot 2026."));
        let back = image::load_from_memory(&bytes(&img).unwrap()).unwrap();
        assert_eq!(back.dimensions(), (1280, 720));
        let path = to_temp_file(&img).unwrap();
        assert!(path.exists());
        assert!(prepare(b"not a picture", "x.png").is_err());
    }
}
