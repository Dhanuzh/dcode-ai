//! Re-recording one step: a short new recording of just that step replaces
//! the step's stretch of an old one — frames, events, private boxes and
//! content boxes — and everything after it moves by the difference in
//! length, edit list included.

use std::path::Path;

use super::record::{Focus, Privacy, Saved, load};
use super::{Edl, Event, FrameInfo, Segment};

/// The steps of a recording, numbered from 1 as `/video steps` lists them.
pub fn steps(events: &[Event]) -> Vec<&Event> {
    events.iter().collect()
}

/// Where step `n` (from 1) begins: the frame shown just before it (the
/// still taken with the element in view), else half a second before.
fn start_of(frames: &[FrameInfo], e: &Event) -> f64 {
    match frames.iter().rfind(|f| f.t <= e.t + 0.05) {
        Some(f) if e.t - f.t < 2.0 => f.t,
        _ => (e.t - 0.5).max(0.0),
    }
}

/// Step `n`'s stretch of the recording: `[from, to)`.
pub fn window(saved: &Saved, n: usize) -> Result<(f64, f64), String> {
    let steps = steps(&saved.events);
    if n == 0 || n > steps.len() {
        return Err(format!(
            "no step {n} — the recording has {} (/video steps lists them)",
            steps.len()
        ));
    }
    let from = start_of(&saved.frames, steps[n - 1]);
    let to = match steps.get(n) {
        Some(next) => start_of(&saved.frames, next).max(from + 0.1),
        None => saved.duration.max(from + 0.1),
    };
    Ok((from, to))
}

/// Times: before the window unchanged, inside it squeezed or stretched to
/// the new length, after it moved by the difference.
struct Map {
    from: f64,
    to: f64,
    new_len: f64,
}

impl Map {
    fn delta(&self) -> f64 {
        self.new_len - (self.to - self.from)
    }

    fn t(&self, t: f64) -> f64 {
        if t < self.from {
            t
        } else if t >= self.to {
            t + self.delta()
        } else {
            self.from + (t - self.from) * self.new_len / (self.to - self.from)
        }
    }

    fn inside(&self, t: f64) -> bool {
        t >= self.from && t < self.to
    }
}

/// Splices recording `new` in place of step `n` of recording `old`; `new`
/// is removed after. Returns a one-line summary.
pub fn splice(old: &Path, n: usize, new: &Path) -> Result<String, String> {
    let o = load(old)?;
    let x = load(new)?;
    let (from, to) = window(&o, n)?;
    // The new take: from its first frame to its end.
    let x0 = x.frames.first().map(|f| f.t).unwrap_or(0.0);
    let new_len = (x.duration - x0).max(0.5);
    let m = Map { from, to, new_len };
    let at_new = |t: f64| from + (t - x0).max(0.0);

    // Frames: the new take's files copied in under fresh names.
    let raw = old.join("raw");
    let tag = chrono::Local::now().format("%H%M%S");
    let mut frames: Vec<FrameInfo> = o
        .frames
        .iter()
        .filter(|f| !m.inside(f.t))
        .map(|f| FrameInfo {
            t: m.t(f.t),
            ..f.clone()
        })
        .collect();
    for (i, f) in x.frames.iter().enumerate() {
        let file = format!("r{n}-{tag}-{:05}.jpg", i + 1);
        std::fs::copy(new.join("raw").join(&f.file), raw.join(&file))
            .map_err(|e| format!("copying the new take: {e}"))?;
        frames.push(FrameInfo {
            t: at_new(f.t),
            file,
            css_w: f.css_w,
        });
    }
    frames.sort_by(|a, b| a.t.total_cmp(&b.t));

    let mut events: Vec<Event> = o
        .events
        .iter()
        .filter(|e| !m.inside(e.t))
        .map(|e| Event {
            t: m.t(e.t),
            ..e.clone()
        })
        .chain(x.events.iter().map(|e| Event {
            t: at_new(e.t),
            ..e.clone()
        }))
        .collect();
    events.sort_by(|a, b| a.t.total_cmp(&b.t));

    let mut privacy: Vec<Privacy> = o
        .privacy
        .iter()
        .filter(|p| !m.inside(p.t))
        .map(|p| Privacy {
            t: m.t(p.t),
            boxes: p.boxes.clone(),
        })
        .chain(x.privacy.iter().map(|p| Privacy {
            t: at_new(p.t),
            boxes: p.boxes.clone(),
        }))
        .collect();
    privacy.sort_by(|a, b| a.t.total_cmp(&b.t));

    let mut focus: Vec<Focus> = o
        .focus
        .iter()
        .filter(|f| !m.inside(f.t))
        .map(|f| Focus {
            t: m.t(f.t),
            b: f.b,
        })
        .chain(x.focus.iter().map(|f| Focus {
            t: at_new(f.t),
            b: f.b,
        }))
        .collect();
    focus.sort_by(|a, b| a.t.total_cmp(&b.t));

    let edl = shift_edl(&o.edl, &m, &x.edl, x0);
    let duration = o.duration + m.delta();

    let write = |name: &str, v: String| {
        std::fs::write(old.join(name), v).map_err(|e| format!("{name}: {e}"))
    };
    let json = |v: &dyn erased::Json| v.json();
    write("frames.json", json(&frames))?;
    write("events.json", json(&events))?;
    write("privacy.json", json(&privacy))?;
    write("focus.json", json(&focus))?;
    write("edl.json", json(&edl))?;
    let mut meta: serde_json::Value = std::fs::read_to_string(old.join("meta.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| serde_json::json!({}));
    meta["duration"] = serde_json::json!(duration);
    write("meta.json", meta.to_string())?;
    let _ = std::fs::remove_dir_all(new);
    Ok(format!(
        "step {n} re-recorded: {:.1}s → {:.1}s ({} frames)",
        to - from,
        new_len,
        x.frames.len()
    ))
}

mod erased {
    pub trait Json {
        fn json(&self) -> String;
    }
    impl<T: serde::Serialize> Json for T {
        fn json(&self) -> String {
            serde_json::to_string_pretty(self).unwrap_or_default()
        }
    }
}

/// The old edit list with its times moved; zooms in the step come from the
/// new take's draft.
fn shift_edl(old: &Edl, m: &Map, new: &Edl, x0: f64) -> Edl {
    let mut e = old.clone();
    for l in &mut e.lines {
        l.at = m.t(l.at);
    }
    for i in &mut e.inserts {
        i.at = m.t(i.at);
    }
    for c in &mut e.captions {
        c.from = m.t(c.from);
        c.to = m.t(c.to).max(c.from);
    }
    for b in &mut e.blur {
        b.from = m.t(b.from);
        b.to = m.t(b.to).max(b.from);
    }
    e.zooms.retain(|z| !m.inside(z.at));
    for z in &mut e.zooms {
        z.at = m.t(z.at);
    }
    e.zooms.extend(new.zooms.iter().map(|z| super::Zoom {
        at: m.from + (z.at - x0).max(0.0),
        ..z.clone()
    }));
    e.zooms.sort_by(|a, b| a.at.total_cmp(&b.at));
    if !e.segments.is_empty() {
        let mut segs: Vec<Segment> = Vec::new();
        for s in &e.segments {
            // The part before the step, and the part after it (moved).
            if s.from < m.from {
                segs.push(Segment {
                    to: s.to.min(m.from),
                    ..s.clone()
                });
            }
            if s.to > m.to {
                segs.push(Segment {
                    from: m.t(s.from.max(m.to)),
                    to: m.t(s.to),
                    speed: s.speed,
                });
            }
        }
        // The new take, at normal speed.
        segs.push(Segment {
            from: m.from,
            to: m.from + m.new_len,
            speed: 1.0,
        });
        segs.sort_by(|a, b| a.from.total_cmp(&b.from));
        e.segments = segs;
    }
    e
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(t: f64, label: &str) -> Event {
        Event {
            t,
            kind: "click".into(),
            x: Some(10.0),
            y: Some(10.0),
            w: Some(5.0),
            h: Some(5.0),
            label: label.into(),
        }
    }

    fn save(dir: &Path, frames: &[f64], events: Vec<Event>, duration: f64, edl: &Edl) {
        std::fs::create_dir_all(dir.join("raw")).unwrap();
        let fr: Vec<FrameInfo> = frames
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let file = format!("f{:06}.jpg", i + 1);
                std::fs::write(dir.join("raw").join(&file), format!("{t}")).unwrap();
                FrameInfo {
                    t: *t,
                    file,
                    css_w: 100.0,
                }
            })
            .collect();
        std::fs::write(dir.join("frames.json"), serde_json::to_string(&fr).unwrap()).unwrap();
        std::fs::write(
            dir.join("events.json"),
            serde_json::to_string(&events).unwrap(),
        )
        .unwrap();
        std::fs::write(dir.join("edl.json"), serde_json::to_string(edl).unwrap()).unwrap();
        std::fs::write(
            dir.join("meta.json"),
            serde_json::json!({"duration": duration, "mode": "stills"}).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn a_step_is_replaced_and_later_times_move() {
        let d = tempfile::tempdir().unwrap();
        let (old, new) = (d.path().join("old"), d.path().join("new"));
        let mut edl = Edl::default();
        edl.lines.push(super::super::Line {
            id: "late".into(),
            at: 9.0,
            text: "after".into(),
            section: String::new(),
            only: Vec::new(),
        });
        edl.segments = vec![Segment {
            from: 0.0,
            to: 12.0,
            speed: 1.0,
        }];
        // Steps at 2, 5 (bad), 8; stills just before each.
        save(
            &old,
            &[0.0, 1.8, 4.7, 7.6, 8.5],
            vec![ev(2.0, "a"), ev(5.0, "bad"), ev(8.0, "c")],
            12.0,
            &edl,
        );
        // The new take of step 2: 1 s long instead of 2.9 s.
        save(
            &new,
            &[0.0, 0.4],
            vec![ev(0.3, "good")],
            1.0,
            &Edl::default(),
        );
        let msg = splice(&old, 2, &new).unwrap();
        assert!(msg.contains("step 2"), "{msg}");
        let s = load(&old).unwrap();
        let labels: Vec<&str> = s.events.iter().map(|e| e.label.as_str()).collect();
        assert_eq!(labels, ["a", "good", "c"]);
        // Window was [4.7, 7.6): 2.9 s became 1 s, so later times move −1.9.
        let c = &s.events[2];
        assert!((c.t - 6.1).abs() < 1e-9, "{c:?}");
        assert!((s.duration - 10.1).abs() < 1e-9);
        assert!((s.edl.lines[0].at - 7.1).abs() < 1e-9);
        assert!(!new.exists());
        // The new frames are in, the old step's frame is gone.
        assert_eq!(s.frames.len(), 6);
        assert!(
            s.frames
                .iter()
                .all(|f| old.join("raw").join(&f.file).is_file())
        );
        assert!(s.frames.windows(2).all(|w| w[0].t <= w[1].t));
        // Segments cover the new take.
        assert!(s.edl.segments.iter().any(|g| (g.from - 4.7).abs() < 1e-9));
    }

    #[test]
    fn a_missing_step_is_explained() {
        let d = tempfile::tempdir().unwrap();
        let old = d.path().join("old");
        save(&old, &[0.0], vec![ev(1.0, "a")], 3.0, &Edl::default());
        let s = load(&old).unwrap();
        assert!(window(&s, 4).unwrap_err().contains("has 1"));
    }
}
