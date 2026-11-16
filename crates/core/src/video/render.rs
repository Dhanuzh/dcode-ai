//! Renders an edit list: the recording's frames, sped up where it waits,
//! zoomed toward what it clicks, with a cursor and click ripples, captions,
//! title and end cards — to MP4 with background music (through `ffmpeg`)
//! and a GIF (always, in Rust).

use std::io::Write;
use std::path::{Path, PathBuf};

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use image::{RgbImage, imageops};

use super::record::{Saved, load};
use super::{END_SECONDS, Edl, Event, FrameInfo, TITLE_SECONDS};

pub const FPS: f64 = 30.0;
const GIF_EVERY: usize = 3; // 10 fps
const GIF_WIDTH: u32 = 640;
const GIF_MAX_SECONDS: f64 = 60.0;
const RAMP: f64 = 0.6;

static FONT_BYTES: &[u8] = include_bytes!("../../assets/fonts/Inter-SemiBold.ttf");

fn font() -> FontRef<'static> {
    FontRef::try_from_slice(FONT_BYTES).expect("bundled font")
}

/// What came out.
#[derive(Debug)]
pub struct Rendered {
    pub mp4: Option<PathBuf>,
    pub gif: PathBuf,
    pub srt: PathBuf,
    pub seconds: f64,
    /// Why there's no MP4 (no ffmpeg), or what went wrong with it.
    pub note: Option<String>,
}

fn ease(x: f64) -> f64 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

/// Recording second for output second `ot` (inside the body).
pub fn source_time(edl: &Edl, ot: f64) -> f64 {
    let mut left = ot
        - if edl.title.trim().is_empty() {
            0.0
        } else {
            TITLE_SECONDS
        };
    for s in &edl.segments {
        let d = (s.to - s.from) / s.speed;
        if left < d {
            return s.from + left.max(0.0) * s.speed;
        }
        left -= d;
    }
    edl.segments.last().map(|s| s.to).unwrap_or(0.0)
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

/// A title or end card.
fn card(w: u32, h: u32, f: &FontRef, title: &str, sub: &str, fade: f32) -> RgbImage {
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
    let px = h as f32 * 0.075;
    let lines = wrap(f, px, title, w as f32 * 0.8);
    let total = lines.len() as f32 * px * 1.25;
    let mut y = h as f32 / 2.0 - total / 2.0 + px;
    for l in &lines {
        let x = (w as f32 - text_width(f, px, l)) / 2.0;
        draw_text(&mut img, f, px, x, y, l, [245, 243, 255], fade);
        y += px * 1.25;
    }
    if !sub.trim().is_empty() {
        let spx = px * 0.5;
        let x = (w as f32 - text_width(f, spx, sub)) / 2.0;
        draw_text(
            &mut img,
            f,
            spx,
            x,
            y + spx * 0.4,
            sub,
            [190, 180, 230],
            fade,
        );
    }
    if fade < 1.0 {
        for p in img.pixels_mut() {
            for v in &mut p.0 {
                *v = (*v as f32 * fade) as u8;
            }
        }
    }
    img
}

fn caption(img: &mut RgbImage, f: &FontRef, text: &str) {
    let (w, h) = (img.width() as f32, img.height() as f32);
    let px = (h * 0.042).max(16.0);
    let lines: Vec<String> = wrap(f, px, text, w * 0.8).into_iter().take(2).collect();
    if lines.is_empty() {
        return;
    }
    let pad = px * 0.6;
    let tw = lines
        .iter()
        .map(|l| text_width(f, px, l))
        .fold(0.0, f32::max);
    let bh = lines.len() as f32 * px * 1.3 + pad;
    let (x0, y0) = ((w - tw) / 2.0 - pad, h - bh - h * 0.05);
    for y in y0.max(0.0) as i64..(y0 + bh) as i64 {
        for x in x0.max(0.0) as i64..(x0 + tw + 2.0 * pad) as i64 {
            blend(img, x, y, [16, 14, 24], 0.72);
        }
    }
    let mut base = y0 + pad / 2.0 + px;
    for l in &lines {
        let x = (w - text_width(f, px, l)) / 2.0;
        draw_text(img, f, px, x, base, l, [255, 255, 255], 1.0);
        base += px * 1.3;
    }
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

/// Captions as SRT, in finished-video time.
pub fn srt(edl: &Edl) -> String {
    let mut out = String::new();
    let mut n = 0;
    for c in &edl.captions {
        let a = (0..=20)
            .map(|i| c.from + (c.to - c.from) * i as f64 / 20.0)
            .find_map(|t| edl.output_time(t));
        let b = (0..=20)
            .rev()
            .map(|i| c.from + (c.to - c.from) * i as f64 / 20.0)
            .find_map(|t| edl.output_time(t));
        if let (Some(a), Some(b)) = (a, b)
            && b > a
        {
            n += 1;
            out.push_str(&format!(
                "{n}\n{} --> {}\n{}\n\n",
                srt_time(a),
                srt_time(b),
                c.text.trim()
            ));
        }
    }
    out
}

/// Renders `<dir>/edl.json` into `video.mp4` (with ffmpeg), `video.gif`
/// and `captions.srt`. Blocking; `progress` gets a few status lines.
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
    let (sw, sh) = (first.width(), first.height());
    let w = edl.width.min(sw) / 2 * 2;
    let h = ((w as f64 * sh as f64 / sw as f64) as u32 / 2 * 2).max(2);
    let f = font();
    let total = edl.output_seconds();
    let n = (total * FPS).ceil() as usize;
    let has_title = !edl.title.trim().is_empty();
    let has_end = !edl.end_text.trim().is_empty();

    let css_ratio = |fr: &FrameInfo| {
        if fr.css_w > 0.0 {
            sw as f64 / fr.css_w
        } else {
            1.0
        }
    };
    let points: Vec<(f64, f64, f64)> = events
        .iter()
        .filter_map(|e| Some((e.t, e.x?, e.y?)))
        .collect();
    let clicks: Vec<&Event> = events
        .iter()
        .filter(|e| e.kind == "click" && e.x.is_some())
        .collect();

    // ffmpeg for the MP4, with music.
    let ffmpeg = super::ffmpeg();
    let mp4_path = dir.join("video.mp4");
    let music_wav = dir.join("music.wav");
    let mut music_note = None;
    let mut child = match &ffmpeg {
        Some(bin) => {
            let mut cmd = std::process::Command::new(bin);
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
            let music = edl.music.trim();
            if music.eq_ignore_ascii_case("auto") {
                super::music::write_wav(&music_wav, &super::music::ambient(total + 0.5))
                    .map_err(|e| e.to_string())?;
                cmd.arg("-i").arg(&music_wav).args([
                    "-map",
                    "0:v",
                    "-map",
                    "1:a",
                    "-c:a",
                    "aac",
                    "-b:a",
                    "160k",
                    "-shortest",
                ]);
            } else if !music.is_empty() && !music.eq_ignore_ascii_case("none") {
                let p = if Path::new(music).is_absolute() {
                    PathBuf::from(music)
                } else {
                    dir.join(music)
                };
                if p.is_file() {
                    let fade_at = (total - 2.5).max(0.0);
                    cmd.args(["-stream_loop", "-1", "-i"]).arg(&p).args([
                        "-filter_complex",
                        &format!(
                            "[1:a]volume=0.3,afade=t=in:d=1,afade=t=out:st={fade_at:.2}:d=2.5[a]"
                        ),
                        "-map",
                        "0:v",
                        "-map",
                        "[a]",
                        "-c:a",
                        "aac",
                        "-b:a",
                        "160k",
                        "-t",
                        &format!("{total:.2}"),
                    ]);
                } else {
                    music_note = Some(format!("music file {} not found — no music", p.display()));
                }
            }
            cmd.args([
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
            .arg(&mp4_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
            cmd.spawn().ok()
        }
        None => None,
    };
    let mut stdin = child.as_mut().and_then(|c| c.stdin.take());

    // GIF frames, kept small.
    let gw = GIF_WIDTH.min(w) / 2 * 2;
    let gh = ((gw as f64 * h as f64 / w as f64) as u32).max(2);
    let mut gif_frames: Vec<Vec<u8>> = Vec::new();

    let mut cache: Option<(usize, RgbImage)> = None;
    let mut last_pct = 0;
    let mut write_failed = false;
    for i in 0..n {
        let ot = i as f64 / FPS;
        let mut out: RgbImage = if has_title && ot < TITLE_SECONDS {
            let fade = (ot / 0.4).min((TITLE_SECONDS - ot) / 0.4).min(1.0) as f32;
            card(w, h, &f, &edl.title, &edl.subtitle, fade.max(0.0))
        } else if has_end && ot >= total - END_SECONDS {
            let k = ot - (total - END_SECONDS);
            let fade = (k / 0.4).min((END_SECONDS - k) / 0.5).min(1.0) as f32;
            card(w, h, &f, &edl.end_text, "", fade.max(0.0))
        } else {
            let s = source_time(&edl, ot);
            let idx = frames.partition_point(|fr| fr.t <= s).saturating_sub(1);
            if cache.as_ref().is_none_or(|(c, _)| *c != idx) {
                let img = image::open(dir.join("raw").join(&frames[idx].file))
                    .map(|i| i.to_rgb8())
                    .unwrap_or_else(|_| {
                        cache
                            .as_ref()
                            .map(|c| c.1.clone())
                            .unwrap_or_else(|| first.clone())
                    });
                let img = if img.dimensions() == (sw, sh) {
                    img
                } else {
                    imageops::resize(&img, sw, sh, imageops::FilterType::Triangle)
                };
                cache = Some((idx, img));
            }
            let src = &cache.as_ref().expect("set above").1;
            let ratio = css_ratio(&frames[idx]);
            let (mx, my) = (sw as f64 / 2.0, sh as f64 / 2.0);
            let (scale, cx, cy) = match zoom_at(&edl, s) {
                // The view's center moves from the middle toward the target.
                Some((scale, wgt, tx, ty)) => (
                    scale,
                    mx + (tx * ratio - mx) * wgt,
                    my + (ty * ratio - my) * wgt,
                ),
                None => (1.0, mx, my),
            };
            let cw = (sw as f64 / scale).round().max(2.0);
            let ch = (sh as f64 / scale).round().max(2.0);
            let x0 = (cx - cw / 2.0).clamp(0.0, sw as f64 - cw);
            let y0 = (cy - ch / 2.0).clamp(0.0, sh as f64 - ch);
            let view =
                imageops::crop_imm(src, x0 as u32, y0 as u32, cw as u32, ch as u32).to_image();
            let mut img = if (cw as u32, ch as u32) == (w, h) {
                view
            } else {
                imageops::resize(&view, w, h, imageops::FilterType::Triangle)
            };
            let k = w as f64 / cw;
            let to_out = |px: f64, py: f64| ((px * ratio - x0) * k, (py * ratio - y0) * k);
            if edl.cursor {
                for c in &clicks {
                    let p = (s - c.t) / 0.5;
                    if (0.0..1.0).contains(&p) {
                        let (ox, oy) = to_out(c.x.unwrap_or(0.0), c.y.unwrap_or(0.0));
                        ripple(&mut img, ox, oy, p, 40.0 * (h as f64 / 720.0));
                    }
                }
                if let Some((px, py)) = cursor_at(&points, s) {
                    let (ox, oy) = to_out(px, py);
                    cursor(&mut img, ox, oy, 34.0 * (h as f64 / 720.0).max(0.6));
                }
            }
            if let Some(c) = edl.captions.iter().find(|c| s >= c.from && s <= c.to) {
                caption(&mut img, &f, &c.text);
            }
            img
        };
        if let Some(sin) = stdin.as_mut()
            && !write_failed
            && sin.write_all(out.as_raw()).is_err()
        {
            write_failed = true;
        }
        if i % GIF_EVERY == 0 && ot <= GIF_MAX_SECONDS {
            if (gw, gh) != (w, h) {
                out = imageops::resize(&out, gw, gh, imageops::FilterType::Triangle);
            }
            gif_frames.push(out.into_raw());
        }
        let pct = i * 100 / n.max(1);
        if pct >= last_pct + 25 {
            last_pct = pct;
            progress(format!("rendering… {pct}%"));
        }
    }
    drop(stdin);
    let mut note = music_note;
    let mp4 = match (child, ffmpeg) {
        (Some(c), _) => match c.wait_with_output() {
            Ok(o) if o.status.success() && !write_failed => Some(mp4_path),
            Ok(o) => {
                note = Some(format!(
                    "ffmpeg failed: {}",
                    String::from_utf8_lossy(&o.stderr)
                        .lines()
                        .last()
                        .unwrap_or("error")
                ));
                None
            }
            Err(e) => {
                note = Some(format!("ffmpeg: {e}"));
                None
            }
        },
        (None, Some(_)) => {
            note = Some("ffmpeg couldn't start".into());
            None
        }
        (None, None) => {
            note = Some("no ffmpeg, so no MP4 or music — `/video setup` downloads it (or install ffmpeg), then `/video render`".into());
            None
        }
    };
    let _ = std::fs::remove_file(&music_wav);

    progress("making the GIF…".into());
    let gif_path = dir.join("video.gif");
    {
        let file = std::fs::File::create(&gif_path).map_err(|e| e.to_string())?;
        let mut enc = gif::Encoder::new(std::io::BufWriter::new(file), gw as u16, gh as u16, &[])
            .map_err(|e| e.to_string())?;
        let _ = enc.set_repeat(gif::Repeat::Infinite);
        for rgb in &gif_frames {
            let mut fr = gif::Frame::from_rgb_speed(gw as u16, gh as u16, rgb, 20);
            fr.delay = (100.0 / (FPS / GIF_EVERY as f64)) as u16;
            enc.write_frame(&fr).map_err(|e| e.to_string())?;
        }
    }
    let srt_path = dir.join("captions.srt");
    std::fs::write(&srt_path, srt(&edl)).map_err(|e| e.to_string())?;
    Ok(Rendered {
        mp4,
        gif: gif_path,
        srt: srt_path,
        seconds: total,
        note,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::{Caption, Segment, Zoom};

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
        let edl = super::super::draft(&events, 4.0, "Test video");
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
        }
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
        caption(&mut img, &f, "Click “Create project” to start a new one");
        img.save(format!("{out}/zoom.png")).unwrap();
    }
}
