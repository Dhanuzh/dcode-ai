//! A brand kit applied to every video: logo, colours, font, intro and outro
//! text. `.dcode-ai/brand.json` in the project, else `~/.dcode-ai/brand.json`:
//!
//! ```json
//! { "name": "Acme", "logo": "logo.png", "logo_corner": "top-right",
//!   "colors": { "from": "#4c348c", "to": "#182252", "accent": "#ffd166" },
//!   "font": "fonts/Brand-SemiBold.ttf",
//!   "intro": "Acme product tours", "outro": "Questions? help.acme.com" }
//! ```
//!
//! Paths are relative to the file's folder.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};

use image::RgbaImage;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Colors {
    /// Background gradient, top-left colour (`#rrggbb`).
    #[serde(default)]
    pub from: String,
    /// …and bottom-right.
    #[serde(default)]
    pub to: String,
    /// Captions' spoken words, highlights.
    #[serde(default)]
    pub accent: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Brand {
    #[serde(default)]
    pub name: String,
    /// A PNG (transparency kept) shown small in a corner of the footage and
    /// above the title.
    #[serde(default)]
    pub logo: String,
    /// `top-right` (default), `top-left`, `bottom-right`, `bottom-left`.
    #[serde(default)]
    pub logo_corner: String,
    #[serde(default)]
    pub colors: Colors,
    /// A .ttf/.otf for titles and captions.
    #[serde(default)]
    pub font: String,
    /// The title card's line under the title when the video has none.
    #[serde(default)]
    pub intro: String,
    /// The end card's text when the video has none.
    #[serde(default)]
    pub outro: String,
    /// Folder the paths are relative to.
    #[serde(skip)]
    pub base: PathBuf,
}

/// Parses `#rrggbb`.
pub fn hex(s: &str) -> Option<[u8; 3]> {
    let h = s.trim().strip_prefix('#')?;
    if h.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

impl Brand {
    fn path(&self, p: &str) -> PathBuf {
        let p = Path::new(p);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.base.join(p)
        }
    }

    pub fn gradient(&self) -> Option<([u8; 3], [u8; 3])> {
        Some((hex(&self.colors.from)?, hex(&self.colors.to)?))
    }

    pub fn accent(&self) -> Option<[u8; 3]> {
        hex(&self.colors.accent)
    }
}

/// Where the brand kit for a recording folder comes from: the project's
/// `.dcode-ai/brand.json` (the recording is in `.dcode-ai/videos/<id>`), else
/// the user's.
pub fn file_for(recording: &Path) -> Option<PathBuf> {
    let project = recording
        .parent()
        .and_then(Path::parent)
        .map(|d| d.join("brand.json"))
        .filter(|p| p.is_file());
    project.or_else(|| {
        dcode_ai_common::config::dcode_ai_home_dir()
            .map(|d| d.join("brand.json"))
            .filter(|p| p.is_file())
    })
}

pub fn load(file: &Path) -> Result<Brand, String> {
    let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    let mut b: Brand =
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", file.display()))?;
    b.base = file.parent().map(Path::to_path_buf).unwrap_or_default();
    Ok(b)
}

/// What the brand kit is missing or got wrong (empty: fine).
pub fn check(b: &Brand) -> Vec<String> {
    let mut v = Vec::new();
    for (k, c) in [
        ("colors.from", &b.colors.from),
        ("colors.to", &b.colors.to),
        ("colors.accent", &b.colors.accent),
    ] {
        if !c.is_empty() && hex(c).is_none() {
            v.push(format!("{k} should be #rrggbb, not {c}"));
        }
    }
    if !b.logo.is_empty() && image::open(b.path(&b.logo)).is_err() {
        v.push(format!("logo {} can't be read", b.path(&b.logo).display()));
    }
    if !b.font.is_empty()
        && std::fs::read(b.path(&b.font))
            .ok()
            .and_then(|d| ab_glyph::FontVec::try_from_vec(d).ok())
            .is_none()
    {
        v.push(format!("font {} can't be read", b.path(&b.font).display()));
    }
    v
}

/// The brand of the render in progress (renders run one at a time per
/// process; frames read it in parallel).
static CURRENT: RwLock<Option<Brand>> = RwLock::new(None);
/// Font files read once, kept for the life of the process.
static FONTS: Mutex<Option<HashMap<PathBuf, &'static [u8]>>> = Mutex::new(None);
/// The logo, by height.
static LOGOS: Mutex<Option<HashMap<u32, RgbaImage>>> = Mutex::new(None);

/// Sets the brand for the next render (`None`: the default look).
pub fn set(b: Option<Brand>) {
    if let Ok(mut l) = LOGOS.lock() {
        *l = None;
    }
    if let Ok(mut c) = CURRENT.write() {
        *c = b;
    }
}

pub fn current() -> Option<Brand> {
    CURRENT.read().ok().and_then(|c| c.clone())
}

/// The brand font's bytes, if it has one that loads.
pub fn font_bytes() -> Option<&'static [u8]> {
    let b = current()?;
    if b.font.is_empty() {
        return None;
    }
    let p = b.path(&b.font);
    let mut g = FONTS.lock().ok()?;
    let map = g.get_or_insert_with(HashMap::new);
    if let Some(bytes) = map.get(&p) {
        return Some(bytes);
    }
    let data = std::fs::read(&p).ok()?;
    ab_glyph::FontRef::try_from_slice(&data).ok()?;
    let leaked: &'static [u8] = Box::leak(data.into_boxed_slice());
    map.insert(p, leaked);
    Some(leaked)
}

/// The logo scaled to `h` pixels high.
pub fn logo(h: u32) -> Option<RgbaImage> {
    let b = current()?;
    if b.logo.is_empty() || h == 0 {
        return None;
    }
    let mut g = LOGOS.lock().ok()?;
    let map = g.get_or_insert_with(HashMap::new);
    if let Some(l) = map.get(&h) {
        return Some(l.clone());
    }
    let pic = image::open(b.path(&b.logo)).ok()?.to_rgba8();
    let w = (pic.width() as f64 * h as f64 / pic.height().max(1) as f64).round() as u32;
    let small = image::imageops::resize(&pic, w.max(1), h, image::imageops::FilterType::Lanczos3);
    map.insert(h, small.clone());
    Some(small)
}

/// Puts the logo in its corner of `img` (`size`: height as a share of the
/// frame's).
pub fn stamp(img: &mut image::RgbImage, size: f64, opacity: f32) {
    let (w, h) = img.dimensions();
    let lh = ((h as f64 * size) as u32).max(8);
    let Some(l) = logo(lh) else {
        return;
    };
    let corner = current().map(|b| b.logo_corner).unwrap_or_default();
    let m = (h as f64 * 0.03) as i64;
    let (lw, lh) = (l.width() as i64, l.height() as i64);
    let (x0, y0) = match corner.as_str() {
        "top-left" => (m, m),
        "bottom-left" => (m, h as i64 - lh - m),
        "bottom-right" => (w as i64 - lw - m, h as i64 - lh - m),
        _ => (w as i64 - lw - m, m),
    };
    overlay(img, &l, x0, y0, opacity);
}

/// Alpha-blends `l` onto `img` at (`x0`, `y0`).
pub fn overlay(img: &mut image::RgbImage, l: &RgbaImage, x0: i64, y0: i64, opacity: f32) {
    let (w, h) = img.dimensions();
    for (x, y, p) in l.enumerate_pixels() {
        let (tx, ty) = (x0 + x as i64, y0 + y as i64);
        if tx < 0 || ty < 0 || tx >= w as i64 || ty >= h as i64 {
            continue;
        }
        let a = p.0[3] as f32 / 255.0 * opacity;
        if a <= 0.0 {
            continue;
        }
        let d = img.get_pixel_mut(tx as u32, ty as u32);
        for c in 0..3 {
            d.0[c] = (d.0[c] as f32 * (1.0 - a) + p.0[c] as f32 * a) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_brand_file_loads_and_is_checked() {
        let d = tempfile::tempdir().unwrap();
        let mut logo = RgbaImage::new(40, 20);
        for p in logo.pixels_mut() {
            *p = image::Rgba([255, 0, 0, 255]);
        }
        logo.save(d.path().join("logo.png")).unwrap();
        let f = d.path().join("brand.json");
        std::fs::write(
            &f,
            r##"{"name":"Acme","logo":"logo.png","colors":{"from":"#102030","to":"#000000","accent":"orange"}}"##,
        )
        .unwrap();
        let b = load(&f).unwrap();
        assert_eq!(b.gradient(), Some(([16, 32, 48], [0, 0, 0])));
        let problems = check(&b);
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].contains("accent"));
        // The logo goes in the top-right corner.
        set(Some(b));
        let mut img = image::RgbImage::new(400, 200);
        stamp(&mut img, 0.1, 1.0);
        set(None);
        assert_eq!(img.get_pixel(400 - 6 - 20, 6 + 10).0, [255, 0, 0]);
        assert_eq!(img.get_pixel(10, 190).0, [0, 0, 0]);
    }

    #[test]
    fn a_project_brand_wins() {
        let d = tempfile::tempdir().unwrap();
        let rec = d.path().join(".dcode-ai/videos/rec-1");
        std::fs::create_dir_all(&rec).unwrap();
        std::fs::write(d.path().join(".dcode-ai/brand.json"), "{}").unwrap();
        assert_eq!(file_for(&rec), Some(d.path().join(".dcode-ai/brand.json")));
    }
}
