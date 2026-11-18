//! A first edit for recordings with no clicks to go by (whole-screen
//! recordings): where the picture changes is where something happens.
//! Still stretches are sped up or cut, the same way browser recordings are
//! trimmed around their actions, and a burst of change in a small part of
//! the screen gets a zoom on it.

use std::path::Path;

use super::{Edl, Event, FrameInfo, Zoom};

/// Thumbnail size the frames are compared at.
const TW: u32 = 160;
const TH: u32 = 90;
/// Frames looked at per second of recording (more add nothing but time).
const SAMPLE_FPS: f64 = 4.0;

/// Something changed on screen at `t`: the share of the picture that did
/// (0–1) and where (source pixels: x, y, w, h).
#[derive(Debug, Clone, PartialEq)]
pub struct Change {
    pub t: f64,
    pub frac: f64,
    pub bbox: [f64; 4],
}

/// The changes between the recording's frames (at most [`SAMPLE_FPS`] a
/// second).
pub fn changes(dir: &Path, frames: &[FrameInfo]) -> Vec<Change> {
    let mut out = Vec::new();
    let mut prev: Option<image::GrayImage> = None;
    let mut last_t = f64::NEG_INFINITY;
    for f in frames {
        if f.t - last_t < 1.0 / SAMPLE_FPS {
            continue;
        }
        last_t = f.t;
        let Ok(img) = image::open(dir.join("raw").join(&f.file)) else {
            continue;
        };
        let size = (img.width(), img.height());
        let g = img.thumbnail_exact(TW, TH).to_luma8();
        if let Some(p) = &prev {
            let (mut n, mut x0, mut y0, mut x1, mut y1) = (0u32, TW, TH, 0u32, 0u32);
            for (x, y, px) in g.enumerate_pixels() {
                if (px.0[0] as i32 - p.get_pixel(x, y).0[0] as i32).abs() > 24 {
                    n += 1;
                    x0 = x0.min(x);
                    y0 = y0.min(y);
                    x1 = x1.max(x);
                    y1 = y1.max(y);
                }
            }
            if n > 0 {
                let (sx, sy) = (size.0 as f64 / TW as f64, size.1 as f64 / TH as f64);
                out.push(Change {
                    t: f.t,
                    frac: n as f64 / (TW * TH) as f64,
                    bbox: [
                        x0 as f64 * sx,
                        y0 as f64 * sy,
                        (x1 - x0 + 1) as f64 * sx,
                        (y1 - y0 + 1) as f64 * sy,
                    ],
                });
            }
        }
        prev = Some(g);
    }
    out
}

/// Changes worth calling activity (a blinking caret or a clock isn't).
fn active(c: &Change) -> bool {
    c.frac > 0.002
}

/// A first edit from the changes: segments that keep the busy stretches
/// and speed up or cut the still ones, and zooms on small busy areas.
/// `size`: the frames' size in pixels.
pub fn apply(
    edl: &mut Edl,
    events: &[Event],
    changes: &[Change],
    duration: f64,
    size: (u32, u32),
) -> (usize, usize) {
    // Activity as events, for the same trimming rules browser recordings
    // get (real events — notes the agent made — count too).
    let mut acts: Vec<Event> = changes
        .iter()
        .filter(|c| active(c))
        .map(|c| Event {
            t: c.t,
            kind: "change".into(),
            x: None,
            y: None,
            w: None,
            h: None,
            label: String::new(),
        })
        .collect();
    acts.extend(events.iter().filter(|e| e.kind != "note").cloned());
    acts.sort_by(|a, b| a.t.total_cmp(&b.t));
    if !acts.is_empty() {
        edl.segments = super::draft(&acts, duration, "").segments;
    }
    // Zooms: bursts of change (gaps under a second) confined to a small
    // part of the screen.
    let area = size.0 as f64 * size.1 as f64;
    let mut bursts: Vec<(f64, f64, [f64; 4])> = Vec::new();
    for c in changes.iter().filter(|c| active(c)) {
        match bursts.last_mut() {
            Some(b) if c.t - b.1 < 1.0 => {
                b.1 = c.t;
                let [x, y, w, h] = b.2;
                let (x0, y0) = (x.min(c.bbox[0]), y.min(c.bbox[1]));
                let (x1, y1) = (
                    (x + w).max(c.bbox[0] + c.bbox[2]),
                    (y + h).max(c.bbox[1] + c.bbox[3]),
                );
                b.2 = [x0, y0, x1 - x0, y1 - y0];
            }
            _ => bursts.push((c.t, c.t, c.bbox)),
        }
    }
    let mut zooms = 0;
    if area > 0.0 {
        for (a, b, [x, y, w, h]) in bursts {
            let small = w * h < area / 3.0;
            let long = b - a >= 0.5;
            let spaced = edl.zooms.last().is_none_or(|z| a - z.at > 3.0);
            if !(small && long && spaced) {
                continue;
            }
            // Fit the busy area with room around it, 1.3–2×.
            let scale = (size.0 as f64 / (w * 1.4))
                .min(size.1 as f64 / (h * 1.4))
                .clamp(1.3, 2.0);
            edl.zooms.push(Zoom {
                at: (a - 0.3).max(0.0),
                x: x + w / 2.0,
                y: y + h / 2.0,
                scale,
                hold: (b - a).clamp(1.0, 4.0),
                anchor: String::new(),
            });
            zooms += 1;
        }
    }
    (edl.segments.len(), zooms)
}

/// Runs the automatic edit on a saved recording, replacing its segments
/// and zooms; returns a one-line summary.
pub fn run(dir: &Path) -> Result<String, String> {
    let s = super::record::load(dir)?;
    let size = s
        .frames
        .first()
        .and_then(|f| image::image_dimensions(dir.join("raw").join(&f.file)).ok())
        .ok_or("no frames")?;
    let ch = changes(dir, &s.frames);
    let mut edl = s.edl.clone();
    edl.zooms.clear();
    // Narration already written keeps its footage: each line counts as
    // activity, so nothing is said over a cut.
    let mut events = s.events.clone();
    events.extend(edl.lines.iter().map(|l| Event {
        t: l.at + 0.8,
        kind: "line".into(),
        x: None,
        y: None,
        w: None,
        h: None,
        label: String::new(),
    }));
    let (segs, zooms) = apply(&mut edl, &events, &ch, s.duration, size);
    let edl = edl.normalized(s.duration);
    let kept: f64 = edl
        .segments
        .iter()
        .map(|g| (g.to - g.from) / g.speed.max(0.1))
        .sum();
    std::fs::write(
        dir.join("edl.json"),
        serde_json::to_string_pretty(&edl).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    Ok(format!(
        "{segs} segments, {zooms} zooms from where the screen changes — {:.0} s of footage became {:.0} s",
        s.duration, kept
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn change(t: f64, frac: f64, bbox: [f64; 4]) -> Change {
        Change { t, frac, bbox }
    }

    #[test]
    fn still_stretches_are_cut_and_small_busy_areas_zoomed() {
        let mut edl = Edl::default();
        // Typing in a small box at 2–4 s, nothing for 20 s, a big change
        // (a new window) at 25 s.
        let mut ch: Vec<Change> = (0..9)
            .map(|i| change(2.0 + i as f64 * 0.25, 0.01, [100.0, 100.0, 200.0, 40.0]))
            .collect();
        ch.push(change(25.0, 0.6, [0.0, 0.0, 1920.0, 1080.0]));
        let (_, zooms) = apply(&mut edl, &[], &ch, 30.0, (1920, 1080));
        assert_eq!(zooms, 1);
        let z = &edl.zooms[0];
        assert!(
            (z.x - 200.0).abs() < 1.0 && (z.y - 120.0).abs() < 1.0,
            "{z:?}"
        );
        assert!(z.scale > 1.3);
        // The 20 s of nothing is cut down: kept footage well under 30 s.
        let kept: f64 = edl.segments.iter().map(|s| (s.to - s.from) / s.speed).sum();
        assert!(kept < 12.0, "{:?}", edl.segments);
        // A blinking caret alone is not activity.
        let mut quiet = Edl::default();
        let caret = vec![change(5.0, 0.0005, [0.0, 0.0, 2.0, 10.0])];
        let (_, z) = apply(&mut quiet, &[], &caret, 10.0, (1920, 1080));
        assert_eq!(z, 0);
        assert!(quiet.segments.is_empty());
    }

    #[test]
    fn frames_that_differ_are_found() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("raw")).unwrap();
        let a = image::RgbImage::from_pixel(320, 180, image::Rgb([20, 20, 20]));
        let mut b = a.clone();
        for x in 0..80 {
            for y in 0..40 {
                b.put_pixel(x, y, image::Rgb([240, 240, 240]));
            }
        }
        a.save(d.path().join("raw/f1.jpg")).unwrap();
        b.save(d.path().join("raw/f2.jpg")).unwrap();
        let frames = vec![
            FrameInfo {
                t: 0.0,
                file: "f1.jpg".into(),
                css_w: 0.0,
            },
            FrameInfo {
                t: 1.0,
                file: "f2.jpg".into(),
                css_w: 0.0,
            },
        ];
        let ch = changes(d.path(), &frames);
        assert_eq!(ch.len(), 1);
        assert!(ch[0].frac > 0.04 && ch[0].frac < 0.1, "{:?}", ch[0]);
        assert!(
            ch[0].bbox[0] < 4.0 && ch[0].bbox[2] > 70.0 && ch[0].bbox[2] < 90.0,
            "{:?}",
            ch[0]
        );
    }
}

/// Runs the automatic edit, then the report and the timeline, on the
/// recording in `$DCODE_AI_RENDER_DIR` (a copy of a real one).
#[cfg(test)]
mod live_folder {
    #[test]
    #[ignore]
    fn autoedit_a_folder() {
        let Ok(dir) = std::env::var("DCODE_AI_RENDER_DIR") else {
            return;
        };
        let dir = std::path::Path::new(&dir);
        let t = std::time::Instant::now();
        eprintln!("{} ({:.1?})", super::run(dir).unwrap(), t.elapsed());
        for l in crate::video::render::check(dir).unwrap() {
            eprintln!("check: {l}");
        }
        for l in crate::video::render::timeline_view(dir, 110)
            .unwrap()
            .iter()
            .take(6)
        {
            eprintln!("{l}");
        }
    }
}
