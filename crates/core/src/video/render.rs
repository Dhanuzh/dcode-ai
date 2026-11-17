//! Renders an edit list into finished videos, one per cut:
//! - `long` (16:9): the recording sped up where it waits, zoomed toward what
//!   it clicks, cursor and click ripples, cards, captions → `video.mp4`
//!   (captioned) and `video-clean.mp4` (for uploading captions separately),
//!   `video.gif`, `captions.srt/.vtt`, `chapters.txt`, `thumbnail.png`.
//! - `short` (9:16): the same edit framed around the action, captions clear
//!   of what the apps cover → `short.mp4`, `short.srt/.vtt`,
//!   `short-thumbnail.png`, `short-review.png` (the covered edges shaded).
//!
//! Narration (your takes or the draft voice) is laid over generated music
//! that ducks under it; the video holds where a line needs longer than the
//! action takes. MP4s need `ffmpeg`; the GIF is made in Rust.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use image::{RgbImage, imageops};

use super::record::Privacy;
use super::record::{Saved, load};
use super::timeline::{Moment, Timeline};
use super::{Edl, Event, FrameInfo, Insert, Line, voice};

pub const FPS: f64 = 30.0;
const GIF_WIDTH: u32 = 640;
const GIF_MAX_SECONDS: f64 = 60.0;
const RAMP: f64 = 0.6;
const AUDIO_RATE: u32 = 44_100;
/// Pause after a line before the next may start.
const LINE_GAP: f64 = 0.35;

static FONT_BYTES: &[u8] = include_bytes!("../../assets/fonts/Inter-SemiBold.ttf");

fn font() -> FontRef<'static> {
    FontRef::try_from_slice(FONT_BYTES).expect("bundled font")
}

/// Adwaita Mono (OFL), for code and terminal scenes.
static MONO_BYTES: &[u8] = include_bytes!("../../assets/fonts/AdwaitaMono-Regular.ttf");

fn mono() -> FontRef<'static> {
    FontRef::try_from_slice(MONO_BYTES).expect("bundled mono font")
}

/// A code excerpt or a terminal session: a window with its lines in a
/// monospace font (`$ ` lines of a terminal are commands, in colour).
fn code_card(w: u32, h: u32, f: &FontRef, ins: &Insert, fade: f32) -> RgbImage {
    let mut img = background(w, h);
    let m = mono();
    let (wf, hf) = (w as f32, h as f32);
    let terminal = ins.kind == "terminal";
    let lines: Vec<&str> = ins.items.iter().flat_map(|i| i.lines()).take(30).collect();
    let longest = lines
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(10)
        .max(30);
    // As large as fits: the longest line across, the lines down.
    let px = (wf * 0.86 / (longest as f32 * 0.6))
        .min(hf * 0.72 / (lines.len().max(1) as f32 * 1.35))
        .clamp(10.0, hf * 0.05);
    let (x0, y0) = (wf * 0.06, hf * 0.14);
    let (x1, y1) = (
        wf * 0.94,
        (y0 + px * 1.35 * lines.len() as f32 + px * 2.6).min(hf * 0.95),
    );
    for y in y0 as i64..y1 as i64 {
        for x in x0 as i64..x1 as i64 {
            blend(&mut img, x, y, [14, 12, 20], 0.92);
        }
    }
    // Window bar: three dots and the title.
    let bar = px * 1.6;
    for (k, c) in [[255u8, 95, 86], [255, 189, 46], [39, 201, 63]]
        .iter()
        .enumerate()
    {
        let (cx, cy, r) = (x0 + px * (1.0 + k as f32 * 1.2), y0 + bar / 2.0, px * 0.32);
        for yy in (cy - r) as i64..=(cy + r) as i64 {
            for xx in (cx - r) as i64..=(cx + r) as i64 {
                let d = ((xx as f32 - cx).powi(2) + (yy as f32 - cy).powi(2)).sqrt();
                if d <= r {
                    blend(&mut img, xx, yy, *c, 1.0);
                }
            }
        }
    }
    let title = if ins.title.trim().is_empty() {
        if terminal { "terminal" } else { "code" }
    } else {
        ins.title.trim()
    };
    let tpx = px * 0.8;
    draw_text(
        &mut img,
        f,
        tpx,
        (wf - text_width(f, tpx, title)) / 2.0,
        y0 + bar / 2.0 + tpx * 0.35,
        title,
        [170, 165, 190],
        1.0,
    );
    let mut y = y0 + bar + px * 1.1;
    for l in &lines {
        let (color, text) = match (terminal, l.strip_prefix("$ ")) {
            (true, Some(cmd)) => {
                draw_text(&mut img, &m, px, x0 + px, y, "$", [120, 220, 140], 1.0);
                ([235, 235, 245], cmd)
            }
            (true, None) => ([170, 170, 185], *l),
            _ => ([220, 222, 235], *l),
        };
        let x = if terminal && l.starts_with("$ ") {
            x0 + px * 2.2
        } else {
            x0 + px
        };
        draw_text(&mut img, &m, px, x, y, text, color, 1.0);
        y += px * 1.35;
        if y > y1 - px * 0.5 {
            break;
        }
    }
    fade_all(&mut img, fade);
    img
}

/// What came out.
#[derive(Debug)]
pub struct Rendered {
    /// The main MP4 (the long cut, else the short).
    pub mp4: Option<PathBuf>,
    pub gif: PathBuf,
    pub srt: PathBuf,
    /// Length of the main cut, seconds.
    pub seconds: f64,
    /// Things worth knowing (no ffmpeg, no draft voice, …), joined.
    pub note: Option<String>,
    /// Every file written.
    pub files: Vec<PathBuf>,
}

/// A caption on screen, in finished-video seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub from: f64,
    pub to: f64,
    pub text: String,
}

fn ease(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// Recording second for output second `ot` (the frame shown then).
pub fn source_time(edl: &Edl, ot: f64) -> f64 {
    frame_time(edl, &Timeline::new(edl, &[]), ot)
}

fn frame_time(edl: &Edl, tl: &Timeline, ot: f64) -> f64 {
    match tl.at(ot) {
        Moment::Frame(s) => s,
        Moment::Insert(i, _) => i.at,
        Moment::Title(_) => edl.segments.first().map(|s| s.from).unwrap_or(0.0),
        Moment::End(_) => edl.segments.last().map(|s| s.to).unwrap_or(0.0),
    }
}

/// Text size for a card of this shape.
fn card_px(w: u32, h: u32) -> f32 {
    (h as f32 * 0.075).min(w as f32 * 0.085)
}

fn background(w: u32, h: u32) -> RgbImage {
    let mut img = RgbImage::new(w, h);
    for y in 0..h {
        let k = y as f32 / h as f32;
        for x in 0..w {
            let d = x as f32 / w as f32 * 0.3;
            let c = [
                (24.0 + 40.0 * (k + d)) as u8,
                (22.0 + 18.0 * (k + d)) as u8,
                (34.0 + 60.0 * (k + d)) as u8,
            ];
            img.put_pixel(x, y, image::Rgb(c));
        }
    }
    img
}

fn fade_all(img: &mut RgbImage, fade: f32) {
    if fade < 1.0 {
        for p in img.pixels_mut() {
            for v in &mut p.0 {
                *v = (*v as f32 * fade.max(0.0)) as u8;
            }
        }
    }
}

/// Centered lines of text starting at `y` (a baseline); returns the next.
fn centered(img: &mut RgbImage, f: &FontRef, px: f32, y: f32, text: &str, c: [u8; 3]) -> f32 {
    let w = img.width() as f32;
    let mut y = y;
    for l in wrap(f, px, text, w * 0.84) {
        let x = (w - text_width(f, px, &l)) / 2.0;
        draw_text(img, f, px, x, y, &l, c, 1.0);
        y += px * 1.25;
    }
    y
}

/// A title or end card.
fn card(w: u32, h: u32, f: &FontRef, title: &str, sub: &str, fade: f32) -> RgbImage {
    let mut img = background(w, h);
    let px = card_px(w, h);
    let lines = wrap(f, px, title, w as f32 * 0.84).len().max(1);
    let total = lines as f32 * px * 1.25;
    let y = centered(
        &mut img,
        f,
        px,
        h as f32 / 2.0 - total / 2.0 + px,
        title,
        [245, 243, 255],
    );
    if !sub.trim().is_empty() {
        centered(&mut img, f, px * 0.5, y + px * 0.2, sub, [190, 180, 230]);
    }
    fade_all(&mut img, fade);
    img
}

/// A card placed into the video: `section`, `bullets` or `quote`.
fn insert_card(w: u32, h: u32, f: &FontRef, ins: &Insert, fade: f32) -> RgbImage {
    let px = card_px(w, h);
    match ins.kind.as_str() {
        "code" | "terminal" => code_card(w, h, f, ins, fade),
        "bullets" => {
            let mut img = background(w, h);
            let (wf, hf) = (w as f32, h as f32);
            let left = wf * 0.1;
            let mut y = hf * 0.18 + px;
            for l in wrap(f, px * 0.9, &ins.title, wf * 0.8) {
                draw_text(&mut img, f, px * 0.9, left, y, &l, [245, 243, 255], 1.0);
                y += px * 1.15;
            }
            y += px * 0.5;
            let bpx = px * 0.6;
            for item in ins.items.iter().take(7) {
                let lines = wrap(f, bpx, item, wf * 0.74);
                for (k, l) in lines.iter().enumerate() {
                    if k == 0 {
                        draw_text(&mut img, f, bpx, left, y, "•", [180, 140, 255], 1.0);
                    }
                    draw_text(
                        &mut img,
                        f,
                        bpx,
                        left + bpx * 1.2,
                        y,
                        l,
                        [230, 228, 245],
                        1.0,
                    );
                    y += bpx * 1.3;
                }
                y += bpx * 0.35;
                if y > hf * 0.92 {
                    break;
                }
            }
            fade_all(&mut img, fade);
            img
        }
        "quote" => {
            let text = format!("“{}”", ins.title.trim().trim_matches(['"', '“', '”']));
            let by = ins
                .items
                .first()
                .map(|b| format!("— {b}"))
                .unwrap_or_default();
            card(w, h, f, &text, &by, fade)
        }
        _ => card(
            w,
            h,
            f,
            &ins.title,
            ins.items.first().map(String::as_str).unwrap_or(""),
            fade,
        ),
    }
}

/// The caption box. Shorts keep it clear of the app's overlays: above the
/// bottom quarter and left of the right-hand buttons.
fn caption(img: &mut RgbImage, f: &FontRef, text: &str, portrait: bool) {
    let (w, h) = (img.width() as f32, img.height() as f32);
    let (px, max_w, center, bottom) = if portrait {
        ((w * 0.052).max(16.0), w * 0.68, w * 0.46, h * 0.72)
    } else {
        ((h * 0.042).max(16.0), w * 0.8, w / 2.0, h * 0.95)
    };
    let lines: Vec<String> = wrap(f, px, text, max_w).into_iter().take(3).collect();
    if lines.is_empty() {
        return;
    }
    let pad = px * 0.6;
    let tw = lines
        .iter()
        .map(|l| text_width(f, px, l))
        .fold(0.0, f32::max);
    let bh = lines.len() as f32 * px * 1.3 + pad;
    let (x0, y0) = (center - tw / 2.0 - pad, bottom - bh);
    for y in y0.max(0.0) as i64..(y0 + bh) as i64 {
        for x in x0.max(0.0) as i64..(x0 + tw + 2.0 * pad) as i64 {
            blend(img, x, y, [16, 14, 24], 0.72);
        }
    }
    let mut base = y0 + pad / 2.0 + px;
    for l in &lines {
        let x = center - text_width(f, px, l) / 2.0;
        draw_text(img, f, px, x, base, l, [255, 255, 255], 1.0);
        base += px * 1.3;
    }
}

/// Shades the edges a 9:16 player covers (top bar, bottom text and
/// buttons, right-hand buttons) — for checking a short by eye.
fn shade_covered(img: &mut RgbImage) {
    let (w, h) = (img.width() as i64, img.height() as i64);
    for y in 0..h {
        for x in 0..w {
            let covered = y < h / 10 || y > h * 3 / 4 || x > w * 85 / 100;
            if covered {
                blend(img, x, y, [220, 40, 60], 0.35);
            }
        }
    }
}

/// A thumbnail: a frame with the title large over a dark fade.
fn thumbnail(mut img: RgbImage, f: &FontRef, title: &str) -> RgbImage {
    let (w, h) = (img.width(), img.height());
    for y in (h * 45 / 100)..h {
        let a = (y - h * 45 / 100) as f32 / (h as f32 * 0.55);
        for x in 0..w {
            blend(&mut img, x as i64, y as i64, [12, 10, 20], a * 0.88);
        }
    }
    let px = (h as f32 * 0.09).min(w as f32 * 0.1);
    let lines: Vec<String> = wrap(f, px, title, w as f32 * 0.86)
        .into_iter()
        .take(3)
        .collect();
    let mut y = h as f32 * 0.93 - (lines.len() as f32 - 1.0) * px * 1.15;
    for l in &lines {
        draw_text(&mut img, f, px, w as f32 * 0.06, y, l, [255, 255, 255], 1.0);
        y += px * 1.15;
    }
    img
}

/// The zoom at second `s`: (scale, weight 0–1, target x, target y in CSS
/// pixels); the strongest zoom wins.
fn zoom_at(edl: &Edl, s: f64) -> Option<(f64, f64, f64, f64)> {
    edl.zooms
        .iter()
        .map(|z| {
            let w = if s < z.at - RAMP || s > z.at + z.hold + RAMP {
                0.0
            } else if s < z.at {
                ease((s - (z.at - RAMP)) / RAMP)
            } else if s <= z.at + z.hold {
                1.0
            } else {
                1.0 - ease((s - z.at - z.hold) / RAMP)
            };
            (1.0 + (z.scale - 1.0) * w, w, z.x, z.y)
        })
        .filter(|z| z.1 > 0.0)
        .max_by(|a, b| a.1.total_cmp(&b.1))
}

/// The cursor (CSS pixels) at second `s`: still between actions, gliding
/// to the next one in the last moment before it.
fn cursor_at(points: &[(f64, f64, f64)], s: f64) -> Option<(f64, f64)> {
    let first = points.first()?;
    let next = points.iter().position(|p| p.0 >= s);
    match next {
        None => points.last().map(|p| (p.1, p.2)),
        Some(0) => Some((first.1, first.2)),
        Some(j) => {
            let (a, b) = (points[j - 1], points[j]);
            let start = (b.0 - 0.6).max(a.0);
            if s <= start {
                Some((a.1, a.2))
            } else {
                let k = ease((s - start) / (b.0 - start).max(0.01));
                Some((a.1 + (b.1 - a.1) * k, a.2 + (b.2 - a.2) * k))
            }
        }
    }
}

fn blend(img: &mut RgbImage, x: i64, y: i64, c: [u8; 3], a: f32) {
    if x < 0 || y < 0 || x >= img.width() as i64 || y >= img.height() as i64 || a <= 0.0 {
        return;
    }
    let p = img.get_pixel_mut(x as u32, y as u32);
    let a = a.clamp(0.0, 1.0);
    for (v, c) in p.0.iter_mut().zip(c) {
        *v = (*v as f32 * (1.0 - a) + c as f32 * a) as u8;
    }
}

fn text_width(f: &FontRef, px: f32, s: &str) -> f32 {
    let sf = f.as_scaled(PxScale::from(px));
    let mut w = 0.0;
    let mut prev = None;
    for ch in s.chars() {
        let id = sf.glyph_id(ch);
        if let Some(p) = prev {
            w += sf.kern(p, id);
        }
        w += sf.h_advance(id);
        prev = Some(id);
    }
    w
}

#[allow(clippy::too_many_arguments)]
fn draw_text(
    img: &mut RgbImage,
    f: &FontRef,
    px: f32,
    x: f32,
    baseline: f32,
    s: &str,
    c: [u8; 3],
    alpha: f32,
) {
    let sf = f.as_scaled(PxScale::from(px));
    let mut pen = x;
    let mut prev = None;
    for ch in s.chars() {
        let id = sf.glyph_id(ch);
        if let Some(p) = prev {
            pen += sf.kern(p, id);
        }
        let g = id.with_scale_and_position(PxScale::from(px), ab_glyph::point(pen, baseline));
        if let Some(o) = f.outline_glyph(g) {
            let b = o.px_bounds();
            o.draw(|gx, gy, cov| {
                blend(
                    img,
                    b.min.x as i64 + gx as i64,
                    b.min.y as i64 + gy as i64,
                    c,
                    cov * alpha,
                );
            });
        }
        pen += sf.h_advance(id);
        prev = Some(id);
    }
}

fn wrap(f: &FontRef, px: f32, s: &str, max: f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for w in s.split_whitespace() {
        let next = if cur.is_empty() {
            w.to_string()
        } else {
            format!("{cur} {w}")
        };
        if text_width(f, px, &next) > max && !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
            cur = w.to_string();
        } else {
            cur = next;
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// The arrow cursor at (x, y), `size` ≈ its height in pixels.
fn cursor(img: &mut RgbImage, x: f64, y: f64, size: f64) {
    const ARROW: [(f64, f64); 7] = [
        (0.0, 0.0),
        (0.0, 17.0),
        (4.0, 13.0),
        (7.0, 20.0),
        (9.5, 19.0),
        (6.5, 12.0),
        (12.0, 12.0),
    ];
    let k = size / 20.0;
    let pts: Vec<(f64, f64)> = ARROW.iter().map(|(a, b)| (x + a * k, y + b * k)).collect();
    let inside = |px: f64, py: f64| {
        let mut c = false;
        let mut j = pts.len() - 1;
        for i in 0..pts.len() {
            let (xi, yi) = pts[i];
            let (xj, yj) = pts[j];
            if (yi > py) != (yj > py) && px < (xj - xi) * (py - yi) / (yj - yi) + xi {
                c = !c;
            }
            j = i;
        }
        c
    };
    for py in (y - 2.0) as i64..(y + 21.0 * k) as i64 {
        for px in (x - 2.0) as i64..(x + 13.0 * k) as i64 {
            let (fx, fy) = (px as f64 + 0.5, py as f64 + 0.5);
            if inside(fx, fy) {
                let edge = [(-1.5, 0.0), (1.5, 0.0), (0.0, -1.5), (0.0, 1.5)]
                    .iter()
                    .any(|(dx, dy)| !inside(fx + dx, fy + dy));
                blend(
                    img,
                    px,
                    py,
                    if edge { [20, 20, 20] } else { [255, 255, 255] },
                    1.0,
                );
            }
        }
    }
}

fn ripple(img: &mut RgbImage, x: f64, y: f64, p: f64, max_r: f64) {
    let r = 6.0 + max_r * p;
    let a = ((1.0 - p) * 0.85) as f32;
    for py in (y - r - 3.0) as i64..(y + r + 3.0) as i64 {
        for px in (x - r - 3.0) as i64..(x + r + 3.0) as i64 {
            let d = ((px as f64 - x).powi(2) + (py as f64 - y).powi(2)).sqrt();
            let on = 1.0 - ((d - r).abs() / 3.0);
            if on > 0.0 {
                blend(img, px, py, [180, 140, 255], a * on as f32);
            }
        }
    }
}

fn srt_time(t: f64) -> String {
    let ms = (t.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02},{:03}",
        ms / 3_600_000,
        ms / 60_000 % 60,
        ms / 1000 % 60,
        ms % 1000
    )
}

fn vtt_time(t: f64) -> String {
    srt_time(t).replace(',', ".")
}

/// Captions as SRT.
pub fn srt_of(cues: &[Cue]) -> String {
    let mut out = String::new();
    for (n, c) in cues.iter().enumerate() {
        out.push_str(&format!(
            "{}\n{} --> {}\n{}\n\n",
            n + 1,
            srt_time(c.from),
            srt_time(c.to),
            c.text.trim()
        ));
    }
    out
}

/// Captions as WebVTT.
pub fn vtt_of(cues: &[Cue]) -> String {
    let mut out = String::from("WEBVTT\n\n");
    for c in cues {
        out.push_str(&format!(
            "{} --> {}\n{}\n\n",
            vtt_time(c.from),
            vtt_time(c.to),
            c.text.trim()
        ));
    }
    out
}

/// The edit list's own captions (no narration), in finished-video time.
pub fn caption_cues(edl: &Edl, tl: &Timeline) -> Vec<Cue> {
    let mut out = Vec::new();
    for c in &edl.captions {
        let a = (0..=20)
            .map(|i| c.from + (c.to - c.from) * i as f64 / 20.0)
            .find_map(|t| tl.output_time(t));
        let b = (0..=20)
            .rev()
            .map(|i| c.from + (c.to - c.from) * i as f64 / 20.0)
            .find_map(|t| tl.output_time(t));
        if let (Some(a), Some(b)) = (a, b)
            && b > a
        {
            out.push(Cue {
                from: a,
                to: b,
                text: c.text.trim().to_string(),
            });
        }
    }
    out
}

/// Captions as SRT, in finished-video time (the edit list's captions).
pub fn srt(edl: &Edl) -> String {
    srt_of(&caption_cues(edl, &Timeline::new(edl, &[])))
}

/// A spoken line split into caption-sized pieces (≤ 10 words, broken at
/// punctuation when it can), timed by their length.
fn line_cues(text: &str, from: f64, to: f64) -> Vec<Cue> {
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut chunks: Vec<String> = Vec::new();
    let mut cur: Vec<&str> = Vec::new();
    for w in words {
        cur.push(w);
        let ends = w.ends_with(['.', ',', ';', ':', '!', '?']);
        if cur.len() >= 10 || (ends && cur.len() >= 4) {
            chunks.push(cur.join(" "));
            cur.clear();
        }
    }
    if !cur.is_empty() {
        if cur.len() < 3
            && let Some(last) = chunks.last_mut()
        {
            last.push(' ');
            last.push_str(&cur.join(" "));
        } else {
            chunks.push(cur.join(" "));
        }
    }
    let total: usize = chunks.iter().map(|c| c.len()).sum::<usize>().max(1);
    let mut t = from;
    chunks
        .into_iter()
        .map(|c| {
            let d = (to - from) * c.len() as f64 / total as f64;
            let cue = Cue {
                from: t,
                to: t + d,
                text: c,
            };
            t += d;
            cue
        })
        .collect()
}

/// A line's audio at 44.1 kHz (None = no audio, timed by its words).
type Spoken = (Option<std::sync::Arc<Vec<f32>>>, f64);

/// Finds each line's audio: your take, else the draft voice (per
/// `edl.voice`). Problems become notes, never failures.
fn line_audio(
    dir: &Path,
    edl: &Edl,
    line: &Line,
    lang: Option<&str>,
    notes: &mut Vec<String>,
) -> Spoken {
    let take = voice::take_path(dir, &line.id);
    let mut path = None;
    if lang.is_none() && matches!(edl.voice.as_str(), "auto" | "takes") && take.is_file() {
        path = Some(take);
    } else if matches!(edl.voice.as_str(), "auto" | "draft") {
        // What the voice says (respellings applied); a translation's draft
        // is filed apart from the original's.
        let mut said = line.clone();
        said.text = edl.spoken(&line.text);
        if let Some(lang) = lang {
            said.id = format!("{}-{lang}", line.id);
        }
        match voice::draft(dir, &said, &edl.draft_voice) {
            Ok(p) => path = Some(p),
            Err(e) => {
                let n = format!("no draft voice: {e}");
                if !notes.contains(&n) {
                    notes.push(n);
                }
            }
        }
    }
    let Some(path) = path else {
        return (None, voice::estimate_seconds(&line.text));
    };
    match voice::read_wav(&path) {
        Ok(pcm) => {
            let pcm = voice::trim_silence(&pcm);
            let audio = voice::resample(&pcm, AUDIO_RATE);
            let secs = audio.len() as f64 / AUDIO_RATE as f64;
            (Some(std::sync::Arc::new(audio)), secs)
        }
        Err(e) => {
            notes.push(e);
            (None, voice::estimate_seconds(&line.text))
        }
    }
}

/// Where recording second `t` plays, or the next kept moment after it:
/// (finished-video second, kept recording second).
fn place(edl: &Edl, tl: &Timeline, t: f64) -> (f64, f64) {
    if let Some(o) = tl.output_time(t) {
        return (o, t);
    }
    match edl.segments.iter().find(|s| s.from >= t) {
        Some(s) => (tl.output_time(s.from).unwrap_or(tl.body_end()), s.from),
        None => (
            tl.body_end(),
            edl.segments.last().map(|s| s.to).unwrap_or(0.0),
        ),
    }
}

/// Holds that give every line time to be said before the next begins (and
/// before the end card): (holds, each line's start).
fn fit(edl: &Edl, lines: &[&Line], secs: &[f64]) -> (Vec<Insert>, Vec<f64>) {
    let mut holds: Vec<Insert> = Vec::new();
    for i in 0..lines.len() {
        let tl = Timeline::new(edl, &holds);
        let (start, _) = place(edl, &tl, lines[i].at);
        let need = start + secs[i] + LINE_GAP;
        let (bound, at) = match lines.get(i + 1) {
            Some(next) => place(edl, &tl, next.at),
            None => (
                tl.body_end(),
                edl.segments.last().map(|s| s.to).unwrap_or(0.0),
            ),
        };
        if bound < need {
            holds.push(Insert {
                at,
                seconds: need - bound,
                kind: "hold".into(),
                title: String::new(),
                items: Vec::new(),
            });
        }
    }
    let tl = Timeline::new(edl, &holds);
    let starts = lines.iter().map(|l| place(edl, &tl, l.at).0).collect();
    (holds, starts)
}

fn clock(t: f64) -> String {
    let s = t.max(0.0) as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Chapters from the lines' sections (the first starts at 0:00).
pub fn chapters(lines: &[&Line], starts: &[f64]) -> Vec<(f64, String)> {
    let mut out: Vec<(f64, String)> = Vec::new();
    for (l, s) in lines.iter().zip(starts) {
        let name = l.section.trim();
        if !name.is_empty() && out.last().is_none_or(|c| c.1 != name) {
            out.push((*s, name.to_string()));
        }
    }
    if let Some(first) = out.first_mut() {
        first.0 = 0.0;
    }
    out
}

pub fn chapters_text(ch: &[(f64, String)]) -> String {
    ch.iter()
        .map(|(t, n)| format!("{} {n}\n", clock(*t)))
        .collect()
}

/// Narration over the music, which ducks while a line is said.
fn mix_audio(
    edl: &Edl,
    dir: &Path,
    placed: &[(f64, std::sync::Arc<Vec<f32>>)],
    total: f64,
) -> Vec<i16> {
    let n = ((total + 0.2) * AUDIO_RATE as f64) as usize;
    let music = if edl.music.trim().eq_ignore_ascii_case("auto") {
        super::music::ambient(total + 0.2)
    } else {
        Vec::new()
    };
    let _ = dir;
    let mut voice_track = vec![0f32; n];
    let mut active = vec![false; n];
    for (start, audio) in placed {
        let s0 = (start * AUDIO_RATE as f64) as usize;
        for (k, v) in audio.iter().enumerate() {
            if let Some(slot) = voice_track.get_mut(s0 + k) {
                *slot += v * 0.95;
                active[s0 + k] = true;
            }
        }
    }
    // Smooth ducking: 1.0 → 0.35 over ~0.25 s.
    let k = 1.0 / (0.25 * AUDIO_RATE as f32);
    let mut g = 1.0f32;
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        let target = if active[i] { 0.35 } else { 1.0 };
        g += (target - g) * k * 4.0;
        let ml = music.get(i * 2).copied().unwrap_or(0) as f32 / 32768.0;
        let mr = music.get(i * 2 + 1).copied().unwrap_or(0) as f32 / 32768.0;
        let v = voice_track[i];
        out.push(((ml * g + v).clamp(-1.0, 1.0) * 32767.0) as i16);
        out.push(((mr * g + v).clamp(-1.0, 1.0) * 32767.0) as i16);
    }
    out
}

/// An ffmpeg process taking raw RGB frames on stdin, with `audio` (our
/// mix) and/or a music file mixed in.
fn encoder(
    bin: &Path,
    (w, h): (u32, u32),
    fps: f64,
    audio: Option<&Path>,
    music_file: Option<&Path>,
    total: f64,
    out: &Path,
) -> Option<Child> {
    let mut cmd = Command::new(bin);
    cmd.args([
        "-y",
        "-loglevel",
        "error",
        "-f",
        "rawvideo",
        "-pix_fmt",
        "rgb24",
    ])
    .args([
        "-s",
        &format!("{w}x{h}"),
        "-r",
        &format!("{fps}"),
        "-i",
        "-",
    ]);
    if let Some(a) = audio {
        cmd.arg("-i").arg(a);
    }
    if let Some(m) = music_file {
        cmd.args(["-stream_loop", "-1", "-i"]).arg(m);
    }
    let fade_at = (total - 2.5).max(0.0);
    let aac = ["-c:a", "aac", "-b:a", "160k"];
    match (audio.is_some(), music_file.is_some()) {
        (true, false) => {
            cmd.args(["-map", "0:v", "-map", "1:a"]).args(aac);
        }
        (false, true) => {
            cmd.args([
                "-filter_complex",
                &format!("[1:a]volume=0.3,afade=t=in:d=1,afade=t=out:st={fade_at:.2}:d=2.5[a]"),
                "-map",
                "0:v",
                "-map",
                "[a]",
            ])
            .args(aac);
        }
        (true, true) => {
            cmd.args([
                "-filter_complex",
                &format!(
                    "[2:a]volume=0.12,afade=t=in:d=1,afade=t=out:st={fade_at:.2}:d=2.5[m];\
[1:a][m]amix=inputs=2:duration=first:normalize=0[a]"
                ),
                "-map",
                "0:v",
                "-map",
                "[a]",
            ])
            .args(aac);
        }
        (false, false) => {}
    }
    cmd.args(["-t", &format!("{total:.3}")])
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-crf",
            "20",
            "-pix_fmt",
            "yuv420p",
            "-movflags",
            "+faststart",
        ])
        .arg(out)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    cmd.spawn().ok()
}

struct Enc {
    child: Child,
    stdin: Option<std::process::ChildStdin>,
    out: PathBuf,
    failed: bool,
}

impl Enc {
    fn start(child: Option<Child>, out: PathBuf) -> Option<Self> {
        let mut child = child?;
        let stdin = child.stdin.take();
        Some(Self {
            child,
            stdin,
            out,
            failed: false,
        })
    }
    fn frame(&mut self, img: &RgbImage) {
        if let Some(s) = self.stdin.as_mut()
            && !self.failed
            && s.write_all(img.as_raw()).is_err()
        {
            self.failed = true;
        }
    }
    fn finish(mut self) -> Result<PathBuf, String> {
        drop(self.stdin.take());
        let o = self.child.wait_with_output().map_err(|e| e.to_string())?;
        if o.status.success() && !self.failed {
            Ok(self.out)
        } else {
            Err(format!(
                "ffmpeg failed: {}",
                String::from_utf8_lossy(&o.stderr)
                    .lines()
                    .last()
                    .unwrap_or("error")
            ))
        }
    }
}

/// The recording's frames, decoded on demand (the last two kept, for
/// dissolves between stills). One per drawing thread.
struct Frames<'a> {
    dir: &'a Path,
    frames: &'a [FrameInfo],
    first: &'a RgbImage,
    size: (u32, u32),
    cache: Vec<(usize, std::sync::Arc<RgbImage>)>,
}

impl<'a> Frames<'a> {
    fn fresh(&self) -> Frames<'a> {
        Frames {
            dir: self.dir,
            frames: self.frames,
            first: self.first,
            size: self.size,
            cache: Vec::new(),
        }
    }

    fn index(&self, s: f64) -> usize {
        self.frames
            .partition_point(|fr| fr.t <= s)
            .saturating_sub(1)
    }

    fn ratio(&self, idx: usize) -> f64 {
        let fr = &self.frames[idx];
        if fr.css_w > 0.0 {
            self.size.0 as f64 / fr.css_w
        } else {
            1.0
        }
    }

    fn image(&mut self, idx: usize) -> std::sync::Arc<RgbImage> {
        if let Some((_, img)) = self.cache.iter().find(|c| c.0 == idx) {
            return img.clone();
        }
        let (sw, sh) = self.size;
        let img = image::open(self.dir.join("raw").join(&self.frames[idx].file))
            .map(|i| i.to_rgb8())
            .unwrap_or_else(|_| {
                self.cache
                    .last()
                    .map(|c| (*c.1).clone())
                    .unwrap_or_else(|| self.first.clone())
            });
        let img = if img.dimensions() == (sw, sh) {
            img
        } else {
            imageops::resize(&img, sw, sh, imageops::FilterType::Triangle)
        };
        let img = std::sync::Arc::new(img);
        if self.cache.len() >= 2 {
            self.cache.remove(0);
        }
        self.cache.push((idx, img.clone()));
        img
    }
}

/// Everything a cut draws from.
struct Scene<'a> {
    edl: &'a Edl,
    points: Vec<(f64, f64, f64)>,
    clicks: Vec<&'a Event>,
    /// Clicks and typing with the element's size, for highlights.
    targets: Vec<&'a Event>,
    privacy: &'a [Privacy],
    /// Stills mode: the camera drifts over each still, stills dissolve.
    stills: bool,
    duration: f64,
    /// A portrait frame's backdrop (with the title), by size.
    backdrops: std::sync::Mutex<HashMap<(u32, u32), RgbImage>>,
}

impl Scene<'_> {
    /// Boxes to pixelate at second `s` (CSS pixels): what was private just
    /// before and just after (the page changes between the two), and the
    /// edit list's own.
    fn blur_at(&self, s: f64) -> Vec<[f64; 4]> {
        let mut out: Vec<[f64; 4]> = Vec::new();
        if self.edl.privacy && !self.privacy.is_empty() {
            let i = self.privacy.partition_point(|p| p.t <= s);
            if i > 0 {
                out.extend(&self.privacy[i - 1].boxes);
            }
            if let Some(next) = self.privacy.get(i)
                && next.t - s < 5.0
            {
                out.extend(&next.boxes);
            }
            if i == 0 && out.is_empty() {
                // Before the first look: what it found then.
                out.extend(&self.privacy[0].boxes);
            }
        }
        out.extend(
            self.edl
                .blur
                .iter()
                .filter(|b| s >= b.from && s <= b.to)
                .map(|b| [b.x, b.y, b.w, b.h]),
        );
        out
    }
}

/// Pixelates a box (pixels) of `img`.
fn pixelate(img: &mut RgbImage, x: f64, y: f64, w: f64, h: f64) {
    let (iw, ih) = (img.width() as i64, img.height() as i64);
    let pad = 3.0;
    let (x0, y0) = (
        ((x - pad).floor() as i64).max(0),
        ((y - pad).floor() as i64).max(0),
    );
    let (x1, y1) = (
        ((x + w + pad).ceil() as i64).min(iw),
        ((y + h + pad).ceil() as i64).min(ih),
    );
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let block = ((iw as f64 / 110.0).max(6.0)) as i64;
    let mut by = y0;
    while by < y1 {
        let mut bx = x0;
        while bx < x1 {
            let (ex, ey) = ((bx + block).min(x1), (by + block).min(y1));
            let mut sum = [0u64; 3];
            let mut n = 0u64;
            for yy in by..ey {
                for xx in bx..ex {
                    let p = img.get_pixel(xx as u32, yy as u32).0;
                    for c in 0..3 {
                        sum[c] += p[c] as u64;
                    }
                    n += 1;
                }
            }
            let avg = image::Rgb([
                (sum[0] / n.max(1)) as u8,
                (sum[1] / n.max(1)) as u8,
                (sum[2] / n.max(1)) as u8,
            ]);
            for yy in by..ey {
                for xx in bx..ex {
                    img.put_pixel(xx as u32, yy as u32, avg);
                }
            }
            bx += block;
        }
        by += block;
    }
}

/// Outlines a box (pixels) and dims everything else by `dim` (0–1).
fn spotlight(img: &mut RgbImage, (x, y, w, h): (f64, f64, f64, f64), weight: f32) {
    if weight <= 0.0 {
        return;
    }
    let (iw, ih) = (img.width() as i64, img.height() as i64);
    let pad = 6.0;
    let (x0, y0, x1, y1) = (x - pad, y - pad, x + w + pad, y + h + pad);
    let dim = 0.18 * weight;
    let line = (iw as f64 / 400.0).max(2.0);
    for py in 0..ih {
        for px in 0..iw {
            let (fx, fy) = (px as f64 + 0.5, py as f64 + 0.5);
            let inside = fx >= x0 && fx <= x1 && fy >= y0 && fy <= y1;
            if !inside {
                blend(img, px, py, [10, 8, 20], dim);
            } else {
                let edge = (fx - x0).min(x1 - fx).min(fy - y0).min(y1 - fy);
                if edge < line {
                    blend(img, px, py, [167, 139, 250], 0.95 * weight);
                }
            }
        }
    }
}

/// A vertical frame: the page at full width in the upper middle, the title
/// above it, room for captions below — the usual layout of a short.
fn compose_portrait(sc: &Scene, fr: &mut Frames, s: f64, w: u32, h: u32) -> RgbImage {
    let (sw, sh) = fr.size;
    let fh = ((w as f64 * sh as f64 / sw as f64) as u32 / 2 * 2).clamp(2, h);
    let inner = compose(sc, fr, s, w, fh);
    let mut out = {
        let mut cache = sc.backdrops.lock().unwrap_or_else(|e| e.into_inner());
        cache
            .entry((w, h))
            .or_insert_with(|| {
                let mut bg = background(w, h);
                let f = font();
                let px = w as f32 * 0.07;
                let title = sc.edl.title.trim();
                if !title.is_empty() {
                    let lines = wrap(&f, px, title, w as f32 * 0.86).len().min(3) as f32;
                    let top = h as f32 * 0.4 - fh as f32 / 2.0;
                    let y = (top - lines * px * 1.25 - px * 0.6).max(px * 1.2) + px;
                    centered(&mut bg, &f, px, y, title, [245, 243, 255]);
                }
                bg
            })
            .clone()
    };
    let y0 = ((h as f64 * 0.4 - fh as f64 / 2.0).max(0.0)) as i64;
    imageops::overlay(&mut out, &inner, 0, y0);
    out
}

/// The recording at second `s`, framed for a `w`×`h` output: private parts
/// pixelated, zoom (and in stills mode a slow drift and dissolves), cursor,
/// ripples, the highlighted element.
fn compose(sc: &Scene, fr: &mut Frames, s: f64, w: u32, h: u32) -> RgbImage {
    let (sw, sh) = fr.size;
    if h > w && sw > sh {
        return compose_portrait(sc, fr, s, w, h);
    }
    let idx = fr.index(s);
    let ratio = fr.ratio(idx);
    let base = fr.image(idx);
    let blur = sc.blur_at(s);
    // Stills: dissolve from the previous still over the first 0.35 s.
    let dissolve = if sc.stills && idx > 0 {
        let k = (s - fr.frames[idx].t) / 0.35;
        (0.0..1.0).contains(&k).then_some(ease(k) as f32)
    } else {
        None
    };
    let src: std::borrow::Cow<RgbImage> = if blur.is_empty() && dissolve.is_none() {
        std::borrow::Cow::Borrowed(&*base)
    } else {
        let mut img = (*base).clone();
        if let Some(k) = dissolve {
            let prev = fr.image(idx - 1);
            for (p, q) in img.pixels_mut().zip(prev.pixels()) {
                for c in 0..3 {
                    p.0[c] = (q.0[c] as f32 * (1.0 - k) + p.0[c] as f32 * k) as u8;
                }
            }
        }
        for [x, y, bw, bh] in &blur {
            pixelate(&mut img, x * ratio, y * ratio, bw * ratio, bh * ratio);
        }
        std::borrow::Cow::Owned(img)
    };
    let aspect = w as f64 / h as f64;
    let (bw, bh) = if sw as f64 / sh as f64 > aspect {
        (sh as f64 * aspect, sh as f64)
    } else {
        (sw as f64, sw as f64 / aspect)
    };
    let (mx, my) = (sw as f64 / 2.0, sh as f64 / 2.0);
    let cur = cursor_at(&sc.points, s);
    let (scale, cx, cy) = match zoom_at(sc.edl, s) {
        Some((scale, wgt, tx, ty)) => (
            scale,
            mx + (tx * ratio - mx) * wgt,
            my + (ty * ratio - my) * wgt,
        ),
        None => (1.0, mx, my),
    };
    // Stills: a slow push-in over each still, so a still never looks frozen.
    let drift = if sc.stills {
        let t0 = fr.frames[idx].t;
        let t1 = fr
            .frames
            .get(idx + 1)
            .map(|f| f.t)
            .unwrap_or(sc.duration)
            .max(t0 + 0.5);
        1.0 + 0.05 * ease((s - t0) / (t1 - t0))
    } else {
        1.0
    };
    let scale = scale * drift;
    let cw = (bw / scale).round().clamp(2.0, sw as f64);
    let ch = (bh / scale).round().clamp(2.0, sh as f64);
    let x0 = (cx - cw / 2.0).clamp(0.0, sw as f64 - cw);
    let y0 = (cy - ch / 2.0).clamp(0.0, sh as f64 - ch);
    let view = imageops::crop_imm(&*src, x0 as u32, y0 as u32, cw as u32, ch as u32).to_image();
    let mut img = if (cw as u32, ch as u32) == (w, h) {
        view
    } else {
        imageops::resize(&view, w, h, imageops::FilterType::Triangle)
    };
    let k = w as f64 / cw;
    let to_out = |px: f64, py: f64| ((px * ratio - x0) * k, (py * ratio - y0) * k);
    if sc.edl.highlight
        && let Some((e, wgt)) = sc
            .targets
            .iter()
            .filter_map(|e| {
                let d = s - e.t;
                let wgt = if (-0.5..0.0).contains(&d) {
                    ease((d + 0.5) / 0.5)
                } else if (0.0..1.6).contains(&d) {
                    1.0
                } else if (1.6..2.0).contains(&d) {
                    1.0 - ease((d - 1.6) / 0.4)
                } else {
                    0.0
                };
                (wgt > 0.0).then_some((*e, wgt))
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))
    {
        let (cx, cy) = (e.x.unwrap_or(0.0), e.y.unwrap_or(0.0));
        let (ew, eh) = (e.w.unwrap_or(0.0), e.h.unwrap_or(0.0));
        let (ax, ay) = to_out(cx - ew / 2.0, cy - eh / 2.0);
        let (bx, by) = to_out(cx + ew / 2.0, cy + eh / 2.0);
        // Not for elements filling most of the frame.
        if (bx - ax) < w as f64 * 0.8 && (by - ay) < h as f64 * 0.8 {
            spotlight(&mut img, (ax, ay, bx - ax, by - ay), wgt as f32);
        }
    }
    if sc.edl.cursor {
        let unit = (w.min(h) as f64 / 720.0).max(0.6);
        for c in &sc.clicks {
            let p = (s - c.t) / 0.5;
            if (0.0..1.0).contains(&p) {
                let (ox, oy) = to_out(c.x.unwrap_or(0.0), c.y.unwrap_or(0.0));
                ripple(&mut img, ox, oy, p, 40.0 * unit);
            }
        }
        if let Some((px, py)) = cur {
            let (ox, oy) = to_out(px, py);
            cursor(&mut img, ox, oy, 34.0 * unit);
        }
    }
    img
}

/// The frame at finished-video second `ot`, without captions.
fn frame_at(
    sc: &Scene,
    tl: &Timeline,
    fr: &mut Frames,
    f: &FontRef,
    ot: f64,
    w: u32,
    h: u32,
) -> RgbImage {
    let edl = sc.edl;
    let fade =
        |local: f64, len: f64| (local / 0.35).min((len - local) / 0.35).clamp(0.0, 1.0) as f32;
    match tl.at(ot) {
        Moment::Title(l) => card(
            w,
            h,
            f,
            &edl.title,
            &edl.subtitle,
            fade(l, super::TITLE_SECONDS),
        ),
        Moment::End(l) => card(w, h, f, &edl.end_text, "", fade(l, super::END_SECONDS)),
        Moment::Frame(s) => compose(sc, fr, s, w, h),
        Moment::Insert(i, _) if i.kind == "hold" => compose(sc, fr, i.at, w, h),
        Moment::Insert(i, l) => insert_card(w, h, f, i, fade(l, i.seconds)),
    }
}

/// A grid of frames from the video (12), to check it at a glance.
fn contact_sheet(frames: &[RgbImage]) -> Option<RgbImage> {
    let first = frames.first()?;
    let (tw, th) = (
        320u32,
        (320.0 * first.height() as f64 / first.width() as f64) as u32,
    );
    let cols = 4u32;
    let rows = (frames.len() as u32).div_ceil(cols);
    let gap = 6;
    let mut sheet = RgbImage::from_pixel(
        cols * (tw + gap) + gap,
        rows * (th + gap) + gap,
        image::Rgb([20, 18, 28]),
    );
    for (i, f) in frames.iter().enumerate() {
        let t = imageops::resize(f, tw, th, imageops::FilterType::Triangle);
        let (c, r) = (i as u32 % cols, i as u32 / cols);
        imageops::overlay(
            &mut sheet,
            &t,
            (gap + c * (tw + gap)) as i64,
            (gap + r * (th + gap)) as i64,
        );
    }
    Some(sheet)
}

/// One cut, rendered.
struct CutOut {
    mp4: Option<PathBuf>,
    gif: Option<PathBuf>,
    srt: PathBuf,
    seconds: f64,
    files: Vec<PathBuf>,
}

/// How to render.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// A quick low-resolution draft (`preview.mp4`): long cut, 480 px,
    /// 12 fps, no GIF, no clean master.
    pub preview: bool,
    /// A translation (`translations.<lang>`): `video.<lang>.mp4`,
    /// `captions.<lang>.srt`.
    pub lang: Option<String>,
}

/// File names for one cut of one render.
struct Names {
    video: String,
    caps: String,
    gif: bool,
    clean: bool,
    extras: bool,
}

#[allow(clippy::too_many_arguments)]
fn render_cut(
    dir: &Path,
    cut: &str,
    sc: &Scene,
    fr: &mut Frames,
    spoken: &HashMap<String, Spoken>,
    names: &Names,
    fps: f64,
    notes: &mut Vec<String>,
    progress: &dyn Fn(String),
) -> Result<CutOut, String> {
    use rayon::prelude::*;
    let edl = sc.edl;
    let portrait = cut == "short";
    let (sw, sh) = fr.size;
    let (w, h) = if portrait {
        match edl.width {
            1080.. => (1080, 1920),
            720.. => (720, 1280),
            _ => (360, 640),
        }
    } else {
        let w = edl.width.min(sw) / 2 * 2;
        (
            w,
            ((w as f64 * sh as f64 / sw as f64) as u32 / 2 * 2).max(2),
        )
    };
    let f = font();
    let lines: Vec<&Line> = edl.lines.iter().filter(|l| l.in_cut(cut)).collect();
    let secs: Vec<f64> = lines
        .iter()
        .map(|l| spoken.get(&l.id).map(|s| s.1).unwrap_or(0.0))
        .collect();
    let (holds, starts) = fit(edl, &lines, &secs);
    let tl = Timeline::new(edl, &holds);
    let total = tl.total();
    let cues: Vec<Cue> = if lines.is_empty() {
        caption_cues(edl, &tl)
    } else {
        lines
            .iter()
            .zip(&starts)
            .zip(&secs)
            .flat_map(|((l, s), d)| line_cues(&l.text, *s, s + d))
            .collect()
    };
    let mut files = Vec::new();

    // Audio: narration over (ducked) generated music.
    let placed: Vec<(f64, std::sync::Arc<Vec<f32>>)> = lines
        .iter()
        .zip(&starts)
        .filter_map(|(l, s)| spoken.get(&l.id)?.0.clone().map(|a| (*s, a)))
        .collect();
    let music = edl.music.trim();
    let auto_music = music.eq_ignore_ascii_case("auto");
    let music_file = (!music.is_empty() && !auto_music && !music.eq_ignore_ascii_case("none"))
        .then(|| {
            let p = if Path::new(music).is_absolute() {
                PathBuf::from(music)
            } else {
                dir.join(music)
            };
            if !p.is_file() {
                notes.push(format!("music file {} not found — no music", p.display()));
            }
            p
        })
        .filter(|p| p.is_file());
    let ffmpeg = super::ffmpeg();
    let audio_path = dir.join(format!("{}-audio.wav", names.video));
    let audio = if ffmpeg.is_some() && (auto_music || !placed.is_empty()) {
        super::music::write_wav(&audio_path, &mix_audio(edl, dir, &placed, total))
            .map_err(|e| e.to_string())?;
        Some(audio_path.as_path())
    } else {
        None
    };
    let mut captioned = ffmpeg.as_ref().and_then(|bin| {
        let out = dir.join(format!("{}.mp4", names.video));
        Enc::start(
            encoder(bin, (w, h), fps, audio, music_file.as_deref(), total, &out),
            out,
        )
    });
    let mut clean = ffmpeg
        .as_ref()
        .filter(|_| !portrait && edl.clean_master && names.clean)
        .and_then(|bin| {
            let out = dir.join(format!("{}-clean.mp4", names.video));
            Enc::start(
                encoder(bin, (w, h), fps, audio, music_file.as_deref(), total, &out),
                out,
            )
        });
    if ffmpeg.is_some() && captioned.is_none() {
        notes.push("ffmpeg couldn't start".into());
    }

    let make_gif = names.gif;
    let gif_every = (fps / 10.0).round().max(1.0) as usize;
    let gw = GIF_WIDTH.min(w) / 2 * 2;
    let gh = ((gw as f64 * h as f64 / w as f64) as u32).max(2);
    // GIF frames, with how many times each repeats (unchanged ones merge).
    let mut gif_frames: Vec<(Vec<u8>, u16)> = Vec::new();
    let n = (total * fps).ceil() as usize;
    let mut last_pct = 0;
    let mut review: Option<RgbImage> = None;
    let mut contact: Vec<RgbImage> = Vec::new();
    let contact_every = (n / 12).max(1);
    // Frames are drawn on every core, a batch at a time, and written in order.
    const BATCH: usize = 16;
    let want_clean = clean.is_some();
    let mut i0 = 0;
    while i0 < n {
        let batch: Vec<usize> = (i0..(i0 + BATCH).min(n)).collect();
        let drawn: Vec<(Option<RgbImage>, RgbImage)> = batch
            .par_iter()
            .map_init(
                || fr.fresh(),
                |frl, &i| {
                    let ot = i as f64 / fps;
                    let img = frame_at(sc, &tl, frl, &f, ot, w, h);
                    let plain = want_clean.then(|| img.clone());
                    let mut img = img;
                    if let Some(c) = cues.iter().find(|c| ot >= c.from && ot < c.to) {
                        caption(&mut img, &f, &c.text, portrait);
                    }
                    (plain, img)
                },
            )
            .collect();
        for (&i, (plain, img)) in batch.iter().zip(drawn) {
            let ot = i as f64 / fps;
            if let (Some(e), Some(p)) = (clean.as_mut(), plain.as_ref()) {
                e.frame(p);
            }
            if let Some(e) = captioned.as_mut() {
                e.frame(&img);
            }
            if portrait && review.is_none() && i >= n / 2 {
                review = Some(img.clone());
            }
            if names.extras && i % contact_every == contact_every / 2 && contact.len() < 12 {
                contact.push(img.clone());
            }
            if make_gif && i % gif_every == 0 && ot <= GIF_MAX_SECONDS {
                let g = if (gw, gh) != (w, h) {
                    imageops::resize(&img, gw, gh, imageops::FilterType::Triangle)
                } else {
                    img
                }
                .into_raw();
                match gif_frames.last_mut() {
                    Some((prev, k)) if *prev == g => *k += 1,
                    _ => gif_frames.push((g, 1)),
                }
            }
            let pct = i * 100 / n.max(1);
            if pct >= last_pct + 25 {
                last_pct = pct;
                progress(format!("rendering the {cut} cut… {pct}%"));
            }
        }
        i0 += BATCH;
    }
    let mut mp4 = None;
    if let Some(e) = captioned {
        match e.finish() {
            Ok(p) => {
                files.push(p.clone());
                mp4 = Some(p);
            }
            Err(e) => notes.push(e),
        }
    }
    if let Some(e) = clean {
        match e.finish() {
            Ok(p) => files.push(p),
            Err(e) => notes.push(e),
        }
    }
    let _ = std::fs::remove_file(&audio_path);

    let mut gif = None;
    if make_gif {
        progress("making the GIF…".into());
        let p = dir.join(format!("{}.gif", names.video));
        let file = std::fs::File::create(&p).map_err(|e| e.to_string())?;
        let mut enc = gif::Encoder::new(std::io::BufWriter::new(file), gw as u16, gh as u16, &[])
            .map_err(|e| e.to_string())?;
        let _ = enc.set_repeat(gif::Repeat::Infinite);
        let step = (100.0 * gif_every as f64 / fps) as u16;
        for (rgb, k) in &gif_frames {
            let mut g = gif::Frame::from_rgb_speed(gw as u16, gh as u16, rgb, 20);
            g.delay = step.saturating_mul(*k);
            enc.write_frame(&g).map_err(|e| e.to_string())?;
        }
        files.push(p.clone());
        gif = Some(p);
    }
    let srt_path = dir.join(format!("{}.srt", names.caps));
    std::fs::write(&srt_path, srt_of(&cues)).map_err(|e| e.to_string())?;
    let vtt_path = dir.join(format!("{}.vtt", names.caps));
    std::fs::write(&vtt_path, vtt_of(&cues)).map_err(|e| e.to_string())?;
    files.push(srt_path.clone());
    files.push(vtt_path);
    if !names.extras {
        return Ok(CutOut {
            mp4,
            gif,
            srt: srt_path,
            seconds: total,
            files,
        });
    }
    let ch = chapters(&lines, &starts);
    if !portrait && !ch.is_empty() {
        let p = dir.join("chapters.txt");
        std::fs::write(&p, chapters_text(&ch)).map_err(|e| e.to_string())?;
        files.push(p);
    }
    if let Some(sheet) = contact_sheet(&contact) {
        let p = dir.join(if portrait {
            "short-contact.png"
        } else {
            "contact.png"
        });
        sheet.save(&p).map_err(|e| e.to_string())?;
        files.push(p);
    }

    // Thumbnail: the first zoomed action (or the middle), title over it.
    let s = edl
        .zooms
        .first()
        .map(|z| z.at + 0.3)
        .unwrap_or_else(|| frame_time(edl, &tl, total / 2.0));
    let (tw, th) = if portrait { (720, 1280) } else { (1280, 720) };
    let thumb_title = if edl.title.trim().is_empty() {
        edl.end_text.as_str()
    } else {
        edl.title.as_str()
    };
    // A short's frame already carries the title above the page.
    let t = if portrait {
        compose(sc, fr, s, tw, th)
    } else {
        thumbnail(compose(sc, fr, s, tw, th), &f, thumb_title)
    };
    let tp = dir.join(if portrait {
        "short-thumbnail.png"
    } else {
        "thumbnail.png"
    });
    t.save(&tp).map_err(|e| e.to_string())?;
    files.push(tp);
    if let Some(mut r) = review {
        shade_covered(&mut r);
        let rp = dir.join("short-review.png");
        r.save(&rp).map_err(|e| e.to_string())?;
        files.push(rp);
    }
    Ok(CutOut {
        mp4,
        gif,
        srt: srt_path,
        seconds: total,
        files,
    })
}

/// Renders `<dir>/edl.json`: every cut it asks for, with narration,
/// captions, chapters and thumbnails. Blocking; `progress` gets a few
/// status lines.
pub fn render(dir: &Path, progress: impl Fn(String)) -> Result<Rendered, String> {
    render_with(dir, &Options::default(), progress)
}

/// [`render`] with options: a quick preview, or a translation.
pub fn render_with(
    dir: &Path,
    opts: &Options,
    progress: impl Fn(String),
) -> Result<Rendered, String> {
    let Saved {
        frames,
        events,
        duration,
        edl,
        mode,
        privacy,
        ..
    } = load(dir)?;
    let mut edl = edl.normalized(duration);
    if let Some(lang) = &opts.lang {
        edl = edl.in_language(lang).ok_or_else(|| {
            format!("no `{lang}` translation in the edit list — /video translate {lang}")
        })?;
    }
    if opts.preview {
        edl.width = edl.width.min(480);
        edl.cuts = vec!["long".into()];
    }
    if frames.is_empty() {
        return Err("the recording has no frames".into());
    }
    let first = image::open(dir.join("raw").join(&frames[0].file))
        .map_err(|e| e.to_string())?
        .to_rgb8();
    let mut fr = Frames {
        dir,
        frames: &frames,
        size: first.dimensions(),
        first: &first,
        cache: Vec::new(),
    };
    let sc = Scene {
        edl: &edl,
        points: events
            .iter()
            .filter_map(|e| Some((e.t, e.x?, e.y?)))
            .collect(),
        clicks: events
            .iter()
            .filter(|e| e.kind == "click" && e.x.is_some())
            .collect(),
        targets: events
            .iter()
            .filter(|e| {
                matches!(e.kind.as_str(), "click" | "type")
                    && e.x.is_some()
                    && e.w.is_some_and(|w| w > 0.0)
            })
            .collect(),
        privacy: &privacy,
        stills: mode == super::record::Mode::Stills,
        duration,
        backdrops: Default::default(),
    };
    let mut notes: Vec<String> = Vec::new();
    let mut spoken: HashMap<String, Spoken> = HashMap::new();
    if !edl.lines.is_empty() && edl.voice != "none" {
        progress(format!("voicing {} lines…", edl.lines.len()));
    }
    for l in &edl.lines {
        let s = if edl.voice == "none" {
            (None, voice::estimate_seconds(&l.text))
        } else {
            line_audio(dir, &edl, l, opts.lang.as_deref(), &mut notes)
        };
        spoken.insert(l.id.clone(), s);
    }
    let fps = if opts.preview { 12.0 } else { FPS };
    let mut main: Option<CutOut> = None;
    let mut files = Vec::new();
    for (k, cut) in edl.cuts.iter().enumerate() {
        let short = cut == "short";
        let names = match (&opts.lang, opts.preview) {
            (_, true) => Names {
                video: "preview".into(),
                caps: "preview".into(),
                gif: false,
                clean: false,
                extras: false,
            },
            (Some(lang), _) => Names {
                video: format!("{}.{lang}", if short { "short" } else { "video" }),
                caps: format!("{}.{lang}", if short { "short" } else { "captions" }),
                gif: false,
                clean: false,
                extras: false,
            },
            (None, _) => Names {
                video: if short { "short" } else { "video" }.into(),
                caps: if short { "short" } else { "captions" }.into(),
                gif: k == 0,
                clean: true,
                extras: true,
            },
        };
        let out = render_cut(
            dir, cut, &sc, &mut fr, &spoken, &names, fps, &mut notes, &progress,
        )?;
        files.extend(out.files.iter().cloned());
        if main.is_none() {
            main = Some(out);
        }
    }
    let main = main.ok_or("no cuts to render")?;
    if super::ffmpeg().is_none() {
        notes.push("no ffmpeg, so no MP4, voice or music — `/video setup` downloads it (or install ffmpeg), then `/video render`".into());
    }
    if opts.lang.is_none() && !opts.preview {
        std::fs::write(
            dir.join("render.json"),
            serde_json::json!({
                "seconds": main.seconds,
                "files": files.iter().filter_map(|p| p.file_name()).map(|n| n.to_string_lossy()).collect::<Vec<_>>(),
                "notes": notes,
            })
            .to_string(),
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(Rendered {
        mp4: main.mp4,
        gif: main.gif.unwrap_or_else(|| dir.join("video.gif")),
        srt: main.srt,
        seconds: main.seconds,
        note: (!notes.is_empty()).then(|| notes.join("; ")),
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::{Caption, Line, Segment, Zoom};

    #[test]
    fn output_and_source_times_agree() {
        let edl = Edl {
            title: "T".into(),
            segments: vec![
                Segment {
                    from: 0.0,
                    to: 4.0,
                    speed: 1.0,
                },
                Segment {
                    from: 4.0,
                    to: 16.0,
                    speed: 6.0,
                },
            ],
            ..Default::default()
        };
        // Title 2.5 s, then 4 s, then 12 s at 6× = 2 s.
        assert!((source_time(&edl, 2.5) - 0.0).abs() < 1e-9);
        assert!((source_time(&edl, 6.5) - 4.0).abs() < 1e-9);
        assert!((source_time(&edl, 7.5) - 10.0).abs() < 1e-9);
        assert!((edl.output_time(10.0).unwrap() - 7.5).abs() < 1e-9);
    }

    #[test]
    fn zoom_eases_in_holds_and_out() {
        let edl = Edl {
            zooms: vec![Zoom {
                at: 5.0,
                x: 100.0,
                y: 50.0,
                scale: 2.0,
                hold: 1.0,
            }],
            ..Default::default()
        };
        assert!(zoom_at(&edl, 3.0).is_none());
        let (half, _, _, _) = zoom_at(&edl, 4.7).unwrap();
        assert!(half > 1.0 && half < 2.0);
        assert_eq!(zoom_at(&edl, 5.5).unwrap().0, 2.0);
        assert!(zoom_at(&edl, 7.0).is_none());
    }

    #[test]
    fn the_cursor_waits_then_glides() {
        let pts = [(1.0, 0.0, 0.0), (5.0, 100.0, 0.0)];
        assert_eq!(cursor_at(&pts, 2.0), Some((0.0, 0.0)));
        let (x, _) = cursor_at(&pts, 4.7).unwrap();
        assert!(x > 0.0 && x < 100.0);
        assert_eq!(cursor_at(&pts, 9.0), Some((100.0, 0.0)));
    }

    #[test]
    fn captions_follow_the_cut() {
        let edl = Edl {
            segments: vec![Segment {
                from: 2.0,
                to: 6.0,
                speed: 1.0,
            }],
            captions: vec![
                Caption {
                    from: 3.0,
                    to: 4.0,
                    text: "Kept".into(),
                },
                Caption {
                    from: 7.0,
                    to: 8.0,
                    text: "Cut".into(),
                },
            ],
            ..Default::default()
        };
        let s = srt(&edl);
        assert!(s.contains("00:00:01,000 --> 00:00:02,000\nKept"), "{s}");
        assert!(!s.contains("Cut"));
    }

    /// A tiny recording rendered end to end (GIF always; MP4 if ffmpeg).
    #[test]
    fn a_small_recording_renders() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().join("raw");
        std::fs::create_dir_all(&raw).unwrap();
        let mut frames = Vec::new();
        for i in 0..4u8 {
            let img = RgbImage::from_pixel(320, 200, image::Rgb([40 * i, 80, 120]));
            let file = format!("f{i}.jpg");
            img.save(raw.join(&file)).unwrap();
            frames.push(FrameInfo {
                t: i as f64,
                file,
                css_w: 320.0,
            });
        }
        let events = vec![Event {
            t: 1.5,
            kind: "click".into(),
            x: Some(160.0),
            y: Some(100.0),
            w: Some(60.0),
            h: Some(24.0),
            label: "OK".into(),
        }];
        let mut edl = super::super::draft(&events, 4.0, "Test video");
        edl.voice = "none".into();
        edl.width = 320;
        edl.cuts = vec!["long".into(), "short".into()];
        edl.lines[0].section = "Start".into();
        std::fs::write(
            dir.path().join("frames.json"),
            serde_json::to_string(&frames).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("events.json"),
            serde_json::to_string(&events).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("edl.json"),
            serde_json::to_string(&edl).unwrap(),
        )
        .unwrap();
        std::fs::write(dir.path().join("meta.json"), r#"{"duration":4.0}"#).unwrap();
        let r = render(dir.path(), |_| {}).unwrap();
        let gif = std::fs::read(&r.gif).unwrap();
        assert_eq!(&gif[..3], b"GIF");
        assert!(r.seconds > 4.0, "cards included");
        if let Some(mp4) = r.mp4 {
            assert!(std::fs::metadata(mp4).unwrap().len() > 1000);
            assert!(dir.path().join("short.mp4").is_file());
            assert!(dir.path().join("video-clean.mp4").is_file());
        }
        for f in [
            "thumbnail.png",
            "short-thumbnail.png",
            "short-review.png",
            "captions.vtt",
            "short.srt",
            "chapters.txt",
        ] {
            assert!(dir.path().join(f).is_file(), "{f}");
        }
        let srt = std::fs::read_to_string(&r.srt).unwrap();
        assert!(
            srt.contains("Click OK."),
            "captions follow the script: {srt}"
        );
    }

    #[test]
    fn private_boxes_are_pixelated_around_page_changes() {
        let edl = Edl {
            privacy: true,
            blur: vec![crate::video::BlurBox {
                from: 0.0,
                to: 2.0,
                x: 1.0,
                y: 1.0,
                w: 2.0,
                h: 2.0,
            }],
            ..Default::default()
        };
        let privacy = vec![
            Privacy {
                t: 1.0,
                boxes: vec![[10.0, 10.0, 5.0, 5.0]],
            },
            Privacy {
                t: 4.0,
                boxes: vec![[50.0, 50.0, 5.0, 5.0]],
            },
        ];
        let sc = Scene {
            edl: &edl,
            points: vec![],
            clicks: vec![],
            targets: vec![],
            privacy: &privacy,
            stills: false,
            duration: 10.0,
            backdrops: Default::default(),
        };
        // Between two looks: both (the page may be changing), plus the manual box.
        assert_eq!(sc.blur_at(1.5).len(), 3);
        // After the last look: only it.
        assert_eq!(sc.blur_at(9.0), vec![[50.0, 50.0, 5.0, 5.0]]);
        // Before the first look: the first.
        assert!(sc.blur_at(0.2).contains(&[10.0, 10.0, 5.0, 5.0]));

        let mut img =
            RgbImage::from_fn(64, 64, |x, y| image::Rgb([(x * 4) as u8, (y * 4) as u8, 0]));
        let before = img.clone();
        pixelate(&mut img, 10.0, 10.0, 20.0, 20.0);
        assert_ne!(img, before);
        assert_eq!(
            img.get_pixel(60, 60),
            before.get_pixel(60, 60),
            "outside untouched"
        );
        // A block is one colour.
        assert_eq!(img.get_pixel(8, 8), img.get_pixel(9, 9));
    }

    #[test]
    fn code_and_terminal_cards_draw() {
        let f = font();
        let ins = Insert {
            at: 0.0,
            seconds: 3.0,
            kind: "terminal".into(),
            title: "bash".into(),
            items: vec!["$ cargo test".into(), "test result: ok".into()],
        };
        let img = insert_card(640, 360, &f, &ins, 1.0);
        assert_eq!(img.dimensions(), (640, 360));
        let plain = background(640, 360);
        assert_ne!(img, plain, "something drawn");
    }

    fn line(id: &str, at: f64, text: &str, section: &str) -> Line {
        Line {
            id: id.into(),
            at,
            text: text.into(),
            section: section.into(),
            only: Vec::new(),
        }
    }

    #[test]
    fn the_video_holds_until_a_line_is_said() {
        let edl = Edl {
            end_text: "E".into(),
            segments: vec![Segment {
                from: 0.0,
                to: 10.0,
                speed: 1.0,
            }],
            ..Default::default()
        };
        let (a, b) = (line("a", 1.0, "x", "Intro"), line("b", 3.0, "y", "Main"));
        // `a` takes 5 s to say but `b` starts 2 s later: hold 3.35 s at 3 s.
        let (holds, starts) = fit(&edl, &[&a, &b], &[5.0, 1.0]);
        assert_eq!(holds.len(), 1);
        assert_eq!(holds[0].at, 3.0);
        assert!((holds[0].seconds - (5.0 + LINE_GAP - 2.0)).abs() < 1e-9);
        assert!(
            (starts[1] - (1.0 + 5.0 + LINE_GAP)).abs() < 1e-9,
            "{starts:?}"
        );
        // A last line that runs past the recording holds before the end card.
        let (holds, _) = fit(&edl, &[&a, &line("c", 9.0, "z", "")], &[1.0, 4.0]);
        assert_eq!(holds.last().unwrap().at, 10.0);
        let ch = chapters(&[&a, &b], &[1.0, 6.35]);
        assert_eq!(chapters_text(&ch), "0:00 Intro\n0:06 Main\n");
    }

    #[test]
    fn long_lines_become_several_captions() {
        let cues = line_cues(
            "First we open the settings page, then we turn on dark mode and save it for everyone.",
            10.0,
            16.0,
        );
        assert!(cues.len() >= 2, "{cues:?}");
        assert_eq!(cues[0].from, 10.0);
        assert!((cues.last().unwrap().to - 16.0).abs() < 1e-9);
        assert!(cues.iter().all(|c| c.text.split_whitespace().count() <= 12));
        let v = vtt_of(&cues);
        assert!(v.starts_with("WEBVTT") && v.contains("00:00:10.000 --> "));
    }
}

#[cfg(test)]
mod look {
    use super::*;

    /// Writes sample frames to $DCODE_AI_LOOK (a folder) to eyeball the
    /// look: a card, a zoomed click with cursor, ripple and caption.
    #[test]
    #[ignore]
    fn sample_frames() {
        let Ok(out) = std::env::var("DCODE_AI_LOOK") else {
            return;
        };
        let f = font();
        let mut page = RgbImage::from_pixel(1280, 720, image::Rgb([248, 248, 252]));
        for y in 0..60 {
            for x in 0..1280 {
                page.put_pixel(x, y, image::Rgb([60, 64, 90]));
            }
        }
        draw_text(
            &mut page,
            &f,
            28.0,
            30.0,
            40.0,
            "Acme Projects",
            [255, 255, 255],
            1.0,
        );
        for y in 300..350 {
            for x in 540..740 {
                page.put_pixel(x, y, image::Rgb([90, 80, 220]));
            }
        }
        draw_text(
            &mut page,
            &f,
            22.0,
            565.0,
            333.0,
            "Create project",
            [255, 255, 255],
            1.0,
        );
        let c = card(
            1280,
            720,
            &f,
            "Creating a project in Acme",
            "a 30-second tour",
            1.0,
        );
        c.save(format!("{out}/card.png")).unwrap();
        let (scale, x, y) = (1.6, 640.0, 325.0);
        let cw = 1280.0 / scale;
        let ch = 720.0 / scale;
        let x0 = (x - cw / 2.0_f64).clamp(0.0, 1280.0 - cw);
        let y0 = (y - ch / 2.0_f64).clamp(0.0, 720.0 - ch);
        let view = imageops::crop_imm(&page, x0 as u32, y0 as u32, cw as u32, ch as u32).to_image();
        let mut img = imageops::resize(&view, 1280, 720, imageops::FilterType::Triangle);
        let k = 1280.0 / cw;
        ripple(&mut img, (640.0 - x0) * k, (325.0 - y0) * k, 0.4, 28.0);
        cursor(&mut img, (640.0 - x0) * k, (325.0 - y0) * k, 26.0);
        caption(
            &mut img,
            &f,
            "Click “Create project” to start a new one",
            false,
        );
        img.save(format!("{out}/zoom.png")).unwrap();
    }
}
