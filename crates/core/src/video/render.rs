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

use super::record::{Saved, load};
use super::timeline::{Moment, Timeline};
use super::{Edl, Event, FrameInfo, Insert, Line, voice};

pub const FPS: f64 = 30.0;
const GIF_EVERY: usize = 3; // 10 fps
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
fn line_audio(dir: &Path, edl: &Edl, line: &Line, notes: &mut Vec<String>) -> Spoken {
    let take = voice::take_path(dir, &line.id);
    let mut path = None;
    if matches!(edl.voice.as_str(), "auto" | "takes") && take.is_file() {
        path = Some(take);
    } else if matches!(edl.voice.as_str(), "auto" | "draft") {
        match voice::draft(dir, line, &edl.draft_voice) {
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
    .args(["-s", &format!("{w}x{h}"), "-r", "30", "-i", "-"]);
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

/// The recording's frames, decoded on demand (the last one kept).
struct Frames<'a> {
    dir: &'a Path,
    frames: &'a [FrameInfo],
    first: &'a RgbImage,
    size: (u32, u32),
    cache: Option<(usize, RgbImage)>,
}

impl Frames<'_> {
    /// The frame at second `s` and its CSS-to-pixel ratio.
    fn at(&mut self, s: f64) -> (&RgbImage, f64) {
        let idx = self
            .frames
            .partition_point(|fr| fr.t <= s)
            .saturating_sub(1);
        if self.cache.as_ref().is_none_or(|(c, _)| *c != idx) {
            let (sw, sh) = self.size;
            let img = image::open(self.dir.join("raw").join(&self.frames[idx].file))
                .map(|i| i.to_rgb8())
                .unwrap_or_else(|_| {
                    self.cache
                        .as_ref()
                        .map(|c| c.1.clone())
                        .unwrap_or_else(|| self.first.clone())
                });
            let img = if img.dimensions() == (sw, sh) {
                img
            } else {
                imageops::resize(&img, sw, sh, imageops::FilterType::Triangle)
            };
            self.cache = Some((idx, img));
        }
        let fr = &self.frames[idx];
        let ratio = if fr.css_w > 0.0 {
            self.size.0 as f64 / fr.css_w
        } else {
            1.0
        };
        (&self.cache.as_ref().expect("set above").1, ratio)
    }
}

/// Everything a cut draws from.
struct Scene<'a> {
    edl: &'a Edl,
    points: Vec<(f64, f64, f64)>,
    clicks: Vec<&'a Event>,
    /// A portrait frame's backdrop (with the title), by size.
    backdrops: std::cell::RefCell<HashMap<(u32, u32), RgbImage>>,
}

/// A vertical frame: the page at full width in the upper middle, the title
/// above it, room for captions below — the usual layout of a short.
fn compose_portrait(sc: &Scene, fr: &mut Frames, s: f64, w: u32, h: u32) -> RgbImage {
    let (sw, sh) = fr.size;
    let fh = ((w as f64 * sh as f64 / sw as f64) as u32 / 2 * 2).clamp(2, h);
    let inner = compose(sc, fr, s, w, fh);
    let mut out = sc
        .backdrops
        .borrow_mut()
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
        .clone();
    let y0 = ((h as f64 * 0.4 - fh as f64 / 2.0).max(0.0)) as i64;
    imageops::overlay(&mut out, &inner, 0, y0);
    out
}

/// The recording at second `s`, framed for a `w`×`h` output (zoom,
/// cursor, ripples; a portrait output follows the action).
fn compose(sc: &Scene, fr: &mut Frames, s: f64, w: u32, h: u32) -> RgbImage {
    let (sw, sh) = fr.size;
    let (src, ratio) = fr.at(s);
    let aspect = w as f64 / h as f64;
    let (bw, bh) = if sw as f64 / sh as f64 > aspect {
        (sh as f64 * aspect, sh as f64)
    } else {
        (sw as f64, sw as f64 / aspect)
    };
    let (mx, my) = (sw as f64 / 2.0, sh as f64 / 2.0);
    if h > w && sw > sh {
        return compose_portrait(sc, fr, s, w, h);
    }
    let cur = cursor_at(&sc.points, s);
    let (bx, by) = (mx, my);
    let (scale, cx, cy) = match zoom_at(sc.edl, s) {
        Some((scale, wgt, tx, ty)) => (
            scale,
            bx + (tx * ratio - bx) * wgt,
            by + (ty * ratio - by) * wgt,
        ),
        None => (1.0, bx, by),
    };
    let cw = (bw / scale).round().clamp(2.0, sw as f64);
    let ch = (bh / scale).round().clamp(2.0, sh as f64);
    let x0 = (cx - cw / 2.0).clamp(0.0, sw as f64 - cw);
    let y0 = (cy - ch / 2.0).clamp(0.0, sh as f64 - ch);
    let view = imageops::crop_imm(src, x0 as u32, y0 as u32, cw as u32, ch as u32).to_image();
    let mut img = if (cw as u32, ch as u32) == (w, h) {
        view
    } else {
        imageops::resize(&view, w, h, imageops::FilterType::Triangle)
    };
    let k = w as f64 / cw;
    let to_out = |px: f64, py: f64| ((px * ratio - x0) * k, (py * ratio - y0) * k);
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
        Moment::Insert(i, l) if i.kind == "hold" => {
            let _ = l;
            compose(sc, fr, i.at, w, h)
        }
        Moment::Insert(i, l) => insert_card(w, h, f, i, fade(l, i.seconds)),
    }
}

/// One cut, rendered.
struct CutOut {
    mp4: Option<PathBuf>,
    gif: Option<PathBuf>,
    srt: PathBuf,
    seconds: f64,
    files: Vec<PathBuf>,
}

#[allow(clippy::too_many_arguments)]
fn render_cut(
    dir: &Path,
    cut: &str,
    sc: &Scene,
    fr: &mut Frames,
    spoken: &HashMap<String, Spoken>,
    make_gif: bool,
    notes: &mut Vec<String>,
    progress: &dyn Fn(String),
) -> Result<CutOut, String> {
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
    let name = if portrait { "short" } else { "video" };
    let caps = if portrait { "short" } else { "captions" };
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
    let audio_path = dir.join(format!("{name}-audio.wav"));
    let audio = if ffmpeg.is_some() && (auto_music || !placed.is_empty()) {
        super::music::write_wav(&audio_path, &mix_audio(edl, dir, &placed, total))
            .map_err(|e| e.to_string())?;
        Some(audio_path.as_path())
    } else {
        None
    };
    let mut captioned = ffmpeg.as_ref().and_then(|bin| {
        let out = dir.join(format!("{name}.mp4"));
        Enc::start(
            encoder(bin, (w, h), audio, music_file.as_deref(), total, &out),
            out,
        )
    });
    let mut clean = ffmpeg
        .as_ref()
        .filter(|_| !portrait && edl.clean_master)
        .and_then(|bin| {
            let out = dir.join("video-clean.mp4");
            Enc::start(
                encoder(bin, (w, h), audio, music_file.as_deref(), total, &out),
                out,
            )
        });
    if ffmpeg.is_some() && captioned.is_none() {
        notes.push("ffmpeg couldn't start".into());
    }

    let gw = GIF_WIDTH.min(w) / 2 * 2;
    let gh = ((gw as f64 * h as f64 / w as f64) as u32).max(2);
    let mut gif_frames: Vec<Vec<u8>> = Vec::new();
    let n = (total * FPS).ceil() as usize;
    let mut last_pct = 0;
    let mut review: Option<RgbImage> = None;
    for i in 0..n {
        let ot = i as f64 / FPS;
        let mut img = frame_at(sc, &tl, fr, &f, ot, w, h);
        if let Some(e) = clean.as_mut() {
            e.frame(&img);
        }
        if let Some(c) = cues.iter().find(|c| ot >= c.from && ot < c.to) {
            caption(&mut img, &f, &c.text, portrait);
        }
        if let Some(e) = captioned.as_mut() {
            e.frame(&img);
        }
        if portrait && review.is_none() && i >= n / 2 {
            review = Some(img.clone());
        }
        if make_gif && i % GIF_EVERY == 0 && ot <= GIF_MAX_SECONDS {
            let g = if (gw, gh) != (w, h) {
                imageops::resize(&img, gw, gh, imageops::FilterType::Triangle)
            } else {
                img
            };
            gif_frames.push(g.into_raw());
        }
        let pct = i * 100 / n.max(1);
        if pct >= last_pct + 25 {
            last_pct = pct;
            progress(format!("rendering the {cut} cut… {pct}%"));
        }
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
        let p = dir.join(format!("{name}.gif"));
        let file = std::fs::File::create(&p).map_err(|e| e.to_string())?;
        let mut enc = gif::Encoder::new(std::io::BufWriter::new(file), gw as u16, gh as u16, &[])
            .map_err(|e| e.to_string())?;
        let _ = enc.set_repeat(gif::Repeat::Infinite);
        for rgb in &gif_frames {
            let mut g = gif::Frame::from_rgb_speed(gw as u16, gh as u16, rgb, 20);
            g.delay = (100.0 / (FPS / GIF_EVERY as f64)) as u16;
            enc.write_frame(&g).map_err(|e| e.to_string())?;
        }
        files.push(p.clone());
        gif = Some(p);
    }
    let srt_path = dir.join(format!("{caps}.srt"));
    std::fs::write(&srt_path, srt_of(&cues)).map_err(|e| e.to_string())?;
    let vtt_path = dir.join(format!("{caps}.vtt"));
    std::fs::write(&vtt_path, vtt_of(&cues)).map_err(|e| e.to_string())?;
    files.push(srt_path.clone());
    files.push(vtt_path);
    let ch = chapters(&lines, &starts);
    if !portrait && !ch.is_empty() {
        let p = dir.join("chapters.txt");
        std::fs::write(&p, chapters_text(&ch)).map_err(|e| e.to_string())?;
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
    let Saved {
        frames,
        events,
        duration,
        edl,
        ..
    } = load(dir)?;
    let edl = edl.normalized(duration);
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
        cache: None,
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
            line_audio(dir, &edl, l, &mut notes)
        };
        spoken.insert(l.id.clone(), s);
    }
    let mut main: Option<CutOut> = None;
    let mut files = Vec::new();
    for (k, cut) in edl.cuts.iter().enumerate() {
        let out = render_cut(
            dir,
            cut,
            &sc,
            &mut fr,
            &spoken,
            k == 0,
            &mut notes,
            &progress,
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
