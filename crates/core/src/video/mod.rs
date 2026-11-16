//! AI screen recordings: the agent records the browser (or a desktop app)
//! while it works, then edits the recording itself — dead time sped up, zoom
//! toward what it clicks, a cursor, captions, title and end cards, background
//! music — into an MP4 (with `ffmpeg`) and a GIF.
//!
//! - [`record`]: frames from the page's screencast plus an **event track** of
//!   every action the agent took (what, where, when) — the editor plans from
//!   it instead of watching the video.
//! - [`Edl`]: the edit list — plain JSON the agent writes and rewrites
//!   ("under 45 s, bigger captions") without recording again.
//! - [`render`]: draws the edit frame by frame in Rust.

pub mod music;
pub mod publish;
pub mod record;
pub mod render;
pub mod timeline;
pub mod voice;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where a project's videos live.
pub fn videos_dir(workspace: &Path) -> PathBuf {
    workspace.join(".dcode-ai").join("videos")
}

/// One captured frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameInfo {
    /// Seconds from the start of the recording.
    pub t: f64,
    pub file: String,
    /// The page's width in CSS pixels when it was taken (to place clicks).
    #[serde(default)]
    pub css_w: f64,
}

/// One thing the agent did while recording.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Event {
    /// Seconds from the start of the recording.
    pub t: f64,
    /// `click`, `type`, `navigate`, `press`, `scroll`, `wait`, `note`.
    pub kind: String,
    /// Where (CSS pixels), for clicks and typing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub y: Option<f64>,
    /// What it acted on or a short description ("Create project" button).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}

/// A kept stretch of the recording, at a speed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Segment {
    pub from: f64,
    pub to: f64,
    #[serde(default = "one")]
    pub speed: f64,
}

fn one() -> f64 {
    1.0
}

/// Zoom toward a point (CSS pixels) around a moment.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Zoom {
    pub at: f64,
    pub x: f64,
    pub y: f64,
    #[serde(default = "default_scale")]
    pub scale: f64,
    /// Seconds held at full zoom after `at`.
    #[serde(default = "default_hold")]
    pub hold: f64,
}

fn default_scale() -> f64 {
    1.6
}

fn default_hold() -> f64 {
    1.2
}

/// A caption shown from `from` to `to` (recording seconds).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Caption {
    pub from: f64,
    pub to: f64,
    pub text: String,
}

/// One line of narration: said from recording second `at`. The `id` is
/// permanent — your recorded take is filed under it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Line {
    pub id: String,
    pub at: f64,
    pub text: String,
    /// Chapter this line belongs to (its first line starts the chapter).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub section: String,
    /// Only in these cuts (`long`, `short`); empty = every cut.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub only: Vec<String>,
}

impl Line {
    pub fn in_cut(&self, cut: &str) -> bool {
        self.only.is_empty() || self.only.iter().any(|c| c == cut)
    }
}

/// Something placed into the video at recording second `at`, for
/// `seconds`: a card (`section`, `bullets`, `quote`) or a `hold` on the
/// frame there.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Insert {
    pub at: f64,
    #[serde(default = "default_insert_seconds")]
    pub seconds: f64,
    #[serde(default = "hold_kind")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<String>,
}

fn default_insert_seconds() -> f64 {
    3.0
}

fn hold_kind() -> String {
    "hold".into()
}

/// The edit list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct Edl {
    #[serde(default)]
    pub title: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub subtitle: String,
    #[serde(default)]
    pub end_text: String,
    #[serde(default)]
    pub segments: Vec<Segment>,
    #[serde(default)]
    pub zooms: Vec<Zoom>,
    #[serde(default)]
    pub captions: Vec<Caption>,
    /// `auto` (generated, on by default), `none`, or a path to an audio file.
    #[serde(default = "auto_music")]
    pub music: String,
    /// Draw the cursor and click ripples.
    #[serde(default = "yes")]
    pub cursor: bool,
    /// Output width in pixels (height follows).
    #[serde(default = "default_width")]
    pub width: u32,
    /// The narration script, in order. When there are lines, captions
    /// follow what is said and `captions` is not used.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lines: Vec<Line>,
    /// Cards and holds placed into the video.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inserts: Vec<Insert>,
    /// `long` (16:9) and/or `short` (9:16).
    #[serde(default = "default_cuts")]
    pub cuts: Vec<String>,
    /// `auto` (your take when recorded, else the draft voice), `draft`,
    /// `takes`, `none`.
    #[serde(default = "auto_music")]
    pub voice: String,
    /// The draft voice: `sapi[:name]`, `say[:name]`, `piper[:model.onnx]`,
    /// `espeak[:voice]`; empty = the first one available.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub draft_voice: String,
    /// Also write the long cut without burned-in captions.
    #[serde(default = "yes")]
    pub clean_master: bool,
    /// Upload folders `/video publish` makes.
    #[serde(default = "default_platforms")]
    pub platforms: Vec<String>,
}

fn default_cuts() -> Vec<String> {
    vec!["long".into()]
}

pub fn default_platforms() -> Vec<String> {
    [
        "youtube",
        "youtube-shorts",
        "instagram-reels",
        "whatsapp-status",
    ]
    .map(String::from)
    .to_vec()
}

/// A safe file-name id: lowercase letters, digits, hyphens.
pub fn clean_id(s: &str) -> String {
    let mut out = String::new();
    for c in s.trim().chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').chars().take(48).collect()
}

fn auto_music() -> String {
    "auto".into()
}

fn yes() -> bool {
    true
}

fn default_width() -> u32 {
    1280
}

/// Title and end cards, seconds.
pub const TITLE_SECONDS: f64 = 2.5;
pub const END_SECONDS: f64 = 2.0;

impl Edl {
    /// Fixes what can't be rendered: segments sorted, inside the recording,
    /// positive speeds; zoom scales 1–3; captions inside the recording.
    pub fn normalized(mut self, duration: f64) -> Self {
        let d = duration.max(0.0);
        self.segments.retain(|s| s.to > s.from);
        for s in &mut self.segments {
            s.from = s.from.clamp(0.0, d);
            s.to = s.to.clamp(0.0, d);
            if !(s.speed.is_finite() && s.speed > 0.0) {
                s.speed = 1.0;
            }
            s.speed = s.speed.clamp(0.25, 16.0);
        }
        self.segments.retain(|s| s.to - s.from > 0.01);
        self.segments.sort_by(|a, b| a.from.total_cmp(&b.from));
        if self.segments.is_empty() && d > 0.0 {
            self.segments.push(Segment {
                from: 0.0,
                to: d,
                speed: 1.0,
            });
        }
        for z in &mut self.zooms {
            z.scale = z.scale.clamp(1.0, 3.0);
            z.hold = z.hold.clamp(0.0, 10.0);
        }
        self.captions
            .retain(|c| c.to > c.from && !c.text.trim().is_empty());
        self.width = self.width.clamp(320, 1920) / 2 * 2;
        self.lines.retain(|l| !l.text.trim().is_empty());
        let mut seen = std::collections::HashSet::new();
        for (n, l) in self.lines.iter_mut().enumerate() {
            l.at = if l.at.is_finite() {
                l.at.clamp(0.0, d)
            } else {
                0.0
            };
            let mut id = clean_id(&l.id);
            if id.is_empty() {
                id = format!("line-{}", n + 1);
            }
            let base = id.clone();
            let mut k = 2;
            while !seen.insert(id.clone()) {
                id = format!("{base}-{k}");
                k += 1;
            }
            l.id = id;
        }
        self.lines.sort_by(|a, b| a.at.total_cmp(&b.at));
        // Lines a moment apart at least, so each can wait for the one before.
        for i in 1..self.lines.len() {
            let prev = self.lines[i - 1].at;
            if self.lines[i].at < prev + 0.05 {
                self.lines[i].at = prev + 0.05;
            }
        }
        for i in &mut self.inserts {
            i.at = if i.at.is_finite() {
                i.at.clamp(0.0, d)
            } else {
                0.0
            };
            i.seconds = if i.seconds.is_finite() {
                i.seconds.clamp(0.3, 20.0)
            } else {
                3.0
            };
            i.kind = i.kind.trim().to_ascii_lowercase();
            if !matches!(i.kind.as_str(), "section" | "bullets" | "quote" | "hold") {
                i.kind = "hold".into();
            }
        }
        self.inserts.sort_by(|a, b| a.at.total_cmp(&b.at));
        self.cuts.retain(|c| c == "long" || c == "short");
        self.cuts.dedup();
        if self.cuts.is_empty() {
            self.cuts = default_cuts();
        }
        let v = self.voice.trim().to_ascii_lowercase();
        self.voice = if matches!(v.as_str(), "draft" | "takes" | "none") {
            v
        } else {
            "auto".into()
        };
        self
    }

    /// Length of the finished video, seconds (cards and inserts included).
    pub fn output_seconds(&self) -> f64 {
        timeline::Timeline::new(self, &[]).total()
    }

    /// Where recording second `t` lands in the finished video, if kept.
    pub fn output_time(&self, t: f64) -> Option<f64> {
        timeline::Timeline::new(self, &[]).output_time(t)
    }
}

/// A first edit from the event track: each action kept at normal speed
/// with a little lead-in and follow-through, the waiting in between sped
/// up, a zoom on each click or field typed into (not on rapid sequences),
/// a plain caption per action. The agent then rewrites captions and title.
pub fn draft(events: &[Event], duration: f64, goal: &str) -> Edl {
    const BEFORE: f64 = 1.0;
    const AFTER: f64 = 1.8;
    let mut windows: Vec<(f64, f64)> = Vec::new();
    for e in events.iter().filter(|e| e.kind != "note") {
        let (a, b) = ((e.t - BEFORE).max(0.0), (e.t + AFTER).min(duration));
        match windows.last_mut() {
            Some(w) if a <= w.1 + 0.5 => w.1 = w.1.max(b),
            _ => windows.push((a, b)),
        }
    }
    let mut segments = Vec::new();
    let mut at = 0.0;
    for (a, b) in windows {
        if a - at > 0.3 {
            // Waiting: fast, but long waits faster.
            let gap = a - at;
            segments.push(Segment {
                from: at,
                to: a,
                speed: if gap > 8.0 { 12.0 } else { 6.0 },
            });
        }
        segments.push(Segment {
            from: a,
            to: b,
            speed: 1.0,
        });
        at = b;
    }
    if duration - at > 0.3 {
        segments.push(Segment {
            from: at,
            to: duration,
            speed: 8.0,
        });
    }
    let mut zooms: Vec<Zoom> = Vec::new();
    for e in events
        .iter()
        .filter(|e| matches!(e.kind.as_str(), "click" | "type"))
    {
        let (Some(x), Some(y)) = (e.x, e.y) else {
            continue;
        };
        if zooms.last().is_some_and(|z| e.t - z.at < 0.8) {
            continue;
        }
        zooms.push(Zoom {
            at: e.t,
            x,
            y,
            scale: if e.kind == "type" { 1.8 } else { 1.6 },
            hold: 1.2,
        });
    }
    let mut lines: Vec<Line> = Vec::new();
    for e in events.iter().filter(|e| e.kind != "scroll") {
        let text = match e.kind.as_str() {
            "click" if !e.label.is_empty() => format!("Click {}.", e.label),
            "type" if !e.label.is_empty() => format!("Fill in {}.", e.label),
            "navigate" => format!("Open {}.", e.label),
            "press" => format!("Press {}.", e.label),
            "wait" => format!("Wait for {}.", e.label),
            _ => e.label.clone(),
        };
        if text.trim().is_empty() {
            continue;
        }
        lines.push(Line {
            id: format!("step-{}", lines.len() + 1),
            at: (e.t - 0.8).max(0.0),
            text,
            section: String::new(),
            only: Vec::new(),
        });
    }
    Edl {
        title: goal.trim().chars().take(70).collect(),
        subtitle: String::new(),
        end_text: "Done".into(),
        segments,
        zooms,
        captions: Vec::new(),
        music: auto_music(),
        cursor: true,
        width: default_width(),
        lines,
        inserts: Vec::new(),
        cuts: default_cuts(),
        voice: auto_music(),
        draft_voice: String::new(),
        clean_master: true,
        platforms: default_platforms(),
    }
    .normalized(duration)
}

/// `ffmpeg` here: on PATH or downloaded by `/video setup`.
pub fn ffmpeg() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    if let Some(home) = dcode_ai_common::config::home_dir() {
        let p = home.join(".dcode-ai").join("bin").join(name);
        if p.is_file() {
            return Some(p);
        }
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(t: f64, kind: &str, xy: Option<(f64, f64)>, label: &str) -> Event {
        Event {
            t,
            kind: kind.into(),
            x: xy.map(|p| p.0),
            y: xy.map(|p| p.1),
            label: label.into(),
        }
    }

    #[test]
    fn the_draft_keeps_actions_and_speeds_up_waiting() {
        let events = vec![
            ev(1.0, "navigate", None, "example.com"),
            ev(12.0, "click", Some((300.0, 200.0)), "\"Sign up\""),
            ev(12.5, "type", Some((300.0, 260.0)), "Email"),
            ev(30.0, "click", Some((500.0, 400.0)), "\"Create\""),
        ];
        let e = draft(&events, 35.0, "Signing up");
        // Actions at normal speed, the long wait in between fast.
        let at = |t: f64| {
            e.segments
                .iter()
                .find(|s| t >= s.from && t <= s.to)
                .unwrap()
                .speed
        };
        assert_eq!(at(12.0), 1.0);
        assert_eq!(at(30.0), 1.0);
        assert!(at(20.0) >= 6.0, "{:?}", e.segments);
        // One zoom per click, not for the field typed 0.5 s later.
        assert_eq!(e.zooms.len(), 2);
        assert!(
            e.lines
                .iter()
                .any(|l| l.text == "Click \"Sign up\"." && l.id == "step-2")
        );
        // Much shorter than the 35 s recording, cards included.
        assert!(e.output_seconds() < 25.0, "{}", e.output_seconds());
        // Times map in order.
        assert!(e.output_time(12.0).unwrap() < e.output_time(30.0).unwrap());
        assert_eq!(e.title, "Signing up");
    }

    #[test]
    fn a_bad_edit_list_is_made_renderable() {
        let e = Edl {
            segments: vec![
                Segment {
                    from: 5.0,
                    to: 99.0,
                    speed: 0.0,
                },
                Segment {
                    from: 3.0,
                    to: 1.0,
                    speed: 1.0,
                },
            ],
            zooms: vec![Zoom {
                at: 1.0,
                x: 0.0,
                y: 0.0,
                scale: 9.0,
                hold: 1.0,
            }],
            width: 1001,
            ..Default::default()
        }
        .normalized(10.0);
        assert_eq!(
            e.segments,
            vec![Segment {
                from: 5.0,
                to: 10.0,
                speed: 1.0
            }]
        );
        assert_eq!(e.zooms[0].scale, 3.0);
        assert_eq!(e.width, 1000);
        let empty = Edl::default().normalized(4.0);
        assert_eq!(
            empty.segments.len(),
            1,
            "everything kept when nothing is said"
        );
    }
}

/// End to end with a real browser: `DCODE_AI_TEST_CDP_PORT=<port>` of a
/// running Chrome (e.g. headless), `cargo test -- --ignored record_`.
#[cfg(test)]
mod live {
    #[tokio::test]
    #[ignore]
    async fn record_a_real_page_and_render_it() {
        let Ok(port) = std::env::var("DCODE_AI_TEST_CDP_PORT") else {
            return;
        };
        let port: u16 = port.parse().unwrap();
        crate::browser::connect(port).await.unwrap();
        let ws = tempfile::tempdir().unwrap();
        let w = ws.path();
        let page = "data:text/html,<body style='font:24px sans-serif;padding:60px'><h1>Acme</h1>\
<input id=n placeholder='Project name' style='font-size:24px'> \
<button style='font-size:24px' onclick=\"document.querySelector('h1').textContent='Created: '+document.getElementById('n').value\">Create project</button></body>";
        let act = |v: serde_json::Value| {
            let w = w.to_path_buf();
            async move { crate::browser::run_action(&v, &w).await }
        };
        act(serde_json::json!({"action": "navigate", "url": page}))
            .await
            .unwrap();
        let id = super::record::start(w, "Creating a project")
            .await
            .unwrap()
            .id;
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        let snap = act(serde_json::json!({"action": "snapshot"}))
            .await
            .unwrap();
        let refnum = |needle: &str| -> u64 {
            let line = snap
                .lines()
                .find(|l| l.starts_with('[') && l.contains(needle))
                .unwrap_or_else(|| panic!("{needle} in {snap}"));
            line.split(['[', ']']).nth(1).unwrap().parse().unwrap()
        };
        let (input, button) = (refnum("Project name"), refnum("Create project"));
        act(serde_json::json!({"action": "type", "ref": input, "text": "Apollo"}))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
        act(serde_json::json!({"action": "click", "ref": button}))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        let s = super::record::stop().await.unwrap();
        assert_eq!(s.id, id);
        assert!(s.frames >= 2, "frames: {}", s.frames);
        let kinds: Vec<&str> = s.events.iter().map(|e| e.kind.as_str()).collect();
        assert_eq!(kinds, ["type", "click"], "{:?}", s.events);
        assert!(s.events[1].x.is_some(), "clicks carry where");
        // Narrated, with a card and a 9:16 short (the vidni-style path).
        let mut edl = s.draft.clone();
        edl.title = "Creating a project".into();
        edl.lines = vec![
            super::Line {
                id: "name".into(),
                at: (s.events[0].t - 0.8).max(0.0),
                text: "First, give your project a name.".into(),
                section: "Set up".into(),
                only: Vec::new(),
            },
            super::Line {
                id: "create".into(),
                at: (s.events[1].t - 0.8).max(0.0),
                text: "Then click Create project, and it's ready.".into(),
                section: "Create".into(),
                only: Vec::new(),
            },
        ];
        edl.inserts = vec![super::Insert {
            at: s.events[1].t + 1.0,
            seconds: 2.5,
            kind: "bullets".into(),
            title: "What we did".into(),
            items: vec!["Named the project".into(), "Created it".into()],
        }];
        edl.cuts = vec!["long".into(), "short".into()];
        std::fs::write(s.dir.join("edl.json"), serde_json::to_string(&edl).unwrap()).unwrap();
        let r = super::render::render(&s.dir, |p| eprintln!("{p}")).unwrap();
        eprintln!("{r:?}");
        assert!(r.gif.is_file());
        assert!(s.dir.join("chapters.txt").is_file());
        if let Ok(out) = std::env::var("DCODE_AI_LOOK") {
            for f in [
                "video.gif",
                "video.mp4",
                "video-clean.mp4",
                "short.mp4",
                "captions.srt",
                "chapters.txt",
                "thumbnail.png",
                "short-thumbnail.png",
                "short-review.png",
                "edl.json",
            ] {
                let _ = std::fs::copy(s.dir.join(f), format!("{out}/{f}"));
            }
        }
    }
}

/// A tab covered by another one sends no screencast frames; the recorder
/// falls back to screenshots. `DCODE_AI_TEST_CDP_PORT=<port>`,
/// `cargo test -- --ignored record_a_hidden`.
#[cfg(test)]
mod live_hidden {
    #[tokio::test]
    #[ignore]
    async fn record_a_hidden_tab() {
        let Ok(port) = std::env::var("DCODE_AI_TEST_CDP_PORT") else {
            return;
        };
        let port: u16 = port.parse().unwrap();
        crate::browser::connect(port).await.unwrap();
        let ws = tempfile::tempdir().unwrap();
        let w = ws.path().to_path_buf();
        let act = |v: serde_json::Value| {
            let w = w.clone();
            async move { crate::browser::run_action(&v, &w).await }
        };
        act(serde_json::json!({"action": "navigate", "url": "data:text/html,<body><h1 id=c>0</h1><script>setInterval(()=>c.textContent=Date.now(),100)</script></body>"}))
            .await
            .unwrap();
        // Cover it: open another tab in front (over HTTP, so the tool
        // stays on the first one).
        let _ = reqwest::Client::new()
            .put(format!("http://127.0.0.1:{port}/json/new?about:blank"))
            .send()
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let vis =
            act(serde_json::json!({"action": "eval", "expression": "document.visibilityState"}))
                .await
                .unwrap();
        eprintln!("visibility before: {vis}");
        let started = super::record::start(&w, "Hidden").await.unwrap();
        eprintln!("visible at start: {}", started.visible);
        // Covered again while recording: the screencast goes quiet.
        let _ = reqwest::Client::new()
            .put(format!("http://127.0.0.1:{port}/json/new?about:blank"))
            .send()
            .await;
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        let vis =
            act(serde_json::json!({"action": "eval", "expression": "document.visibilityState"}))
                .await
                .unwrap();
        eprintln!("visibility while recording: {vis}");
        // However hidden the page gets, frames must keep coming: silence
        // the screencast as Chrome does for a covered window.
        crate::browser::stop_screencast().await.unwrap();
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        let s = super::record::stop().await.unwrap();
        eprintln!("{} frames in {:.1}s", s.frames, s.duration);
        assert!(s.frames >= 8, "frames keep coming: {}", s.frames);
    }
}
