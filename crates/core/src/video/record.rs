//! Recording: the page's screencast frames (JPEG, timestamped) written to
//! `<videos>/<id>/raw/`, and the event track — every browser action the
//! agent takes while recording, with where and when.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde_json::Value;

use super::{Edl, Event, FrameInfo};

/// Longest recording, seconds.
pub const MAX_SECONDS: f64 = 600.0;

struct Active {
    id: String,
    dir: PathBuf,
    goal: String,
    start: f64,
    events: Arc<Mutex<Vec<Event>>>,
    frames: Arc<Mutex<Vec<FrameInfo>>>,
    task: tokio::task::JoinHandle<()>,
}

static ACTIVE: Mutex<Option<Active>> = Mutex::new(None);

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Whether a recording is running.
pub fn is_recording() -> bool {
    ACTIVE.lock().is_ok_and(|a| a.is_some())
}

/// Adds to the event track (a no-op when not recording). Coordinates in
/// CSS pixels.
pub fn note(kind: &str, xy: Option<(f64, f64)>, label: &str) {
    let Ok(g) = ACTIVE.lock() else { return };
    let Some(a) = g.as_ref() else { return };
    if let Ok(mut ev) = a.events.lock() {
        ev.push(Event {
            t: (now() - a.start).max(0.0),
            kind: kind.into(),
            x: xy.map(|p| p.0),
            y: xy.map(|p| p.1),
            label: label.chars().take(120).collect(),
        });
    }
}

/// Starts recording the browser (or the page the agent drives).
pub async fn start(workspace: &Path, goal: &str) -> Result<String, String> {
    if is_recording() {
        return Err("already recording — record_stop first".into());
    }
    let id = format!("rec-{}", chrono::Local::now().format("%Y%m%d-%H%M%S"));
    let dir = super::videos_dir(workspace).join(&id);
    std::fs::create_dir_all(dir.join("raw")).map_err(|e| e.to_string())?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let acker = crate::browser::start_screencast(tx).await?;
    let start = now();
    let frames: Arc<Mutex<Vec<FrameInfo>>> = Default::default();
    let raw = dir.join("raw");
    let task = {
        let frames = frames.clone();
        tokio::spawn(async move {
            let mut n = 0usize;
            while let Some(v) = rx.recv().await {
                let p = &v["params"];
                if let Some(session) = p["sessionId"].as_u64() {
                    acker.ack(session);
                }
                let at = p["metadata"]["timestamp"].as_f64().unwrap_or_else(now);
                let t = (at - start).max(0.0);
                if t > MAX_SECONDS {
                    continue;
                }
                let Some(Ok(bytes)) = p["data"]
                    .as_str()
                    .map(|d| base64::engine::general_purpose::STANDARD.decode(d))
                else {
                    continue;
                };
                n += 1;
                let file = format!("f{n:06}.jpg");
                if std::fs::write(raw.join(&file), bytes).is_err() {
                    continue;
                }
                if let Ok(mut f) = frames.lock() {
                    f.push(FrameInfo {
                        t,
                        file,
                        css_w: p["metadata"]["deviceWidth"].as_f64().unwrap_or(0.0),
                    });
                }
            }
        })
    };
    if let Ok(mut g) = ACTIVE.lock() {
        *g = Some(Active {
            id: id.clone(),
            dir,
            goal: goal.to_string(),
            start,
            events: Default::default(),
            frames,
            task,
        });
    }
    Ok(id)
}

/// What a finished recording holds.
pub struct Stopped {
    pub id: String,
    pub dir: PathBuf,
    pub duration: f64,
    pub frames: usize,
    pub events: Vec<Event>,
    pub draft: Edl,
}

/// Stops recording; writes `frames.json`, `events.json` and a draft
/// `edl.json` next to the frames.
pub async fn stop() -> Result<Stopped, String> {
    let active = ACTIVE
        .lock()
        .map_err(|_| "lock".to_string())?
        .take()
        .ok_or("not recording")?;
    let _ = crate::browser::stop_screencast().await;
    // The frame task ends once the page stops sending.
    let _ = tokio::time::timeout(std::time::Duration::from_secs(3), active.task).await;
    let duration = (now() - active.start).clamp(0.0, MAX_SECONDS);
    let frames = active.frames.lock().map(|f| f.clone()).unwrap_or_default();
    let events = active.events.lock().map(|e| e.clone()).unwrap_or_default();
    if frames.is_empty() {
        return Err("no frames were captured — is the page visible in the browser?".into());
    }
    let draft = super::draft(&events, duration, &active.goal);
    let write = |name: &str, v: &dyn erased::Json| {
        std::fs::write(active.dir.join(name), v.json()).map_err(|e| e.to_string())
    };
    write("frames.json", &frames)?;
    write("events.json", &events)?;
    write("edl.json", &draft)?;
    std::fs::write(
        active.dir.join("meta.json"),
        serde_json::json!({"goal": active.goal, "duration": duration}).to_string(),
    )
    .map_err(|e| e.to_string())?;
    Ok(Stopped {
        id: active.id,
        dir: active.dir,
        duration,
        frames: frames.len(),
        events,
        draft,
    })
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

/// A recording on disk: its frames, event track, duration, edit list.
pub struct Saved {
    pub dir: PathBuf,
    pub frames: Vec<FrameInfo>,
    pub events: Vec<Event>,
    pub duration: f64,
    pub edl: Edl,
}

pub fn load(dir: &Path) -> Result<Saved, String> {
    let read =
        |name: &str| std::fs::read_to_string(dir.join(name)).map_err(|e| format!("{name}: {e}"));
    let frames: Vec<FrameInfo> =
        serde_json::from_str(&read("frames.json")?).map_err(|e| e.to_string())?;
    let events: Vec<Event> = serde_json::from_str(&read("events.json")?).unwrap_or_default();
    let meta: Value = read("meta.json")
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let duration = meta["duration"]
        .as_f64()
        .unwrap_or_else(|| frames.last().map(|f| f.t + 1.0).unwrap_or(0.0));
    let edl: Edl =
        serde_json::from_str(&read("edl.json")?).map_err(|e| format!("edl.json: {e}"))?;
    Ok(Saved {
        dir: dir.to_path_buf(),
        frames,
        events,
        duration,
        edl,
    })
}

/// Recordings in the project, newest first: (id, dir).
pub fn list(workspace: &Path) -> Vec<(String, PathBuf)> {
    let mut v: Vec<(String, PathBuf)> = std::fs::read_dir(super::videos_dir(workspace))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().join("frames.json").is_file())
        .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    v
}

/// The event track as lines for the agent ("12.4s click (300,200) "Sign up"").
pub fn describe(events: &[Event]) -> String {
    events
        .iter()
        .map(|e| {
            let at = match (e.x, e.y) {
                (Some(x), Some(y)) => format!(" ({x:.0},{y:.0})"),
                _ => String::new(),
            };
            format!("{:>6.1}s {}{at} {}", e.t, e.kind, e.label)
        })
        .collect::<Vec<_>>()
        .join("\n")
}
