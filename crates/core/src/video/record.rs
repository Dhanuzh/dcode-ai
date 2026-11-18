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
pub const MAX_SECONDS: f64 = 1800.0;

/// How a recording takes its pictures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// The page's live screencast (screenshots when it goes quiet).
    #[default]
    Live,
    /// One settled screenshot per step, the camera moving over it.
    Stills,
    /// The whole screen (desktop apps, the terminal), not just the browser.
    Screen,
}

/// Private parts of the page found at a moment (CSS pixel boxes).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Privacy {
    pub t: f64,
    pub boxes: Vec<[f64; 4]>,
}

/// Where the page's main content sat at a moment (CSS pixels: x, y, w, h),
/// for framing it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Focus {
    pub t: f64,
    pub b: [f64; 4],
}

/// The page's visible text at a moment (for text anchors).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct PageText {
    pub t: f64,
    pub text: String,
}

struct Active {
    id: String,
    dir: PathBuf,
    goal: String,
    start: f64,
    mode: Mode,
    redact: Vec<String>,
    events: Arc<Mutex<Vec<Event>>>,
    frames: Arc<Mutex<Vec<FrameInfo>>>,
    privacy: Arc<Mutex<Vec<Privacy>>>,
    focus: Arc<Mutex<Vec<Focus>>>,
    texts: Arc<Mutex<Vec<PageText>>>,
    writer: Arc<Writer>,
    task: Option<tokio::task::JoinHandle<()>>,
    /// Screenshots while the screencast is quiet.
    fallback: Option<tokio::task::JoinHandle<()>>,
    /// The whole-screen capture (screen mode).
    screen: Option<super::capture::Running>,
    /// The presenter's webcam.
    webcam: Option<super::capture::Running>,
    /// Splice into this recording's step when stopping.
    redo: Option<(PathBuf, usize)>,
}

/// Writes frames (from the screencast or a screenshot) into `raw/`.
struct Writer {
    raw: PathBuf,
    start: f64,
    n: std::sync::atomic::AtomicUsize,
    frames: Arc<Mutex<Vec<FrameInfo>>>,
    /// When the last screencast frame arrived (wall clock).
    last: Mutex<f64>,
    /// Paused since (recording seconds on the raw clock), and the paused
    /// stretches so far (raw clock), cut out at stop.
    paused_since: Mutex<Option<f64>>,
    pauses: Mutex<Vec<(f64, f64)>>,
    /// The cap cut the take short.
    capped: std::sync::atomic::AtomicBool,
}

impl Writer {
    fn is_paused(&self) -> bool {
        self.paused_since.lock().is_ok_and(|p| p.is_some())
    }

    /// Seconds recorded so far on the raw clock `t`, pauses left out.
    fn recorded(&self, t: f64) -> f64 {
        let paused: f64 = self
            .pauses
            .lock()
            .map(|p| p.iter().map(|(a, b)| b - a).sum())
            .unwrap_or(0.0);
        let open = self
            .paused_since
            .lock()
            .ok()
            .and_then(|p| *p)
            .map(|since| (t - since).max(0.0))
            .unwrap_or(0.0);
        (t - paused - open).max(0.0)
    }

    /// `screencast`: from the live stream (resets the quiet timer), else a
    /// fallback screenshot.
    fn write(&self, at: f64, bytes: &[u8], css_w: f64, screencast: bool) {
        let t = (at - self.start).max(0.0);
        if self.is_paused() {
            return;
        }
        if self.recorded(t) > MAX_SECONDS {
            self.capped
                .store(true, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        let n = self.n.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let file = format!("f{n:06}.jpg");
        if std::fs::write(self.raw.join(&file), bytes).is_err() {
            return;
        }
        if screencast && let Ok(mut l) = self.last.lock() {
            *l = now();
        }
        if let Ok(mut f) = self.frames.lock() {
            f.push(FrameInfo { t, file, css_w });
        }
    }

    fn quiet_for(&self) -> f64 {
        self.last.lock().map(|l| now() - *l).unwrap_or(0.0)
    }
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

/// The running recording's mode.
pub fn mode() -> Option<Mode> {
    ACTIVE.lock().ok()?.as_ref().map(|a| a.mode)
}

/// Words the running recording hides (besides emails, ids and tokens).
pub fn redact_words() -> Vec<String> {
    ACTIVE
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|a| a.redact.clone()))
        .unwrap_or_default()
}

/// Adds to the event track (a no-op when not recording). Coordinates and
/// the element's size in CSS pixels.
pub fn note(kind: &str, xy: Option<(f64, f64)>, size: Option<(f64, f64)>, label: &str) {
    let at = ACTION_AT.lock().ok().and_then(|mut m| m.take());
    let Ok(g) = ACTIVE.lock() else { return };
    let Some(a) = g.as_ref() else { return };
    if a.writer.is_paused() {
        return;
    }
    if let Ok(mut ev) = a.events.lock() {
        ev.push(Event {
            t: (at.unwrap_or_else(now) - a.start).max(0.0),
            kind: kind.into(),
            x: xy.map(|p| p.0),
            y: xy.map(|p| p.1),
            w: size.map(|p| p.0),
            h: size.map(|p| p.1),
            label: label.chars().take(120).collect(),
        });
    }
}

/// A step the agent names while recording (screen mode has no browser
/// actions to log): it shows in the script as a point to narrate at.
pub fn mark(label: &str) {
    note("step", None, None, label);
}

/// When the current click or typing actually happened (set just before it;
/// the step is noted only after the page it opens has loaded).
static ACTION_AT: Mutex<Option<f64>> = Mutex::new(None);

/// Marks the moment of the current action (`false`: forget it).
pub fn mark_action(on: bool) {
    if let Ok(mut m) = ACTION_AT.lock() {
        *m = on.then(now);
    }
}

/// Pauses the recording: nothing until [`resume`] goes into the video.
pub fn pause() -> Result<String, String> {
    let g = ACTIVE.lock().map_err(|_| "lock".to_string())?;
    let a = g.as_ref().ok_or("not recording")?;
    let mut p = a
        .writer
        .paused_since
        .lock()
        .map_err(|_| "lock".to_string())?;
    if p.is_some() {
        return Err("already paused — record_resume goes on".into());
    }
    *p = Some((now() - a.start).max(0.0));
    Ok("paused — nothing is recorded until record_resume".into())
}

/// Goes on after [`pause`].
pub fn resume() -> Result<String, String> {
    let g = ACTIVE.lock().map_err(|_| "lock".to_string())?;
    let a = g.as_ref().ok_or("not recording")?;
    let since = a
        .writer
        .paused_since
        .lock()
        .map_err(|_| "lock".to_string())?
        .take()
        .ok_or("not paused")?;
    let until = (now() - a.start).max(since);
    if let Ok(mut p) = a.writer.pauses.lock() {
        p.push((since, until));
    }
    Ok(format!(
        "recording again (the {:.0} s paused are left out)",
        until - since
    ))
}

/// Seconds left before the cap, when under two minutes (for a heads-up in
/// tool results).
pub fn cap_warning() -> Option<String> {
    let g = ACTIVE.lock().ok()?;
    let a = g.as_ref()?;
    let left = MAX_SECONDS - a.writer.recorded((now() - a.start).max(0.0));
    if left <= 0.0 {
        Some(format!(
            "⚠ the recording reached its {} min cap — nothing more is being recorded; record_stop now",
            MAX_SECONDS as u64 / 60
        ))
    } else if left < 120.0 {
        Some(format!(
            "⚠ {left:.0} s left before the recording's {} min cap — wrap up and record_stop (or record_pause while waiting)",
            MAX_SECONDS as u64 / 60
        ))
    } else {
        None
    }
}

/// A time on the raw clock with the paused stretches cut out (`None`: it
/// was during a pause).
pub fn squeeze(t: f64, pauses: &[(f64, f64)]) -> Option<f64> {
    let mut shift = 0.0;
    for (a, b) in pauses {
        if t >= *a && t < *b {
            return None;
        }
        if t >= *b {
            shift += b - a;
        }
    }
    Some(t - shift)
}

/// A still for a stills-mode recording (a no-op otherwise).
pub fn add_still(bytes: &[u8], css_w: f64) {
    let Ok(g) = ACTIVE.lock() else { return };
    if let Some(a) = g.as_ref() {
        a.writer.write(now(), bytes, css_w, false);
    }
}

/// The private parts of the page now (a no-op when not recording).
pub fn add_privacy(boxes: Vec<[f64; 4]>) {
    let Ok(g) = ACTIVE.lock() else { return };
    if let Some(a) = g.as_ref()
        && !a.writer.is_paused()
        && let Ok(mut p) = a.privacy.lock()
    {
        p.push(Privacy {
            t: (now() - a.start).max(0.0),
            boxes,
        });
    }
}

/// Where the page's main content is now (a no-op when not recording).
pub fn add_focus(b: [f64; 4]) {
    let Ok(g) = ACTIVE.lock() else { return };
    if let Some(a) = g.as_ref()
        && !a.writer.is_paused()
        && let Ok(mut f) = a.focus.lock()
    {
        f.push(Focus {
            t: (now() - a.start).max(0.0),
            b,
        });
    }
}

/// The page's visible text now (a no-op when not recording).
pub fn add_text(text: String) {
    let Ok(g) = ACTIVE.lock() else { return };
    if let Some(a) = g.as_ref()
        && !a.writer.is_paused()
        && let Ok(mut v) = a.texts.lock()
    {
        v.push(PageText {
            t: (now() - a.start).max(0.0),
            text,
        });
    }
}

/// How to record.
#[derive(Debug, Clone, Default)]
pub struct Options {
    pub mode: Mode,
    /// Words to hide wherever they show (names, customers…).
    pub redact: Vec<String>,
    /// Record at the window's own size instead of the standard 1440×810
    /// CSS-pixel viewport (drawn at 1920×1080).
    pub keep_viewport: bool,
    /// Record the webcam too, for a presenter bubble.
    pub webcam: bool,
    /// Re-record one step of an earlier recording: (its id, step from 1).
    /// The new take replaces that step when the recording stops.
    pub redo: Option<(String, usize)>,
}

/// Starts recording the browser (or the page the agent drives).
pub async fn start(workspace: &Path, goal: &str, opts: Options) -> Result<Started, String> {
    if is_recording() {
        return Err("already recording — record_stop first".into());
    }
    let redo = match &opts.redo {
        Some((rid, n)) => {
            let found = list(workspace)
                .into_iter()
                .find(|(v, _)| v == rid || v.ends_with(rid.as_str()))
                .ok_or_else(|| format!("no recording {rid} to redo a step of"))?;
            let saved = load(&found.1)?;
            super::splice::window(&saved, *n)?;
            Some((found.1, *n))
        }
        None => None,
    };
    let id = format!("rec-{}", chrono::Local::now().format("%Y%m%d-%H%M%S"));
    let dir = super::videos_dir(workspace).join(&id);
    std::fs::create_dir_all(dir.join("raw")).map_err(|e| e.to_string())?;
    if !opts.keep_viewport && opts.mode != Mode::Screen {
        crate::browser::fit_recording_viewport().await?;
    }
    let start = now();
    let frames: Arc<Mutex<Vec<FrameInfo>>> = Default::default();
    let writer = Arc::new(Writer {
        raw: dir.join("raw"),
        start,
        n: Default::default(),
        frames: frames.clone(),
        last: Mutex::new(start),
        paused_since: Mutex::new(None),
        pauses: Mutex::new(Vec::new()),
        capped: Default::default(),
    });
    let activate = |task, fallback| {
        if let Ok(mut g) = ACTIVE.lock() {
            *g = Some(Active {
                id: id.clone(),
                dir: dir.clone(),
                goal: goal.to_string(),
                start,
                mode: opts.mode,
                redact: opts.redact.clone(),
                events: Default::default(),
                frames: frames.clone(),
                privacy: Default::default(),
                focus: Default::default(),
                texts: Default::default(),
                writer: writer.clone(),
                task,
                fallback,
                screen: None,
                webcam: None,
                redo: redo.clone(),
            });
        }
    };
    // Captures beside the browser: the whole screen, the webcam.
    let side_captures = |dir: &Path| -> Result<(), String> {
        let at = (now() - start).max(0.0);
        let screen = if opts.mode == Mode::Screen {
            let mut r = super::capture::start_screen(dir, at)?;
            std::thread::sleep(std::time::Duration::from_millis(600));
            if r.failed_early() {
                return Err(
                    "the screen recorder stopped straight away (no screen capture here?)".into(),
                );
            }
            match r.wait_first_frame(std::time::Duration::from_secs(15)) {
                // The screenshot loop's clock, which names its files, starts
                // after PowerShell's start-up: anchor its frames to now.
                Ok(true) => r.at = (now() - start).max(0.0),
                Ok(false) => {}
                Err(e) => {
                    r.stop();
                    return Err(e);
                }
            }
            Some(r)
        } else {
            None
        };
        let webcam = if opts.webcam {
            let mut r = match super::capture::start_webcam(dir, at) {
                Ok(r) => r,
                Err(e) => {
                    if let Some(s) = screen {
                        s.stop();
                    }
                    return Err(e);
                }
            };
            std::thread::sleep(std::time::Duration::from_millis(400));
            if r.failed_early() {
                if let Some(s) = screen {
                    s.stop();
                }
                return Err(
                    "the webcam recorder stopped straight away (camera busy or not allowed?)"
                        .into(),
                );
            }
            Some(r)
        } else {
            None
        };
        if let Ok(mut g) = ACTIVE.lock()
            && let Some(a) = g.as_mut()
        {
            a.screen = screen;
            a.webcam = webcam;
        }
        Ok(())
    };
    if opts.mode == Mode::Screen {
        activate(None, None);
        if let Err(e) = side_captures(&dir) {
            if let Ok(mut g) = ACTIVE.lock() {
                *g = None;
            }
            return Err(e);
        }
        return Ok(Started {
            id: id.clone(),
            visible: true,
        });
    }
    if opts.mode == Mode::Stills {
        // No live recording: a settled still now, then one after each step.
        activate(None, None);
        if let Err(e) = crate::browser::observe_now().await {
            if let Ok(mut g) = ACTIVE.lock() {
                *g = None;
            }
            crate::browser::restore_viewport().await;
            return Err(e);
        }
        if let Err(e) = side_captures(&dir) {
            if let Ok(mut g) = ACTIVE.lock() {
                *g = None;
            }
            crate::browser::restore_viewport().await;
            return Err(e);
        }
        return Ok(Started {
            id: id.clone(),
            visible: true,
        });
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
    let (acker, visible) = match crate::browser::start_screencast(tx).await {
        Ok(v) => v,
        Err(e) => {
            crate::browser::restore_viewport().await;
            return Err(e);
        }
    };
    let acker = Arc::new(acker);
    let task = {
        let (writer, acker) = (writer.clone(), acker.clone());
        tokio::spawn(async move {
            while let Some(v) = rx.recv().await {
                let p = &v["params"];
                if let Some(session) = p["sessionId"].as_u64() {
                    acker.ack(session);
                }
                let at = p["metadata"]["timestamp"].as_f64().unwrap_or_else(now);
                let Some(Ok(bytes)) = p["data"]
                    .as_str()
                    .map(|d| base64::engine::general_purpose::STANDARD.decode(d))
                else {
                    continue;
                };
                writer.write(
                    at,
                    &bytes,
                    p["metadata"]["deviceWidth"].as_f64().unwrap_or(0.0),
                    true,
                );
            }
        })
    };
    // Chrome sends no screencast frames for a tab it thinks is hidden (a
    // covered window on Windows, a background tab): then take screenshots,
    // about four a second, so the video still moves.
    let fallback = {
        let (writer, acker) = (writer.clone(), acker.clone());
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_millis(250));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                if writer.quiet_for() < 0.8 {
                    continue;
                }
                if let Ok((bytes, css_w)) = acker.capture().await {
                    writer.write(now(), &bytes, css_w, false);
                }
            }
        })
    };
    activate(Some(task), Some(fallback));
    if let Err(e) = side_captures(&dir) {
        if let Ok(mut g) = ACTIVE.lock()
            && let Some(a) = g.take()
        {
            if let Some(t) = a.task {
                t.abort();
            }
            if let Some(f) = a.fallback {
                f.abort();
            }
        }
        let _ = crate::browser::stop_screencast().await;
        crate::browser::restore_viewport().await;
        return Err(e);
    }
    // What is private on the page at the start.
    let _ = crate::browser::observe_now().await;
    Ok(Started {
        id: id.clone(),
        visible,
    })
}

/// A recording that started: its id, and whether the page was visible
/// (a hidden one is recorded by screenshots, more slowly).
pub struct Started {
    pub id: String,
    pub visible: bool,
}

/// What a finished recording holds.
pub struct Stopped {
    pub id: String,
    pub dir: PathBuf,
    pub duration: f64,
    pub frames: usize,
    pub events: Vec<Event>,
    pub draft: Edl,
    /// A redo: what was spliced into the earlier recording.
    pub spliced: Option<String>,
    /// Worth telling: the cap cut the take, pauses left out, a first edit.
    pub notes: Vec<String>,
}

/// Stops recording; writes `frames.json`, `events.json` and a draft
/// `edl.json` next to the frames.
pub async fn stop() -> Result<Stopped, String> {
    let active = ACTIVE
        .lock()
        .map_err(|_| "lock".to_string())?
        .take()
        .ok_or("not recording")?;
    if let Some(f) = &active.fallback {
        f.abort();
    }
    if let Some(task) = active.task {
        let _ = crate::browser::stop_screencast().await;
        // The frame task ends once the page stops sending.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), task).await;
    }
    if active.mode != Mode::Screen {
        crate::browser::restore_viewport().await;
    }
    let raw_duration = (now() - active.start).max(0.0);
    // Paused stretches (one still open is closed now), cut out below.
    let mut pauses = active
        .writer
        .pauses
        .lock()
        .map(|p| p.clone())
        .unwrap_or_default();
    if let Some(since) = active.writer.paused_since.lock().ok().and_then(|p| *p) {
        pauses.push((since, raw_duration));
    }
    let paused_total: f64 = pauses.iter().map(|(a, b)| b - a).sum();
    let duration = (raw_duration - paused_total).clamp(0.0, MAX_SECONDS);
    let capped = active
        .writer
        .capped
        .load(std::sync::atomic::Ordering::Relaxed);
    let webcam_at = active.webcam.as_ref().map(|w| w.at);
    if let Some(w) = active.webcam {
        let _ = tokio::task::spawn_blocking(move || w.stop()).await;
    }
    if let Some(sc) = active.screen {
        let (out, at, raw) = (sc.out.clone(), sc.at, active.dir.join("raw"));
        let shots = tokio::task::spawn_blocking(move || {
            sc.stop();
            super::capture::screen_frames(&out, &raw, at)
        })
        .await
        .map_err(|e| e.to_string())??;
        if let Ok(mut f) = active.frames.lock() {
            f.extend(shots.into_iter().map(|(t, file)| FrameInfo {
                t,
                file,
                css_w: 0.0,
            }));
        }
    }
    let mut frames = active.frames.lock().map(|f| f.clone()).unwrap_or_default();
    // Screencast frames and screenshots interleave; keep them in time order.
    frames.sort_by(|a, b| a.t.total_cmp(&b.t));
    let mut events = active.events.lock().map(|e| e.clone()).unwrap_or_default();
    let mut privacy = active.privacy.lock().map(|p| p.clone()).unwrap_or_default();
    let mut focus = active.focus.lock().map(|p| p.clone()).unwrap_or_default();
    let mut texts = active.texts.lock().map(|p| p.clone()).unwrap_or_default();
    if !pauses.is_empty() {
        // The paused stretches go: what was in them, and the time.
        frames.retain_mut(|f| squeeze(f.t, &pauses).map(|t| f.t = t).is_some());
        events.retain_mut(|e| squeeze(e.t, &pauses).map(|t| e.t = t).is_some());
        privacy.retain_mut(|p| squeeze(p.t, &pauses).map(|t| p.t = t).is_some());
        focus.retain_mut(|p| squeeze(p.t, &pauses).map(|t| p.t = t).is_some());
        texts.retain_mut(|p| squeeze(p.t, &pauses).map(|t| p.t = t).is_some());
    }
    frames.retain(|f| f.t <= MAX_SECONDS);
    if frames.is_empty() {
        return Err("no frames were captured — is the page visible in the browser?".into());
    }
    let mut draft = super::draft(&events, duration, &active.goal);
    let mut notes = Vec::new();
    if capped {
        notes.push(format!(
            "the {} min cap cut the take: what came after wasn't recorded",
            MAX_SECONDS as u64 / 60
        ));
    }
    if paused_total > 0.5 {
        notes.push(format!(
            "{:.0} s paused in {} pause(s), left out",
            paused_total,
            pauses.len()
        ));
    }
    // A screen recording has no clicks to edit by: cut and zoom by where
    // the picture changes.
    if active.mode == Mode::Screen {
        let (dir, fr) = (active.dir.clone(), frames.clone());
        let ch = tokio::task::spawn_blocking(move || super::autoedit::changes(&dir, &fr))
            .await
            .unwrap_or_default();
        let size = frames
            .first()
            .and_then(|f| image::image_dimensions(active.dir.join("raw").join(&f.file)).ok())
            .unwrap_or((1920, 1080));
        let (segs, zooms) = super::autoedit::apply(&mut draft, &events, &ch, duration, size);
        draft = draft.normalized(duration);
        notes.push(format!(
            "first edit from screen changes: {segs} segments (still stretches sped up or cut), {zooms} zooms on busy areas"
        ));
    }
    if let Some(at) = webcam_at
        && active.dir.join("webcam.mp4").is_file()
    {
        draft.webcam = Some(super::Webcam {
            file: "webcam.mp4".into(),
            // The clip runs through pauses: where they were and how long.
            cuts: pauses
                .iter()
                .filter_map(|(a, b)| squeeze(*a, &pauses).map(|t| [t, b - a]))
                .collect(),
            offset: at,
            corner: "bottom-right".into(),
            size: 0.26,
            shape: "circle".into(),
        });
    }
    let write = |name: &str, v: &dyn erased::Json| {
        std::fs::write(active.dir.join(name), v.json()).map_err(|e| e.to_string())
    };
    write("frames.json", &frames)?;
    write("events.json", &events)?;
    write("edl.json", &draft)?;
    write("privacy.json", &privacy)?;
    write("focus.json", &focus)?;
    if !texts.is_empty() {
        write("text.json", &texts)?;
    }
    std::fs::write(
        active.dir.join("meta.json"),
        serde_json::json!({"goal": active.goal, "duration": duration, "mode": active.mode})
            .to_string(),
    )
    .map_err(|e| e.to_string())?;
    if let Some((old, n)) = active.redo {
        // Splice this take into the earlier recording's step.
        let summary = super::splice::splice(&old, n, &active.dir)?;
        let s = load(&old)?;
        return Ok(Stopped {
            id: old
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default(),
            dir: old,
            duration: s.duration,
            frames: s.frames.len(),
            events: s.events,
            draft: s.edl,
            spliced: Some(summary),
            notes,
        });
    }
    Ok(Stopped {
        id: active.id,
        dir: active.dir,
        duration,
        frames: frames.len(),
        events,
        draft,
        spliced: None,
        notes,
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
    pub mode: Mode,
    pub privacy: Vec<Privacy>,
    /// The main content's box over time (none in older recordings).
    pub focus: Vec<Focus>,
    /// The page's visible text at each step (browser recordings).
    pub texts: Vec<PageText>,
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
    let mode: Mode = serde_json::from_value(meta["mode"].clone()).unwrap_or_default();
    let privacy: Vec<Privacy> = read("privacy.json")
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let focus: Vec<Focus> = read("focus.json")
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let texts: Vec<PageText> = read("text.json")
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    Ok(Saved {
        dir: dir.to_path_buf(),
        frames,
        events,
        duration,
        mode,
        privacy,
        focus,
        texts,
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

/// The event track as lines for the agent ("12.4s click (300,200) "Sign
/// up""), kept short: repeats of the same action merge ("scroll down ×4"),
/// and a long track keeps its start and end.
pub fn describe(events: &[Event]) -> String {
    let mut lines: Vec<String> = Vec::new();
    let mut i = 0;
    while i < events.len() {
        let e = &events[i];
        let mut j = i + 1;
        while j < events.len() && events[j].kind == e.kind && events[j].label == e.label {
            j += 1;
        }
        let at = match (e.x, e.y) {
            (Some(x), Some(y)) => format!(" ({x:.0},{y:.0})"),
            _ => String::new(),
        };
        let times = if j - i > 1 {
            format!(" ×{} (to {:.1}s)", j - i, events[j - 1].t)
        } else {
            String::new()
        };
        lines.push(format!("{:>6.1}s {}{at} {}{times}", e.t, e.kind, e.label));
        i = j;
    }
    const KEEP: usize = 60;
    if lines.len() > KEEP {
        let cut = lines.len() - KEEP;
        let tail = lines.split_off(KEEP / 2 + cut);
        lines.truncate(KEEP / 2);
        lines.push(format!("   … {cut} more actions …"));
        lines.extend(tail);
    }
    lines.join("\n")
}

#[cfg(test)]
mod pause_tests {
    use super::squeeze;

    #[test]
    fn paused_stretches_are_cut_out() {
        let pauses = [(10.0, 25.0), (40.0, 45.0)];
        assert_eq!(squeeze(5.0, &pauses), Some(5.0));
        assert_eq!(squeeze(12.0, &pauses), None);
        assert_eq!(squeeze(30.0, &pauses), Some(15.0));
        assert_eq!(squeeze(42.0, &pauses), None);
        assert_eq!(squeeze(50.0, &pauses), Some(30.0));
    }
}
