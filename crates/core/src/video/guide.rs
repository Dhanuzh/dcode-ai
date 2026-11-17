//! A step-by-step how-to guide from a recording: the narration as the text,
//! a still per step with the element that was used outlined — as Markdown
//! (`guide.md` + `guide/step-N.jpg`) and one self-contained HTML page
//! (`guide.html`, pictures inline).

use std::path::{Path, PathBuf};

use base64::Engine;
use image::RgbImage;

use super::record::{Saved, load};
use super::{Event, FrameInfo};

/// One step of the guide.
struct Step {
    section: String,
    text: String,
    picture: Option<RgbImage>,
}

/// The frame showing the moment `t`: the last one at or before it.
fn frame_at(frames: &[FrameInfo], t: f64) -> Option<&FrameInfo> {
    frames.iter().rfind(|f| f.t <= t + 0.05).or(frames.first())
}

/// The action a line talks about: the first click or typing from shortly
/// before the line to before the next line.
fn action_for(events: &[Event], from: f64, to: f64) -> Option<&Event> {
    events
        .iter()
        .find(|e| matches!(e.kind.as_str(), "click" | "type") && e.t >= from - 0.5 && e.t < to)
}

/// Draws a rounded outline around an element (CSS pixels × `ratio`).
fn outline(img: &mut RgbImage, e: &Event, ratio: f64) {
    let (Some(cx), Some(cy)) = (e.x, e.y) else {
        return;
    };
    let (w, h) = (e.w.unwrap_or(40.0), e.h.unwrap_or(24.0));
    let pad = 6.0;
    let x0 = ((cx - w / 2.0 - pad) * ratio).max(0.0) as i64;
    let y0 = ((cy - h / 2.0 - pad) * ratio).max(0.0) as i64;
    let x1 = (((cx + w / 2.0 + pad) * ratio) as i64).min(img.width() as i64 - 1);
    let y1 = (((cy + h / 2.0 + pad) * ratio) as i64).min(img.height() as i64 - 1);
    let color = image::Rgb([230, 57, 70]);
    let t = (img.width() as f64 / 640.0).ceil().max(2.0) as i64;
    for k in 0..t {
        for x in x0..=x1 {
            for y in [y0 + k, y1 - k] {
                if y >= 0 && y < img.height() as i64 {
                    img.put_pixel(x as u32, y as u32, color);
                }
            }
        }
        for y in y0..=y1 {
            for x in [x0 + k, x1 - k] {
                if x >= 0 && x < img.width() as i64 {
                    img.put_pixel(x as u32, y as u32, color);
                }
            }
        }
    }
}

fn steps(s: &Saved) -> Vec<Step> {
    let edl = s.edl.clone().normalized(s.duration);
    let mut out = Vec::new();
    for (i, l) in edl.lines.iter().enumerate() {
        let next = edl.lines.get(i + 1).map(|n| n.at).unwrap_or(s.duration);
        let action = action_for(&s.events, l.at, next);
        // The still just before the action (the element in view), else the
        // page a moment after the line starts.
        let at = action.map(|a| a.t).unwrap_or(l.at + 1.0);
        let picture = frame_at(&s.frames, at).and_then(|f| {
            let mut img = image::open(s.dir.join("raw").join(&f.file)).ok()?.to_rgb8();
            if let Some(a) = action {
                let ratio = if f.css_w > 0.0 {
                    img.width() as f64 / f.css_w
                } else {
                    1.0
                };
                outline(&mut img, a, ratio);
            }
            // Guides don't need full size.
            if img.width() > 1280 {
                let h = img.height() * 1280 / img.width();
                img = image::imageops::resize(&img, 1280, h, image::imageops::FilterType::Triangle);
            }
            Some(img)
        });
        out.push(Step {
            section: l.section.clone(),
            text: l.text.clone(),
            picture,
        });
    }
    out
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Writes `guide.md`, `guide/step-N.jpg` and `guide.html`; returns their
/// paths.
pub fn write(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let s = load(dir)?;
    if s.edl.lines.is_empty() {
        return Err("no narration lines to turn into steps — /video edit to add narration".into());
    }
    let title = if s.edl.title.trim().is_empty() {
        "How-to".to_string()
    } else {
        s.edl.title.trim().to_string()
    };
    let steps = steps(&s);
    let pics = dir.join("guide");
    std::fs::create_dir_all(&pics).map_err(|e| e.to_string())?;
    let mut md = format!("# {title}\n\n");
    if !s.edl.subtitle.trim().is_empty() {
        md.push_str(&format!("{}\n\n", s.edl.subtitle.trim()));
    }
    let mut html = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{t}</title><style>\
:root{{--bg:#fafafa;--fg:#1d1d1f;--muted:#6e6e73;--card:#fff;--line:#e5e5ea}}\
@media (prefers-color-scheme:dark){{:root{{--bg:#111;--fg:#f2f2f7;--muted:#a1a1a6;--card:#1c1c1e;--line:#2c2c2e}}}}\
body{{margin:0;background:var(--bg);color:var(--fg);font:16px/1.55 system-ui,-apple-system,Segoe UI,sans-serif}}\
main{{max-width:880px;margin:0 auto;padding:32px 16px 64px}}h1{{font-size:30px;margin:0 0 6px}}\
.sub{{color:var(--muted);margin:0 0 28px}}h2{{font-size:20px;margin:36px 0 12px}}\
.step{{background:var(--card);border:1px solid var(--line);border-radius:12px;padding:16px;margin:0 0 16px}}\
.n{{display:inline-block;min-width:26px;height:26px;border-radius:13px;background:#5b4bdb;color:#fff;text-align:center;font-weight:600;margin-right:8px}}\
img{{width:100%;height:auto;border-radius:8px;margin-top:12px;border:1px solid var(--line)}}\
</style></head><body><main><h1>{t}</h1><p class=\"sub\">{sub}</p>",
        t = esc(&title),
        sub = esc(s.edl.subtitle.trim())
    );
    let mut section = String::new();
    let mut files = Vec::new();
    for (i, st) in steps.iter().enumerate() {
        let n = i + 1;
        if !st.section.is_empty() && st.section != section {
            section = st.section.clone();
            md.push_str(&format!("## {section}\n\n"));
            html.push_str(&format!("<h2>{}</h2>", esc(&section)));
        }
        md.push_str(&format!("{n}. {}\n", st.text.trim()));
        html.push_str(&format!(
            "<div class=\"step\"><span class=\"n\">{n}</span>{}",
            esc(st.text.trim())
        ));
        if let Some(img) = &st.picture {
            let name = format!("step-{n}.jpg");
            let p = pics.join(&name);
            let mut bytes = Vec::new();
            image::DynamicImage::ImageRgb8(img.clone())
                .write_to(
                    &mut std::io::Cursor::new(&mut bytes),
                    image::ImageFormat::Jpeg,
                )
                .map_err(|e| e.to_string())?;
            std::fs::write(&p, &bytes).map_err(|e| e.to_string())?;
            md.push_str(&format!("\n   ![Step {n}](guide/{name})\n"));
            html.push_str(&format!(
                "<img alt=\"Step {n}\" src=\"data:image/jpeg;base64,{}\">",
                base64::engine::general_purpose::STANDARD.encode(&bytes)
            ));
            files.push(p);
        }
        md.push('\n');
        html.push_str("</div>");
    }
    if !s.edl.end_text.trim().is_empty() {
        md.push_str(&format!("{}\n", s.edl.end_text.trim()));
        html.push_str(&format!("<p>{}</p>", esc(s.edl.end_text.trim())));
    }
    html.push_str("</main></body></html>\n");
    let (mdp, htmlp) = (dir.join("guide.md"), dir.join("guide.html"));
    std::fs::write(&mdp, md).map_err(|e| e.to_string())?;
    std::fs::write(&htmlp, html).map_err(|e| e.to_string())?;
    let mut all = vec![mdp, htmlp];
    all.extend(files);
    Ok(all)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::{Edl, Line};

    #[test]
    fn a_guide_has_a_step_per_line_with_its_picture() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path();
        std::fs::create_dir_all(dir.join("raw")).unwrap();
        for i in 1..=2 {
            RgbImage::from_pixel(200, 100, image::Rgb([200, 200, 200]))
                .save(dir.join("raw").join(format!("f{i:06}.jpg")))
                .unwrap();
        }
        let frames = vec![
            FrameInfo {
                t: 0.0,
                file: "f000001.jpg".into(),
                css_w: 200.0,
            },
            FrameInfo {
                t: 2.0,
                file: "f000002.jpg".into(),
                css_w: 200.0,
            },
        ];
        let events = vec![Event {
            t: 2.2,
            kind: "click".into(),
            x: Some(50.0),
            y: Some(50.0),
            w: Some(40.0),
            h: Some(20.0),
            label: "button".into(),
        }];
        let line = |id: &str, at: f64, text: &str| Line {
            id: id.into(),
            at,
            text: text.into(),
            section: "Start".into(),
            only: vec![],
        };
        let edl = Edl {
            title: "Make <a> project".into(),
            lines: vec![
                line("a", 0.5, "Open the page."),
                line("b", 1.8, "Click Create."),
            ],
            ..Default::default()
        };
        std::fs::write(
            dir.join("frames.json"),
            serde_json::to_string(&frames).unwrap(),
        )
        .unwrap();
        std::fs::write(
            dir.join("events.json"),
            serde_json::to_string(&events).unwrap(),
        )
        .unwrap();
        std::fs::write(dir.join("edl.json"), serde_json::to_string(&edl).unwrap()).unwrap();
        std::fs::write(dir.join("meta.json"), r#"{"duration":4.0}"#).unwrap();
        let files = write(dir).unwrap();
        let md = std::fs::read_to_string(dir.join("guide.md")).unwrap();
        assert!(md.contains("## Start"), "{md}");
        assert!(md.contains("1. Open the page."));
        assert!(md.contains("2. Click Create."));
        assert!(md.contains("guide/step-2.jpg"));
        let html = std::fs::read_to_string(dir.join("guide.html")).unwrap();
        assert!(html.contains("Make &lt;a&gt; project"));
        assert!(html.contains("data:image/jpeg;base64,"));
        assert_eq!(files.len(), 4);
        // Step 2's still has the clicked element outlined in red.
        let img = image::open(dir.join("guide/step-2.jpg")).unwrap().to_rgb8();
        let p = img.get_pixel(24, 50).0;
        assert!(p[0] > 180 && p[1] < 120, "{p:?}");
    }
}
