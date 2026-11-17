//! Browser control over the Chrome DevTools Protocol: launch (or attach to)
//! Chrome / Edge, then drive one page — navigate, read it as text, click,
//! type, press keys, scroll, screenshot, read the console. Exposed to the
//! agent as one `browser` tool with an `action`.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::tools::ToolExecutor;

/// Debug port dcode-ai launches its browser on (9222 is often taken).
pub const DEFAULT_PORT: u16 = 9333;
/// Console lines kept for `console`.
const CONSOLE_KEEP: usize = 100;
/// Most interactive elements listed in a snapshot.
const MAX_ELEMENTS: usize = 150;
/// Page text kept in a snapshot.
const MAX_TEXT: usize = 6_000;

type Pending = Arc<std::sync::Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>>;

/// A CDP connection to one page.
pub struct Page {
    out: mpsc::UnboundedSender<String>,
    pending: Pending,
    /// CDP command ids; shared with the screencast acker so its acks get
    /// unique ids too.
    next_id: Arc<AtomicU64>,
    console: Arc<std::sync::Mutex<VecDeque<String>>>,
    /// Debug port, for status and new tabs.
    pub port: u16,
    /// The tab (CDP target) this connection drives.
    pub target_id: String,
    alive: Arc<std::sync::atomic::AtomicBool>,
    /// The last snapshot sent to the model, to report only what changed.
    last: std::sync::Mutex<Option<Value>>,
    /// While recording: where screencast frames go.
    screencast: ScreencastSink,
}

type ScreencastSink = Arc<std::sync::Mutex<Option<mpsc::UnboundedSender<Value>>>>;

/// Acknowledges screencast frames (the browser sends the next one only
/// after an ack).
pub struct ScreencastAcker {
    out: mpsc::UnboundedSender<String>,
    next_id: Arc<AtomicU64>,
    pending: Pending,
    /// The page's CSS width, looked up now and then (not every frame).
    css_w: std::sync::Mutex<(f64, Option<std::time::Instant>)>,
}

impl ScreencastAcker {
    pub fn ack(&self, session: u64) {
        // Takes the next command id, so it never clashes with a command;
        // nothing waits on it, so its reply is ignored.
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let _ = self.out.send(screencast_ack_message(id, session));
    }

    /// A CDP call on the recorded page without taking the shared browser
    /// lock (the agent's own actions hold it while they run).
    async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| "lock".to_string())?
            .insert(id, tx);
        let msg = json!({"id": id, "method": method, "params": params}).to_string();
        self.out
            .send(msg)
            .map_err(|_| "the browser connection closed".to_string())?;
        match tokio::time::timeout(Duration::from_secs(3), rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err("the browser connection closed".into()),
            Err(_) => {
                if let Ok(mut p) = self.pending.lock() {
                    p.remove(&id);
                }
                Err(format!("the browser did not answer {method}"))
            }
        }
    }

    /// One frame of the page as JPEG, and the page's width in CSS pixels —
    /// for when the screencast goes quiet (Chrome sends no screencast frames
    /// for a tab it thinks is hidden, e.g. a window Windows counts as
    /// covered).
    pub async fn capture(&self) -> Result<(Vec<u8>, f64), String> {
        let shot = self
            .call(
                "Page.captureScreenshot",
                json!({"format": "jpeg", "quality": 80}),
            )
            .await?;
        let data = shot["data"].as_str().ok_or("no screenshot data")?;
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(data)
            .map_err(|e| e.to_string())?;
        let cached = self.css_w.lock().map(|c| *c).unwrap_or((0.0, None));
        let css_w = match cached {
            (w, Some(at)) if at.elapsed() < Duration::from_secs(3) => w,
            (old, _) => {
                let w = self
                    .call("Page.getLayoutMetrics", json!({}))
                    .await
                    .ok()
                    .and_then(|m| {
                        m["cssVisualViewport"]["clientWidth"]
                            .as_f64()
                            .or_else(|| m["layoutViewport"]["clientWidth"].as_f64())
                    })
                    .unwrap_or(old);
                if let Ok(mut c) = self.css_w.lock() {
                    *c = (w, Some(std::time::Instant::now()));
                }
                w
            }
        };
        Ok((bytes, css_w))
    }
}

/// The `Page.screencastFrameAck` message. CDP message ids must fit in a
/// 32-bit int: Chrome drops a message with a bigger id, and an unacked
/// screencast stops after two frames.
fn screencast_ack_message(id: u64, session: u64) -> String {
    json!({
        "id": id,
        "method": "Page.screencastFrameAck",
        "params": {"sessionId": session}
    })
    .to_string()
}

/// Starts the screencast of the page the agent drives (launching the
/// browser if needed); frames go to `tx`.
pub async fn start_screencast(
    tx: mpsc::UnboundedSender<Value>,
) -> Result<(ScreencastAcker, bool), String> {
    let mut slot = shared().lock().await;
    let (page, _) = ensure_page(&mut slot, true).await?;
    // Bring the tab forward and keep it "focused": Chrome only paints (and
    // screencasts) a tab it considers visible.
    let _ = page.call("Page.bringToFront", json!({})).await;
    let _ = page
        .call(
            "Emulation.setFocusEmulationEnabled",
            json!({"enabled": true}),
        )
        .await;
    let visible = page
        .eval("document.visibilityState")
        .await
        .map(|v| v.as_str() != Some("hidden"))
        .unwrap_or(true);
    if let Ok(mut s) = page.screencast.lock() {
        *s = Some(tx);
    }
    page.call(
        "Page.startScreencast",
        json!({"format": "jpeg", "quality": 80, "maxWidth": 1920, "maxHeight": 1080, "everyNthFrame": 1}),
    )
    .await?;
    Ok((
        ScreencastAcker {
            out: page.out.clone(),
            next_id: page.next_id.clone(),
            pending: page.pending.clone(),
            css_w: std::sync::Mutex::new((0.0, None)),
        },
        visible,
    ))
}

/// Stops the screencast.
pub async fn stop_screencast() -> Result<(), String> {
    let slot = shared().lock().await;
    let page = slot.as_ref().ok_or("no browser")?;
    let r = page.call("Page.stopScreencast", json!({})).await;
    if let Ok(mut s) = page.screencast.lock() {
        *s = None;
    }
    r.map(|_| ())
}

/// Where the last click or typing happened (CSS pixels), for the recording.
static LAST_XY: std::sync::Mutex<Option<(f64, f64)>> = std::sync::Mutex::new(None);
/// The size of that element (CSS pixels).
static LAST_BOX: std::sync::Mutex<Option<(f64, f64)>> = std::sync::Mutex::new(None);

/// Finds what must not be shown in a recording: emails, ticket ids
/// (`ABC-123`), API keys and tokens, password and email fields, and the
/// `extra` words. Boxes in CSS pixels of the viewport.
const PRIVATE_JS: &str = r#"((extra) => {
  const esc = s => s.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const pats = [
    /[\w.+-]+@[\w-]+(\.[\w-]+)+/g,
    /\b[A-Z][A-Z0-9]{1,9}-\d{1,7}\b/g,
    /\b(?:sk|pk|rk|ghp|gho|ghs|github_pat|xox[abps]|AKIA)[-_A-Za-z0-9]{12,}\b/g,
    /\beyJ[\w-]{10,}\.[\w-]{10,}\.[\w-]{5,}/g,
  ];
  for (const w of extra) if (w && w.trim()) pats.push(new RegExp(esc(w.trim()), 'gi'));
  const out = [], vw = innerWidth, vh = innerHeight;
  const add = r => { if (r.width > 0 && r.height > 0 && r.bottom > 0 && r.right > 0 &&
    r.top < vh && r.left < vw && out.length < 400) out.push([r.left, r.top, r.width, r.height]); };
  if (!document.body) return out;
  const tw = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  let n;
  while ((n = tw.nextNode()) && out.length < 400) {
    const t = n.nodeValue; if (!t || t.length < 3) continue;
    for (const p of pats) { p.lastIndex = 0; let m;
      while ((m = p.exec(t)) && out.length < 400) {
        const rg = document.createRange(); rg.setStart(n, m.index); rg.setEnd(n, m.index + m[0].length);
        for (const r of rg.getClientRects()) add(r);
        if (!p.global) break; } }
  }
  for (const el of document.querySelectorAll('input[type=password],input[type=email],input[autocomplete*=email]'))
    add(el.getBoundingClientRect());
  for (const el of document.querySelectorAll('input,textarea')) {
    const v = el.value || ''; if (v && pats.some(p => { p.lastIndex = 0; return p.test(v); })) add(el.getBoundingClientRect());
  }
  return out;
})"#;

/// Where the page's main content is (CSS pixels of the viewport: x, y, w,
/// h): the area-weighted 3–97 % extent of visible text and images, leaving
/// out fixed and sticky bars, navigation, headers and footers — so a page
/// with wide empty margins can be framed on what matters.
const FOCUS_JS: &str = r#"(() => {
  const vw = innerWidth, vh = innerHeight, B = 8;
  const nx = Math.ceil(vw / B) + 1, ny = Math.ceil(vh / B) + 1;
  const cx = new Float64Array(nx), cy = new Float64Array(ny);
  let total = 0;
  const chrome = new Map();
  const isChrome = el => {
    const path = [];
    for (let e = el; e && e !== document.body && e !== document.documentElement; e = e.parentElement) {
      if (chrome.has(e)) { const v = chrome.get(e); path.forEach(p => chrome.set(p, v)); return v; }
      path.push(e);
      const s = getComputedStyle(e), t = e.tagName, r = e.getAttribute('role');
      if (s.position === 'fixed' || s.position === 'sticky' || t === 'NAV' || t === 'HEADER' ||
          t === 'FOOTER' || t === 'ASIDE' || r === 'navigation' || r === 'banner' || r === 'contentinfo') {
        path.forEach(p => chrome.set(p, true)); return true; }
    }
    path.forEach(p => chrome.set(p, false)); return false;
  };
  const add = r => {
    const l = Math.max(0, r.left), t = Math.max(0, r.top), rr = Math.min(vw, r.right), b = Math.min(vh, r.bottom);
    if (rr - l < 2 || b - t < 2) return;
    const a = (rr - l) * (b - t); total += a;
    const h = b - t, w = rr - l;
    for (let i = Math.floor(l / B); i <= Math.floor(rr / B); i++) cx[i] += h * B;
    for (let j = Math.floor(t / B); j <= Math.floor(b / B); j++) cy[j] += w * B;
  };
  if (!document.body) return null;
  const tw = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  let n, seen = 0;
  while ((n = tw.nextNode()) && seen < 4000) {
    if (!n.nodeValue || n.nodeValue.trim().length < 2 || !n.parentElement) continue;
    seen++;
    if (isChrome(n.parentElement)) continue;
    const rg = document.createRange(); rg.selectNodeContents(n);
    for (const r of rg.getClientRects()) add(r);
  }
  for (const el of document.querySelectorAll('img,svg,canvas,video')) {
    const r = el.getBoundingClientRect();
    if (r.width < 24 || r.height < 24 || isChrome(el)) continue;
    add(r);
  }
  if (total < vw * vh * 0.01) return null;
  const span = (arr, B) => {
    const sum = arr.reduce((a, b) => a + b, 0); let acc = 0, lo = 0, hi = arr.length - 1;
    for (let i = 0; i < arr.length; i++) { acc += arr[i]; if (acc >= sum * 0.03) { lo = i; break; } }
    acc = 0;
    for (let i = arr.length - 1; i >= 0; i--) { acc += arr[i]; if (acc >= sum * 0.03) { hi = i; break; } }
    return [lo * B, (hi + 1) * B];
  };
  const [x0, x1] = span(cx, B), [y0, y1] = span(cy, B);
  return [x0, y0, Math.min(vw, x1) - x0, Math.min(vh, y1) - y0];
})()"#;

/// Waits until the page has settled after an action: loaded, then two
/// screenshots a quarter second apart identical (at most `max`).
async fn settle(page: &Page, max: Duration) {
    let start = std::time::Instant::now();
    let shot = || async {
        page.call(
            "Page.captureScreenshot",
            json!({"format": "jpeg", "quality": 30}),
        )
        .await
        .ok()
        .and_then(|v| v["data"].as_str().map(str::to_string))
    };
    let mut last = shot().await;
    while start.elapsed() < max {
        tokio::time::sleep(Duration::from_millis(250)).await;
        let now = shot().await;
        if now.is_some() && now == last {
            return;
        }
        last = now;
    }
}

/// What a recording needs from the page after an action: it settles, its
/// private parts are found, and in stills mode a still is taken.
async fn observe_for_recording(page: &Page) {
    observe_settled(page, Duration::from_secs(4)).await;
}

/// Stills mode, just before a click or typing (after the element was
/// scrolled into view): a still of the page as it is, so the marker and the
/// zoom land on the element — not on the page the click opens.
async fn still_before_action(page: &Page) {
    if crate::video::record::mode() == Some(crate::video::record::Mode::Stills) {
        observe_settled(page, Duration::from_millis(1500)).await;
    }
}

async fn observe_settled(page: &Page, max: Duration) {
    use crate::video::record;
    let Some(mode) = record::mode() else { return };
    settle(page, max).await;
    if let Ok(Value::Array(b)) = page.eval(FOCUS_JS).await
        && let [Some(x), Some(y), Some(w), Some(h)] =
            [0, 1, 2, 3].map(|i| b.get(i).and_then(Value::as_f64))
    {
        record::add_focus([x, y, w, h]);
    }
    let words = serde_json::to_string(&record::redact_words()).unwrap_or("[]".into());
    if let Ok(Value::Array(boxes)) = page.eval(&format!("{PRIVATE_JS}({words})")).await {
        let boxes: Vec<[f64; 4]> = boxes
            .iter()
            .filter_map(|b| {
                let a = b.as_array()?;
                Some([
                    a.first()?.as_f64()?,
                    a.get(1)?.as_f64()?,
                    a.get(2)?.as_f64()?,
                    a.get(3)?.as_f64()?,
                ])
            })
            .collect();
        record::add_privacy(boxes);
    }
    if mode == record::Mode::Stills
        && let Ok(shot) = page
            .call(
                "Page.captureScreenshot",
                json!({"format": "jpeg", "quality": 88}),
            )
            .await
        && let Some(data) = shot["data"].as_str()
    {
        use base64::Engine;
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) {
            let css_w = page
                .eval("innerWidth")
                .await
                .ok()
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            record::add_still(&bytes, css_w);
        }
    }
}

/// The viewport a recording is made at: 1440×810 CSS pixels drawn at
/// 1920×1080, whatever the window size or browser zoom — so a page in a
/// wide or zoomed-out window does not come out as a small strip of tiny
/// text in the middle of the frame.
const RECORD_CSS: (f64, f64) = (1440.0, 810.0);
const RECORD_PX: f64 = 1920.0;

/// Sets the recording viewport on the page the agent drives.
pub async fn fit_recording_viewport() -> Result<(), String> {
    let mut slot = shared().lock().await;
    let (page, _) = ensure_page(&mut slot, true).await?;
    let set = |w: f64, h: f64| {
        page.call(
            "Emulation.setDeviceMetricsOverride",
            json!({"width": w.round() as u64, "height": h.round() as u64,
                   "deviceScaleFactor": RECORD_PX / w, "mobile": false}),
        )
    };
    let (cw, ch) = RECORD_CSS;
    set(cw, ch).await?;
    // Browser zoom makes CSS pixels differ from window pixels: measure and
    // correct once.
    if let Some(iw) = page.eval("innerWidth").await.ok().and_then(|v| v.as_f64())
        && iw > 0.0
        && (iw - cw).abs() > 2.0
    {
        let k = cw / iw;
        set(cw * k, ch * k).await?;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    Ok(())
}

/// Back to the window's own viewport after a recording.
pub async fn restore_viewport() {
    let slot = shared().lock().await;
    if let Some(page) = slot.as_ref() {
        let _ = page
            .call("Emulation.clearDeviceMetricsOverride", json!({}))
            .await;
    }
}

/// [`observe_for_recording`] on the page the agent drives (at the start of
/// a recording).
pub async fn observe_now() -> Result<(), String> {
    let mut slot = shared().lock().await;
    let (page, _) = ensure_page(&mut slot, true).await?;
    let _ = page.call("Page.bringToFront", json!({})).await;
    observe_for_recording(page).await;
    Ok(())
}

impl Page {
    /// Connect to a page's WebSocket debugger URL.
    async fn connect(ws_url: &str, port: u16) -> Result<Self, String> {
        let (ws, _) = tokio_tungstenite::connect_async(ws_url)
            .await
            .map_err(|e| format!("could not connect to the browser: {e}"))?;
        let (mut sink, mut stream) = ws.split();
        let (out, mut out_rx) = mpsc::unbounded_channel::<String>();
        let pending: Pending = Default::default();
        let console: Arc<std::sync::Mutex<VecDeque<String>>> = Default::default();
        let screencast: ScreencastSink = Default::default();
        let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        tokio::spawn(async move {
            while let Some(text) = out_rx.recv().await {
                if sink.send(WsMessage::Text(text.into())).await.is_err() {
                    break;
                }
            }
        });
        {
            let pending = pending.clone();
            let console = console.clone();
            let alive = alive.clone();
            let screencast = screencast.clone();
            tokio::spawn(async move {
                while let Some(Ok(msg)) = stream.next().await {
                    let WsMessage::Text(text) = msg else {
                        continue;
                    };
                    let Ok(v) = serde_json::from_str::<Value>(&text) else {
                        continue;
                    };
                    if let Some(id) = v["id"].as_u64() {
                        let tx = pending.lock().ok().and_then(|mut p| p.remove(&id));
                        if let Some(tx) = tx {
                            let r = match v.get("error") {
                                Some(e) => {
                                    Err(e["message"].as_str().unwrap_or("CDP error").to_string())
                                }
                                None => Ok(v["result"].clone()),
                            };
                            let _ = tx.send(r);
                        }
                    } else if v["method"] == "Page.screencastFrame" {
                        if let Ok(s) = screencast.lock()
                            && let Some(tx) = s.as_ref()
                        {
                            let _ = tx.send(v);
                        }
                    } else if let Some(line) = console_line(&v)
                        && let Ok(mut c) = console.lock()
                    {
                        if c.len() >= CONSOLE_KEEP {
                            c.pop_front();
                        }
                        c.push_back(line);
                    }
                }
                alive.store(false, Ordering::Relaxed);
                // Fail anything still waiting.
                if let Ok(mut p) = pending.lock() {
                    for (_, tx) in p.drain() {
                        let _ = tx.send(Err("the browser connection closed".into()));
                    }
                }
            });
        }
        let page = Self {
            out,
            pending,
            next_id: Arc::new(AtomicU64::new(1)),
            console,
            port,
            target_id: ws_url.rsplit('/').next().unwrap_or_default().to_string(),
            alive,
            last: std::sync::Mutex::new(None),
            screencast,
        };
        for domain in ["Page.enable", "Runtime.enable", "Log.enable"] {
            page.call(domain, json!({})).await?;
        }
        Ok(page)
    }

    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    /// Send a CDP command and wait for its result.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .map_err(|_| "lock".to_string())?
            .insert(id, tx);
        let msg = json!({"id": id, "method": method, "params": params}).to_string();
        self.out
            .send(msg)
            .map_err(|_| "the browser connection closed".to_string())?;
        match tokio::time::timeout(Duration::from_secs(30), rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err("the browser connection closed".into()),
            Err(_) => Err(format!("the browser did not answer {method} in 30s")),
        }
    }

    /// Evaluate JavaScript in the page and return its value.
    pub async fn eval(&self, expression: &str) -> Result<Value, String> {
        let r = self
            .call(
                "Runtime.evaluate",
                json!({"expression": expression, "returnByValue": true, "awaitPromise": true}),
            )
            .await?;
        if let Some(ex) = r.get("exceptionDetails") {
            let text = ex["exception"]["description"]
                .as_str()
                .or_else(|| ex["text"].as_str())
                .unwrap_or("script error");
            return Err(text.lines().next().unwrap_or(text).to_string());
        }
        Ok(r["result"]["value"].clone())
    }

    /// Wait until the page has loaded (or 15 s pass), plus a short settle.
    async fn wait_loaded(&self) {
        let start = std::time::Instant::now();
        while start.elapsed() < Duration::from_secs(15) {
            tokio::time::sleep(Duration::from_millis(250)).await;
            match self.eval("document.readyState").await {
                Ok(Value::String(s)) if s == "complete" => break,
                Ok(Value::String(s))
                    if s == "interactive" && start.elapsed() > Duration::from_secs(4) =>
                {
                    break;
                }
                _ => {}
            }
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }

    fn console_lines(&self) -> Vec<String> {
        self.console
            .lock()
            .map(|c| c.iter().cloned().collect())
            .unwrap_or_default()
    }
}

/// A console message or uncaught error as one line.
fn console_line(v: &Value) -> Option<String> {
    let p = &v["params"];
    match v["method"].as_str()? {
        "Runtime.consoleAPICalled" => {
            let args: Vec<String> = p["args"]
                .as_array()?
                .iter()
                .map(|a| {
                    a["value"]
                        .as_str()
                        .map(str::to_string)
                        .or_else(|| a.get("value").map(|v| v.to_string()))
                        .or_else(|| a["description"].as_str().map(str::to_string))
                        .unwrap_or_default()
                })
                .collect();
            Some(format!(
                "[{}] {}",
                p["type"].as_str().unwrap_or("log"),
                args.join(" ")
            ))
        }
        "Runtime.exceptionThrown" => {
            let d = &p["exceptionDetails"];
            let text = d["exception"]["description"]
                .as_str()
                .or_else(|| d["text"].as_str())
                .unwrap_or("error");
            Some(format!("[error] {}", text.lines().next().unwrap_or(text)))
        }
        "Log.entryAdded" => {
            let e = &p["entry"];
            Some(format!(
                "[{}] {}{}",
                e["level"].as_str().unwrap_or("log"),
                e["text"].as_str().unwrap_or(""),
                e["url"]
                    .as_str()
                    .filter(|u| !u.is_empty())
                    .map(|u| format!(" ({u})"))
                    .unwrap_or_default()
            ))
        }
        _ => None,
    }
}

/// The one page this process drives (shared by the tool and `/browser`).
/// URL of the page the model last saw (for `/rewind`).
fn last_url_cell() -> &'static std::sync::Mutex<Option<String>> {
    static URL: OnceLock<std::sync::Mutex<Option<String>>> = OnceLock::new();
    URL.get_or_init(Default::default)
}

/// The page the agent was last on, if it used the browser this process.
pub fn last_url() -> Option<String> {
    last_url_cell().lock().ok().and_then(|u| u.clone())
}

fn note_url(v: &Value) {
    if let (Some(url), Ok(mut cell)) = (v["url"].as_str(), last_url_cell().lock())
        && !url.is_empty()
    {
        *cell = Some(url.to_string());
    }
}

pub fn shared() -> &'static Mutex<Option<Page>> {
    static PAGE: OnceLock<Mutex<Option<Page>>> = OnceLock::new();
    PAGE.get_or_init(|| Mutex::new(None))
}

fn is_wsl() -> bool {
    std::env::var_os("WSL_DISTRO_NAME").is_some()
}

/// The browser executable to launch: `DCODE_AI_BROWSER`, else Chrome, Edge,
/// Chromium or Brave where they usually live (the Windows ones from WSL).
pub fn find_browser() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("DCODE_AI_BROWSER") {
        return Some(PathBuf::from(p));
    }
    let on_path = |name: &str| {
        std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths)
                .map(|d| d.join(name))
                .find(|p| p.is_file())
        })
    };
    let mut candidates: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        for app in [
            "Google Chrome.app/Contents/MacOS/Google Chrome",
            "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            "Chromium.app/Contents/MacOS/Chromium",
            "Brave Browser.app/Contents/MacOS/Brave Browser",
        ] {
            candidates.push(Path::new("/Applications").join(app));
        }
    } else if cfg!(windows) {
        for base in ["C:\\Program Files", "C:\\Program Files (x86)"] {
            candidates.push(Path::new(base).join("Google\\Chrome\\Application\\chrome.exe"));
            candidates.push(Path::new(base).join("Microsoft\\Edge\\Application\\msedge.exe"));
        }
    } else {
        for name in [
            "google-chrome",
            "google-chrome-stable",
            "chromium",
            "chromium-browser",
            "microsoft-edge",
            "brave-browser",
        ] {
            if let Some(p) = on_path(name) {
                return Some(p);
            }
        }
        if is_wsl() {
            for base in ["/mnt/c/Program Files", "/mnt/c/Program Files (x86)"] {
                candidates.push(Path::new(base).join("Google/Chrome/Application/chrome.exe"));
                candidates.push(Path::new(base).join("Microsoft/Edge/Application/msedge.exe"));
            }
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// Run a command and return its trimmed stdout.
fn output_of(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd)
        .args(args)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !s.is_empty()).then_some(s)
}

/// The profile folder for dcode-ai's browser, as the browser expects it
/// (a Windows path for a Windows browser started from WSL).
fn profile_dir(browser: &Path) -> Option<String> {
    let windows_exe = browser
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("exe"));
    if is_wsl() && windows_exe {
        let local = output_of("cmd.exe", &["/c", "echo %LOCALAPPDATA%"])?;
        let unix = output_of("wslpath", &["-u", &local])?;
        let dir = Path::new(&unix).join("dcode-ai").join("browser-profile");
        std::fs::create_dir_all(&dir).ok()?;
        return output_of("wslpath", &["-w", &dir.to_string_lossy()]);
    }
    let dir = dcode_ai_common::config::dcode_ai_home_dir()?.join("browser-profile");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.to_string_lossy().to_string())
}

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_default()
}

/// Whether something answers CDP on `port`.
pub(crate) async fn port_alive(port: u16) -> bool {
    http()
        .get(format!("http://127.0.0.1:{port}/json/version"))
        .send()
        .await
        .is_ok_and(|r| r.status().is_success())
}

/// Launch the browser on `port` (visible, own profile) and wait for CDP.
pub async fn launch(port: u16, url: Option<&str>) -> Result<String, String> {
    let exe = find_browser().ok_or(
        "no Chrome, Edge, Chromium or Brave found — install one, or set DCODE_AI_BROWSER to its path",
    )?;
    let profile = profile_dir(&exe).ok_or("could not create the browser profile folder")?;
    let mut cmd = std::process::Command::new(&exe);
    cmd.arg(format!("--remote-debugging-port={port}"))
        .arg(format!("--user-data-dir={profile}"))
        .args(["--no-first-run", "--no-default-browser-check"])
        // Keep painting when the window is covered or in the background:
        // otherwise Windows' occlusion tracking hides the tab and screen
        // recordings get no frames.
        .args([
            "--disable-features=CalculateNativeWinOcclusion",
            // Screenshots from a fresh surface: they work even while the
            // window is covered (the recorder's fallback uses them).
            "--enable-features=CDPScreenshotNewSurface",
            "--disable-backgrounding-occluded-windows",
            "--disable-renderer-backgrounding",
            "--disable-background-timer-throttling",
        ])
        .arg(url.unwrap_or("about:blank"))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", exe.display()))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    for _ in 0..60 {
        if port_alive(port).await {
            return Ok(exe.display().to_string());
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err(format!(
        "{} started but its debug port {port} did not answer{}",
        exe.display(),
        if is_wsl() {
            " — on WSL this needs mirrored networking (networkingMode=mirrored in .wslconfig)"
        } else {
            ""
        }
    ))
}

/// One browser tab.
#[derive(Debug, Clone, PartialEq)]
pub struct Tab {
    pub id: String,
    pub title: String,
    pub url: String,
    ws: String,
}

/// The browser's own pages (omnibox popup, devtools, settings).
fn is_browser_ui(url: &str) -> bool {
    [
        "devtools://",
        "chrome://",
        "chrome-untrusted://",
        "edge://",
        "brave://",
    ]
    .iter()
    .any(|s| url.starts_with(s))
}

/// A tab with nothing in it yet.
fn is_blank(url: &str) -> bool {
    url.is_empty()
        || url == "about:blank"
        || url.contains("://newtab")
        || url.contains("new-tab-page")
}

/// Open tabs, most recently used first (the order the browser reports).
pub async fn list_tabs(port: u16) -> Result<Vec<Tab>, String> {
    let list: Value = http()
        .get(format!("http://127.0.0.1:{port}/json/list"))
        .send()
        .await
        .map_err(|e| format!("no browser on port {port}: {e}"))?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    Ok(tabs_from_list(&list))
}

/// Tabs from the browser's `/json/list` answer.
fn tabs_from_list(list: &Value) -> Vec<Tab> {
    list.as_array()
        .map(|pages| {
            pages
                .iter()
                .filter(|p| p["type"] == "page")
                .filter_map(|p| {
                    let url = p["url"].as_str().unwrap_or("").to_string();
                    // A new-tab page counts (it is where the user is); other
                    // browser UI pages do not.
                    if is_browser_ui(&url) && !is_blank(&url) {
                        return None;
                    }
                    Some(Tab {
                        id: p["id"].as_str()?.to_string(),
                        title: p["title"].as_str().unwrap_or("").to_string(),
                        url,
                        ws: p["webSocketDebuggerUrl"].as_str()?.to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Connect to `tab` and bring it to the front.
async fn attach_tab(port: u16, tab: &Tab) -> Result<Page, String> {
    let _ = http()
        .get(format!("http://127.0.0.1:{port}/json/activate/{}", tab.id))
        .send()
        .await;
    Page::connect(&tab.ws, port).await
}

/// Open a new tab at `url` and connect to it.
async fn open_tab(port: u16, url: &str) -> Result<Page, String> {
    let new: Value = http()
        .put(format!("http://127.0.0.1:{port}/json/new?{url}"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let ws = new["webSocketDebuggerUrl"]
        .as_str()
        .ok_or("the browser did not open a tab")?;
    Page::connect(ws, port).await
}

/// Attach to the browser on `port`: the most recent tab with something in
/// it, else the most recent tab, else a new one.
pub async fn attach(port: u16) -> Result<Page, String> {
    let tabs = list_tabs(port).await?;
    match tabs
        .iter()
        .find(|t| !is_blank(&t.url))
        .or_else(|| tabs.first())
    {
        Some(tab) => Page::connect(&tab.ws, port).await,
        None => open_tab(port, "about:blank").await,
    }
}

/// The connected page, launching dcode-ai's browser first if needed. When
/// the tab it drives was closed or left blank while the user opened
/// another one, it follows the user's tab. Returns a note when it moved.
async fn ensure_page(
    slot: &mut Option<Page>,
    launch_if_needed: bool,
) -> Result<(&Page, Option<String>), String> {
    if slot.as_ref().is_some_and(|p| !p.is_alive()) {
        *slot = None;
    }
    let mut note = None;
    match slot.as_ref() {
        // An app target never launches the browser.
        None if !launch_if_needed => {
            return Err("that app isn't connected any more — /app open it again".into());
        }
        None => {
            if !port_alive(DEFAULT_PORT).await {
                launch(DEFAULT_PORT, None).await?;
            }
            *slot = Some(attach(DEFAULT_PORT).await?);
        }
        Some(page) => {
            let port = page.port;
            if let Ok(tabs) = list_tabs(port).await {
                let ours = tabs.iter().find(|t| t.id == page.target_id);
                let stale = ours.is_none_or(|t| is_blank(&t.url));
                if stale
                    && let Some(other) = tabs
                        .iter()
                        .find(|t| t.id != page.target_id && !is_blank(&t.url))
                {
                    note = Some(format!(
                        "(Now on the tab the user is using: {} — {})",
                        other.title, other.url
                    ));
                    *slot = Some(attach_tab(port, other).await?);
                }
            }
        }
    }
    Ok((slot.as_ref().expect("set above"), note))
}

/// Script that numbers the visible interactive elements (`data-dcode-ref`)
/// and returns the page as JSON: title, URL, elements, text.
const SNAPSHOT_JS: &str = r#"(() => {
  const vis = el => { const r = el.getBoundingClientRect(); const s = getComputedStyle(el);
    return r.width > 0 && r.height > 0 && s.visibility !== 'hidden' && s.display !== 'none'; };
  // Refs are stable while the page lives: an element keeps its number.
  let next = window.__dcodeNext || 0;
  const sel = 'a[href],button,input:not([type=hidden]),select,textarea,summary,[role=button],[role=link],[role=checkbox],[role=radio],[role=tab],[role=menuitem],[role=option],[role=switch],[contenteditable=""],[contenteditable=true],[onclick]';
  const out = []; let n = 0; let more = false;
  for (const el of document.querySelectorAll(sel)) {
    if (!vis(el) || el.disabled) continue;
    if (n >= MAX) { more = true; break; }
    n++;
    let ref = el.getAttribute('data-dcode-ref');
    if (!ref) { ref = String(++next); el.setAttribute('data-dcode-ref', ref); }
    const tag = el.tagName.toLowerCase();
    const role = el.getAttribute('role') || (tag === 'a' ? 'link' : tag);
    const name = (el.getAttribute('aria-label') || el.innerText || el.placeholder || el.title || el.getAttribute('name') || el.alt || '')
      .replace(/\s+/g, ' ').trim().slice(0, 80);
    let extra = '';
    if (tag === 'input') {
      const t = el.type || 'text'; extra = ` type=${t}`;
      if (t === 'checkbox' || t === 'radio') extra += el.checked ? ' checked' : '';
      else if (t !== 'password' && el.value) extra += ` value="${String(el.value).slice(0, 40)}"`;
    } else if (tag === 'textarea' && el.value) {
      extra = ` value="${el.value.slice(0, 40)}"`;
    } else if (tag === 'select') {
      extra = ` value="${(el.options[el.selectedIndex] || {}).text || ''}"`;
    } else if (tag === 'a') {
      const h = el.getAttribute('href') || '';
      if (h && !h.startsWith('javascript')) extra = ` → ${h.slice(0, 80)}`;
    }
    out.push(`[${ref}] ${role}${name ? ` "${name}"` : ''}${extra}`);
  }
  window.__dcodeNext = next;
  const text = (document.body ? document.body.innerText : '').replace(/\n{3,}/g, '\n\n');
  return JSON.stringify({ title: document.title, url: location.href, elements: out, more, text: text.slice(0, TEXT), cut: text.length > TEXT });
})()"#;

async fn snapshot_value(page: &Page) -> Result<Value, String> {
    let js = SNAPSHOT_JS
        .replace("MAX", &MAX_ELEMENTS.to_string())
        .replace("TEXT", &MAX_TEXT.to_string());
    let raw = page.eval(&js).await?;
    serde_json::from_str(raw.as_str().unwrap_or("{}")).map_err(|e| e.to_string())
}

/// The whole page as text for the model.
async fn snapshot(page: &Page) -> Result<String, String> {
    let v = snapshot_value(page).await?;
    note_url(&v);
    let text = format_snapshot(&v);
    if let Ok(mut last) = page.last.lock() {
        *last = Some(v);
    }
    Ok(text)
}

/// After an action: only what changed when still on the same page (most
/// of a page stays the same after a click), the whole page otherwise.
async fn changes(page: &Page) -> Result<String, String> {
    let v = snapshot_value(page).await?;
    note_url(&v);
    let prev = page.last.lock().ok().and_then(|mut l| l.replace(v.clone()));
    Ok(match prev {
        Some(prev) if prev["url"] == v["url"] => format_changes(&prev, &v),
        _ => format_snapshot(&v),
    })
}

/// `find`: only the parts of the page that mention `query`.
async fn find(page: &Page, query: &str) -> Result<String, String> {
    let js = SNAPSHOT_JS.replace("MAX", "1000").replace("TEXT", "200000");
    let raw = page.eval(&js).await?;
    let v: Value = serde_json::from_str(raw.as_str().unwrap_or("{}")).map_err(|e| e.to_string())?;
    note_url(&v);
    Ok(format_find(&v, query))
}

/// Most matches `find` lists (elements, and text lines).
const MAX_FIND: usize = 30;

fn format_find(v: &Value, query: &str) -> String {
    let q = query.trim().to_lowercase();
    let mut out = format!(
        "Page: {}\nURL: {}\nMatches for {query:?}:\n",
        v["title"].as_str().unwrap_or(""),
        v["url"].as_str().unwrap_or("")
    );
    let elements: Vec<&str> = v["elements"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let hits: Vec<&&str> = elements
        .iter()
        .filter(|e| e.to_lowercase().contains(&q))
        .collect();
    if !hits.is_empty() {
        out.push_str("\nElements:\n");
        for e in hits.iter().take(MAX_FIND) {
            out.push_str(e);
            out.push('\n');
        }
        if hits.len() > MAX_FIND {
            out.push_str(&format!("… {} more\n", hits.len() - MAX_FIND));
        }
    }
    let lines: Vec<&str> = v["text"]
        .as_str()
        .unwrap_or("")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let text_hits: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| l.to_lowercase().contains(&q))
        .collect();
    if !text_hits.is_empty() {
        out.push_str("\nText:\n");
        for l in text_hits.iter().take(MAX_FIND) {
            let clipped: String = l.chars().take(300).collect();
            out.push_str(&clipped);
            out.push('\n');
        }
        if text_hits.len() > MAX_FIND {
            out.push_str(&format!("… {} more\n", text_hits.len() - MAX_FIND));
        }
    }
    if hits.is_empty() && text_hits.is_empty() {
        out.push_str("\nNothing on the page mentions that. `snapshot` shows the whole page.\n");
    }
    out
}

/// Most changed text lines listed after an action.
const MAX_CHANGED_LINES: usize = 40;

fn format_changes(old: &Value, new: &Value) -> String {
    let list = |v: &Value, key: &str| -> Vec<String> {
        v[key]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    };
    let lines = |v: &Value| -> Vec<String> {
        v["text"]
            .as_str()
            .unwrap_or("")
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
            .collect()
    };
    let (old_el, new_el) = (list(old, "elements"), list(new, "elements"));
    let added: Vec<&String> = new_el.iter().filter(|e| !old_el.contains(e)).collect();
    let removed = old_el.iter().filter(|e| !new_el.contains(e)).count();
    let old_text: std::collections::HashSet<String> = lines(old).into_iter().collect();
    let new_lines: Vec<String> = lines(new)
        .into_iter()
        .filter(|l| !old_text.contains(l))
        .collect();

    let mut out = format!(
        "Page: {} (same page)\nURL: {}\n",
        new["title"].as_str().unwrap_or(""),
        new["url"].as_str().unwrap_or("")
    );
    if added.is_empty() && removed == 0 && new_lines.is_empty() {
        out.push_str("\nNo visible change. `snapshot` shows the whole page.\n");
        return out;
    }
    if !added.is_empty() {
        out.push_str("\nNew or changed elements:\n");
        for e in &added {
            out.push_str(e);
            out.push('\n');
        }
    }
    if removed > 0 {
        out.push_str(&format!("({removed} element(s) gone)\n"));
    }
    if !new_lines.is_empty() {
        out.push_str("\nNew text:\n");
        for l in new_lines.iter().take(MAX_CHANGED_LINES) {
            out.push_str(l);
            out.push('\n');
        }
        if new_lines.len() > MAX_CHANGED_LINES {
            out.push_str(&format!(
                "… {} more line(s); `snapshot` shows the whole page\n",
                new_lines.len() - MAX_CHANGED_LINES
            ));
        }
    }
    out.push_str("\nOther elements keep their numbers; `snapshot` lists them all.\n");
    out
}

fn format_snapshot(v: &Value) -> String {
    let mut out = format!(
        "Page: {}\nURL: {}\n",
        v["title"].as_str().unwrap_or(""),
        v["url"].as_str().unwrap_or("")
    );
    let elements: Vec<&str> = v["elements"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if elements.is_empty() {
        out.push_str("\nNo interactive elements visible.\n");
    } else {
        out.push_str("\nInteractive elements (use the [number] as `ref` for click / type):\n");
        for e in &elements {
            out.push_str(e);
            out.push('\n');
        }
        if v["more"].as_bool() == Some(true) {
            out.push_str("… more elements below; scroll to see them\n");
        }
    }
    let text = v["text"].as_str().unwrap_or("").trim();
    if !text.is_empty() {
        out.push_str("\nPage text:\n");
        out.push_str(text);
        out.push('\n');
        if v["cut"].as_bool() == Some(true) {
            out.push_str("… [page text cut; scroll for more]\n");
        }
    }
    out
}

/// Scroll element `r` into view; returns its centre and whether it is a
/// password field (its size goes to [`LAST_BOX`], for recordings).
async fn locate(page: &Page, r: u64) -> Result<(f64, f64, bool), String> {
    let js = format!(
        r#"(() => {{ const el = document.querySelector('[data-dcode-ref="{r}"]');
  if (!el) return null;
  el.scrollIntoView({{block: 'center', inline: 'center', behavior: 'instant'}});
  const b = el.getBoundingClientRect();
  return [b.left + b.width / 2, b.top + b.height / 2, el.type === 'password', b.width, b.height];
}})()"#
    );
    let found = page.eval(&js).await?;
    if matches!(&found, Value::Array(a) if a.len() >= 3) {
        still_before_action(page).await;
        crate::video::record::mark_action(true);
    }
    match found {
        Value::Array(a) if a.len() >= 3 => {
            if let (Some(w), Some(h)) = (
                a.get(3).and_then(Value::as_f64),
                a.get(4).and_then(Value::as_f64),
            ) && let Ok(mut b) = LAST_BOX.lock()
            {
                *b = Some((w, h));
            }
            Ok((
                a[0].as_f64().unwrap_or(0.0),
                a[1].as_f64().unwrap_or(0.0),
                a[2].as_bool().unwrap_or(false),
            ))
        }
        _ => Err(format!(
            "no element [{r}] on the page now — take a snapshot to get current numbers"
        )),
    }
}

async fn click_at(page: &Page, x: f64, y: f64) -> Result<(), String> {
    for (kind, buttons) in [("mouseMoved", 0), ("mousePressed", 1), ("mouseReleased", 0)] {
        page.call(
            "Input.dispatchMouseEvent",
            json!({"type": kind, "x": x, "y": y, "button": "left", "buttons": buttons, "clickCount": 1}),
        )
        .await?;
    }
    Ok(())
}

/// CDP key fields for a key name (`Enter`, `Tab`, `ArrowDown`, a letter…).
fn key_fields(key: &str) -> Value {
    let (code, vk, text) = match key {
        "Enter" => ("Enter", 13, Some("\r")),
        "Tab" => ("Tab", 9, None),
        "Escape" | "Esc" => ("Escape", 27, None),
        "Backspace" => ("Backspace", 8, None),
        "Delete" => ("Delete", 46, None),
        "ArrowUp" => ("ArrowUp", 38, None),
        "ArrowDown" => ("ArrowDown", 40, None),
        "ArrowLeft" => ("ArrowLeft", 37, None),
        "ArrowRight" => ("ArrowRight", 39, None),
        "PageUp" => ("PageUp", 33, None),
        "PageDown" => ("PageDown", 34, None),
        "Home" => ("Home", 36, None),
        "End" => ("End", 35, None),
        " " | "Space" => ("Space", 32, Some(" ")),
        _ => ("", 0, None),
    };
    let key = if key == "Esc" { "Escape" } else { key };
    let mut v = json!({"key": key, "code": code, "windowsVirtualKeyCode": vk});
    if let Some(t) = text {
        v["text"] = json!(t);
    } else if key.chars().count() == 1 {
        v["text"] = json!(key);
    }
    v
}

async fn press(page: &Page, key: &str) -> Result<(), String> {
    let fields = key_fields(key);
    let mut down = fields.clone();
    down["type"] = json!(if fields.get("text").is_some() {
        "keyDown"
    } else {
        "rawKeyDown"
    });
    page.call("Input.dispatchKeyEvent", down).await?;
    let mut up = fields;
    up["type"] = json!("keyUp");
    page.call("Input.dispatchKeyEvent", up).await?;
    Ok(())
}

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        format!("{}… [cut]", text.chars().take(max).collect::<String>())
    }
}

/// `example.com` → `https://example.com`, `localhost:3000` → `http://…`;
/// URLs that already have a scheme (`data:`, `file:`, `about:`) are kept.
fn with_scheme(url: &str) -> String {
    let local = ["localhost", "127.0.0.1", "0.0.0.0", "[::1]"];
    if local.iter().any(|l| url.starts_with(l)) {
        return format!("http://{url}");
    }
    let has_scheme = url.split_once(':').is_some_and(|(s, _)| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
            && !s.contains('.')
    });
    if has_scheme {
        url.to_string()
    } else {
        format!("https://{url}")
    }
}

/// Run one `browser` action; returns text for the model.
pub async fn run_action(input: &Value, workspace_root: &Path) -> Result<String, String> {
    match input["action"].as_str().unwrap_or("") {
        // Desktop apps: start one with debugging and connect, or list them.
        "open_app" => {
            let app = input["app"]
                .as_str()
                .filter(|a| !a.trim().is_empty())
                .ok_or("open_app needs `app` (vscode, slack, teams, outlook, postman, discord, notion, or a path to the .exe)")?;
            let new = input["new"].as_bool().unwrap_or(false);
            return crate::apps::open(app, new).await;
        }
        "apps" => {
            let l = crate::apps::list().await;
            return Ok(if l.is_empty() {
                "No apps connected. open_app starts one (vscode, slack, teams, outlook, postman, discord, notion).".into()
            } else {
                format!(
                    "Connected apps (use their name as `target`):\n{}",
                    l.join("\n")
                )
            });
        }
        _ => {}
    }
    // `target`: an app opened with `/app open` instead of the browser.
    if let Some(name) = input["target"]
        .as_str()
        .map(str::trim)
        .filter(|t| !t.is_empty() && !t.eq_ignore_ascii_case("browser"))
    {
        let mut apps = crate::apps::apps().lock().await;
        let Some(page) = apps.remove(name) else {
            let open: Vec<String> = apps.keys().cloned().collect();
            return Err(format!(
                "no app `{name}` connected{} — the user opens one with /app open <name>",
                if open.is_empty() {
                    String::new()
                } else {
                    format!(" (connected: {})", open.join(", "))
                }
            ));
        };
        let mut slot = Some(page);
        let result = run_on(&mut slot, false, input, workspace_root).await;
        if let Some(p) = slot {
            apps.insert(name.to_string(), p);
        }
        return result;
    }
    let mut slot = shared().lock().await;
    run_on(&mut slot, true, input, workspace_root).await
}

/// One action on the page in `slot` (the browser's, or an app's).
async fn run_on(
    slot: &mut Option<Page>,
    launch_if_needed: bool,
    input: &Value,
    workspace_root: &Path,
) -> Result<String, String> {
    let action = input["action"].as_str().unwrap_or("snapshot");
    if matches!(action, "switch" | "switch_tab" | "new_tab") {
        return tab_action(action, input, slot, launch_if_needed).await;
    }
    let (page, note) = ensure_page(slot, launch_if_needed).await?;
    if action == "tabs" {
        let tabs = list_tabs(page.port).await?;
        let mut out = String::from("Open tabs (switch with {\"action\":\"switch\",\"tab\":N}):\n");
        for (i, t) in tabs.iter().enumerate() {
            out.push_str(&format!(
                "[{}] {}{} — {}\n",
                i + 1,
                if t.id == page.target_id {
                    "(current) "
                } else {
                    ""
                },
                if t.title.is_empty() {
                    "untitled"
                } else {
                    &t.title
                },
                t.url
            ));
        }
        return Ok(out);
    }
    if action == "replay" {
        return replay(input, page, workspace_root).await;
    }
    if action == "steps" {
        let steps = recorded_steps();
        return Ok(if steps.is_empty() {
            "No browser steps recorded yet.".into()
        } else {
            serde_json::to_string_pretty(&steps).unwrap_or_default()
        });
    }
    // Clicks and typing are recorded by what the element is, not its number
    // (numbers change between page loads), so a replay finds it again.
    let target = if matches!(action, "click" | "type" | "fill") {
        match input["ref"].as_u64() {
            Some(r) => describe(page, r).await,
            None => None,
        }
    } else {
        None
    };
    if let Ok(mut l) = LAST_XY.lock() {
        *l = None;
    }
    if let Ok(mut l) = LAST_BOX.lock() {
        *l = None;
    }
    crate::video::record::mark_action(false);
    // While recording, a click that opens another tab must not lose the
    // recording: note the tabs before it.
    let recording = crate::video::record::is_recording();
    let tabs_before: Vec<String> = if recording && action == "click" {
        list_tabs(page.port)
            .await
            .map(|t| t.into_iter().map(|t| t.id).collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let mut result = run_page_action(action, input, page, workspace_root).await;
    if recording
        && action == "click"
        && result.is_ok()
        && let Some(note) = follow_new_tab(page, &tabs_before).await
    {
        result = result.map(|r| format!("{note}\n\n{r}"));
    }
    if result.is_ok() && crate::video::record::is_recording() {
        // The event track of a screen recording.
        let xy = LAST_XY.lock().ok().and_then(|l| *l);
        let size = LAST_BOX.lock().ok().and_then(|l| *l);
        let s = |k: &str| input[k].as_str().unwrap_or("").to_string();
        let (kind, label) = match action {
            "click" => ("click", target.clone().unwrap_or_default()),
            "type" | "fill" => ("type", target.clone().unwrap_or_default()),
            "navigate" | "open" | "goto" => ("navigate", s("url")),
            "press" | "key" => ("press", s("key")),
            "scroll" => ("scroll", s("direction")),
            "wait_for" | "wait" => ("wait", s("text")),
            "back" => ("navigate", "back".into()),
            _ => ("", String::new()),
        };
        if !kind.is_empty() {
            crate::video::record::note(kind, xy, size, &label);
            observe_for_recording(page).await;
        }
    }
    if result.is_ok() {
        record(action, input, target);
    }
    match (note, result) {
        (Some(n), Ok(text)) => Ok(format!("{n}\n\n{text}")),
        (_, r) => r,
    }
}

/// Browser actions done this session, for `steps` and `/browser save`.
fn recording() -> &'static std::sync::Mutex<Vec<Value>> {
    static STEPS: OnceLock<std::sync::Mutex<Vec<Value>>> = OnceLock::new();
    STEPS.get_or_init(Default::default)
}

/// The steps recorded so far.
pub fn recorded_steps() -> Vec<Value> {
    recording().lock().map(|s| s.clone()).unwrap_or_default()
}

/// Remember an action that changes the page (reads are not recorded).
fn record(action: &str, input: &Value, target: Option<String>) {
    let mut step = match action {
        "navigate" | "open" | "goto" => json!({"action": "navigate", "url": input["url"]}),
        "click" => json!({"action": "click"}),
        "type" | "fill" => {
            json!({"action": "type", "text": input["text"], "submit": input["submit"]})
        }
        "press" | "key" => json!({"action": "press", "key": input["key"]}),
        "scroll" => json!({"action": "scroll", "direction": input["direction"]}),
        "back" => json!({"action": "back"}),
        "wait_for" => json!({"action": "wait_for", "text": input["text"], "gone": input["gone"]}),
        _ => return,
    };
    if let Some(t) = target {
        step["target"] = json!(t);
    }
    if let Ok(mut s) = recording().lock() {
        s.push(step);
    }
}

/// `role "name"` of element `r`, as the snapshot lists it.
async fn describe(page: &Page, r: u64) -> Option<String> {
    let js = format!(
        r#"(() => {{ const el = document.querySelector('[data-dcode-ref="{r}"]');
  if (!el) return null;
  const tag = el.tagName.toLowerCase();
  const role = el.getAttribute('role') || (tag === 'a' ? 'link' : tag);
  const name = (el.getAttribute('aria-label') || el.innerText || el.placeholder || el.title || el.getAttribute('name') || el.alt || '')
    .replace(/\s+/g, ' ').trim().slice(0, 80);
  return name ? `${{role}} "${{name}}"` : role; }})()"#
    );
    page.eval(&js).await.ok()?.as_str().map(str::to_string)
}

/// The current number of the element described as `target` (`role "name"`).
async fn find_ref(page: &Page, target: &str) -> Result<u64, String> {
    let js = SNAPSHOT_JS
        .replace("MAX", &MAX_ELEMENTS.to_string())
        .replace("TEXT", "0");
    let raw = page.eval(&js).await?;
    let v: Value = serde_json::from_str(raw.as_str().unwrap_or("{}")).map_err(|e| e.to_string())?;
    v["elements"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .find_map(|line| {
            let (num, rest) = line.strip_prefix('[')?.split_once("] ")?;
            rest.starts_with(target).then(|| num.parse().ok()).flatten()
        })
        .ok_or_else(|| format!("could not find {target} on the page"))
}

/// Run saved steps (`/browser save`) again, one by one.
async fn replay(input: &Value, page: &Page, workspace_root: &Path) -> Result<String, String> {
    let file = input["file"].as_str().ok_or("replay needs `file`")?;
    let path = workspace_root.join(file);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let steps: Vec<Value> = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let mut log = Vec::new();
    for (i, step) in steps.iter().enumerate() {
        let mut step = step.clone();
        let action = step["action"].as_str().unwrap_or("").to_string();
        if let Some(target) = step["target"].as_str() {
            match find_ref(page, target).await {
                Ok(r) => step["ref"] = json!(r),
                Err(e) => return Err(format!("step {}: {e}\n{}", i + 1, log.join("\n"))),
            }
        }
        match run_page_action(&action, &step, page, workspace_root).await {
            Ok(_) => log.push(format!(
                "✓ {} {action}{}",
                i + 1,
                step["target"]
                    .as_str()
                    .map(|t| format!(" {t}"))
                    .unwrap_or_default()
            )),
            Err(e) => {
                return Err(format!(
                    "step {} ({action}) failed: {e}\n{}",
                    i + 1,
                    log.join("\n")
                ));
            }
        }
    }
    Ok(format!(
        "Replayed {} steps.\n{}\n\n{}",
        steps.len(),
        log.join("\n"),
        snapshot(page).await?
    ))
}

/// A screenshot of the page once it has settled (JPEG bytes).
async fn settled_shot(page: &Page) -> Result<Vec<u8>, String> {
    settle(page, Duration::from_secs(4)).await;
    let shot = page
        .call(
            "Page.captureScreenshot",
            json!({"format": "jpeg", "quality": 85}),
        )
        .await?;
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(shot["data"].as_str().ok_or("no screenshot")?)
        .map_err(|e| e.to_string())
}

/// How much of a picture changed (0–1), on small grey thumbnails.
pub fn picture_change(a: &[u8], b: &[u8]) -> Option<f64> {
    let t = |d: &[u8]| {
        image::load_from_memory(d)
            .ok()
            .map(|i| i.thumbnail_exact(96, 54).to_luma8())
    };
    let (a, b) = (t(a)?, t(b)?);
    let changed = a
        .pixels()
        .zip(b.pixels())
        .filter(|(p, q)| (p.0[0] as i32 - q.0[0] as i32).abs() > 32)
        .count();
    Some(changed as f64 / (96.0 * 54.0))
}

/// A share of the picture that may change before a step counts as changed
/// (clocks, counters, ads).
const SMOKE_TOLERANCE: f64 = 0.06;

/// `/browser test save <name>` and `/browser test run [name]`: the saved
/// browser steps as a smoke test. Saving replays them and keeps a
/// screenshot per step as the baseline; running replays them and compares.
pub async fn smoke_test(workspace: &Path, name: &str, save: bool) -> Result<String, String> {
    let safe: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect();
    if safe.is_empty() {
        return Err("give the test a name: /browser test save <name>".into());
    }
    let dir = workspace.join(".dcode-ai/browser-tests").join(&safe);
    let steps: Vec<Value> = if save {
        let from_file = workspace.join(format!(".dcode-ai/browser-steps/{safe}.json"));
        match std::fs::read_to_string(&from_file) {
            Ok(t) => serde_json::from_str(&t).map_err(|e| e.to_string())?,
            Err(_) => recorded_steps(),
        }
    } else {
        let t = std::fs::read_to_string(dir.join("steps.json"))
            .map_err(|_| format!("no test `{safe}` — /browser test save {safe} first"))?;
        serde_json::from_str(&t).map_err(|e| e.to_string())?
    };
    if steps.is_empty() {
        return Err(
            "no browser steps to test — do them with the browser (or /browser save <name>) first"
                .into(),
        );
    }
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    if save {
        std::fs::write(
            dir.join("steps.json"),
            serde_json::to_string_pretty(&steps).unwrap_or_default(),
        )
        .map_err(|e| e.to_string())?;
    }
    let run_dir = dir.join("last-run");
    if !save {
        let _ = std::fs::remove_dir_all(&run_dir);
        std::fs::create_dir_all(&run_dir).map_err(|e| e.to_string())?;
    }
    let mut slot = shared().lock().await;
    let (page, _) = ensure_page(&mut slot, true).await?;
    let mut report = Vec::new();
    let mut changed = 0;
    for (i, step) in steps.iter().enumerate() {
        let n = i + 1;
        let mut step = step.clone();
        let action = step["action"].as_str().unwrap_or("").to_string();
        let what = format!(
            "{action}{}",
            step["target"]
                .as_str()
                .or_else(|| step["url"].as_str())
                .map(|t| format!(" {t}"))
                .unwrap_or_default()
        );
        if let Some(target) = step["target"].as_str() {
            match find_ref(page, target).await {
                Ok(r) => step["ref"] = json!(r),
                Err(e) => {
                    report.push(format!("✗ {n}. {what}: {e}"));
                    return Ok(finish_report(&dir, save, &report, n, steps.len(), changed));
                }
            }
        }
        if let Err(e) = run_page_action(&action, &step, page, workspace).await {
            report.push(format!("✗ {n}. {what}: {e}"));
            return Ok(finish_report(&dir, save, &report, n, steps.len(), changed));
        }
        let shot = settled_shot(page).await?;
        if save {
            std::fs::write(dir.join(format!("base-{n}.jpg")), &shot).map_err(|e| e.to_string())?;
            report.push(format!("✓ {n}. {what}"));
            continue;
        }
        std::fs::write(run_dir.join(format!("step-{n}.jpg")), &shot).map_err(|e| e.to_string())?;
        match std::fs::read(dir.join(format!("base-{n}.jpg")))
            .ok()
            .and_then(|b| picture_change(&b, &shot))
        {
            Some(c) if c > SMOKE_TOLERANCE => {
                changed += 1;
                report.push(format!(
                    "≠ {n}. {what}: {:.0}% of the page differs from the baseline (last-run/step-{n}.jpg)",
                    c * 100.0
                ));
            }
            Some(_) => report.push(format!("✓ {n}. {what}")),
            None => report.push(format!("? {n}. {what}: no baseline picture")),
        }
    }
    Ok(finish_report(
        &dir,
        save,
        &report,
        steps.len() + 1,
        steps.len(),
        changed,
    ))
}

/// Writes the run's report next to the test and sums it up. `reached`: the
/// step it got to (past the end: all ran).
fn finish_report(
    dir: &Path,
    save: bool,
    lines: &[String],
    reached: usize,
    total: usize,
    changed: usize,
) -> String {
    let ok = reached > total;
    let head = if save {
        if ok {
            format!("baseline saved: {total} steps, a screenshot each")
        } else {
            format!("couldn't save the baseline: step {reached} failed")
        }
    } else if !ok {
        format!("FAILED at step {reached} of {total}")
    } else if changed > 0 {
        format!("ran {total} steps — {changed} look different from the baseline")
    } else {
        format!("passed: {total} steps, all match the baseline")
    };
    let body = format!("{head}\n{}", lines.join("\n"));
    let file = if save {
        "baseline.md"
    } else {
        "last-run/report.md"
    };
    let _ = std::fs::write(dir.join(file), &body);
    body
}

/// While recording: when a click opened a new tab, bring its page into the
/// recorded tab (and close the new one), so the recording follows it.
async fn follow_new_tab(page: &Page, before: &[String]) -> Option<String> {
    if before.is_empty() {
        return None;
    }
    // A new tab can take a moment to appear.
    let mut fresh = None;
    for _ in 0..6 {
        if let Ok(tabs) = list_tabs(page.port).await {
            fresh = tabs
                .into_iter()
                .find(|t| !before.contains(&t.id) && t.id != page.target_id);
        }
        if fresh.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let tab = fresh?;
    // Its address may still be about:blank while it starts loading.
    let mut url = tab.url.clone();
    for _ in 0..8 {
        if !is_blank(&url) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
        url = list_tabs(page.port)
            .await
            .ok()?
            .into_iter()
            .find(|t| t.id == tab.id)
            .map(|t| t.url)
            .unwrap_or(url);
    }
    if is_blank(&url) {
        return None;
    }
    let _ = http()
        .get(format!(
            "http://127.0.0.1:{}/json/close/{}",
            page.port, tab.id
        ))
        .send()
        .await;
    restore_site(page, &url).await;
    page.call("Page.navigate", json!({"url": url})).await.ok()?;
    page.wait_loaded().await;
    Some(format!(
        "(It opened a new tab; the recording follows it: {url} is now open in the recorded tab.)"
    ))
}

/// `switch {tab}` and `new_tab {url}`.
async fn tab_action(
    action: &str,
    input: &Value,
    slot: &mut Option<Page>,
    launch_if_needed: bool,
) -> Result<String, String> {
    let (page, _) = ensure_page(slot, launch_if_needed).await?;
    let port = page.port;
    let next = if action == "new_tab" {
        let url = with_scheme(input["url"].as_str().unwrap_or("about:blank").trim());
        open_tab(port, &url).await?
    } else {
        let n = input["tab"]
            .as_u64()
            .or_else(|| input["ref"].as_u64())
            .ok_or("switch needs `tab`, a number from the `tabs` list")? as usize;
        let tabs = list_tabs(port).await?;
        let tab = tabs
            .get(n.saturating_sub(1))
            .ok_or_else(|| format!("no tab {n}; there are {}", tabs.len()))?;
        attach_tab(port, tab).await?
    };
    *slot = Some(next);
    let page = slot.as_ref().expect("set above");
    page.wait_loaded().await;
    snapshot(page).await
}

async fn run_page_action(
    action: &str,
    input: &Value,
    page: &Page,
    workspace_root: &Path,
) -> Result<String, String> {
    let r = || {
        input["ref"]
            .as_u64()
            .or_else(|| {
                input["ref"]
                    .as_str()
                    .and_then(|s| s.trim_matches(['[', ']']).parse().ok())
            })
            .ok_or_else(|| {
                "this action needs `ref`, an element number from the snapshot".to_string()
            })
    };
    match action {
        "navigate" | "open" | "goto" => {
            let mut url = input["url"].as_str().unwrap_or("").trim().to_string();
            if url.is_empty() {
                return Err("navigate needs `url`".into());
            }
            url = with_scheme(&url);
            restore_site(page, &url).await;
            let r = page.call("Page.navigate", json!({"url": url})).await?;
            if let Some(err) = r["errorText"].as_str().filter(|e| !e.is_empty()) {
                return Err(format!("could not open {url}: {err}"));
            }
            page.wait_loaded().await;
            snapshot(page).await
        }
        "snapshot" | "read" => snapshot(page).await,
        "find" | "search" => {
            let query = input["query"]
                .as_str()
                .or_else(|| input["text"].as_str())
                .filter(|q| !q.trim().is_empty())
                .ok_or("find needs `query`, the text to look for")?;
            find(page, query).await
        }
        "click" => {
            let (x, y, _) = locate(page, r()?).await?;
            if let Ok(mut l) = LAST_XY.lock() {
                *l = Some((x, y));
            }
            click_at(page, x, y).await?;
            page.wait_loaded().await;
            Ok(format!("Clicked.\n\n{}", changes(page).await?))
        }
        "type" | "fill" => {
            let n = r()?;
            let text = input["text"].as_str().unwrap_or("");
            let (x, y, password) = locate(page, n).await?;
            if let Ok(mut l) = LAST_XY.lock() {
                *l = Some((x, y));
            }
            if password {
                return Err(
                    "that is a password field — dcode-ai does not type passwords; ask the user to sign in in the browser window".into(),
                );
            }
            click_at(page, x, y).await?;
            // Replace what is there.
            page.eval(&format!(
                r#"(() => {{ const el = document.querySelector('[data-dcode-ref="{n}"]');
  if (el && 'select' in el && typeof el.select === 'function') el.select();
  else if (el && el.isContentEditable) document.execCommand('selectAll'); }})()"#
            ))
            .await?;
            page.call("Input.insertText", json!({"text": text})).await?;
            if input["submit"].as_bool() == Some(true) {
                press(page, "Enter").await?;
                page.wait_loaded().await;
                return Ok(format!(
                    "Typed and pressed Enter.\n\n{}",
                    changes(page).await?
                ));
            }
            Ok("Typed.".into())
        }
        "press" | "key" => {
            let key = input["key"].as_str().unwrap_or("Enter");
            press(page, key).await?;
            page.wait_loaded().await;
            Ok(format!("Pressed {key}.\n\n{}", changes(page).await?))
        }
        "scroll" => {
            let dir = if input["direction"].as_str() == Some("up") {
                -1
            } else {
                1
            };
            page.eval(&format!(
                "window.scrollBy(0, {dir} * window.innerHeight * 0.8)"
            ))
            .await?;
            tokio::time::sleep(Duration::from_millis(300)).await;
            changes(page).await
        }
        "back" => {
            page.eval("history.back()").await?;
            page.wait_loaded().await;
            snapshot(page).await
        }
        "screenshot" => {
            let shot = page
                .call("Page.captureScreenshot", json!({"format": "png"}))
                .await?;
            let data = shot["data"].as_str().ok_or("no screenshot data")?;
            use base64::Engine;
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(data)
                .map_err(|e| e.to_string())?;
            let dir = workspace_root.join(".dcode-ai").join("screenshots");
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let name = format!(
                "shot-{}.png",
                chrono::Utc::now().format("%Y%m%d-%H%M%S%.3f")
            );
            std::fs::write(dir.join(&name), bytes).map_err(|e| e.to_string())?;
            Ok(format!(
                "Saved a screenshot to .dcode-ai/screenshots/{name}"
            ))
        }
        "wait_for" | "wait" => {
            let text = input["text"].as_str().ok_or("wait_for needs `text`")?;
            let gone = input["gone"].as_bool().unwrap_or(false);
            let timeout = input["timeout_ms"].as_u64().unwrap_or(10_000).min(60_000);
            let probe = format!(
                "(document.title + '\\n' + (document.body ? document.body.innerText : '')).includes({})",
                serde_json::to_string(text).unwrap_or_default()
            );
            let start = std::time::Instant::now();
            loop {
                let there = page.eval(&probe).await?.as_bool().unwrap_or(false);
                if there != gone {
                    break;
                }
                if start.elapsed() >= Duration::from_millis(timeout) {
                    return Err(format!(
                        "after {}s the page {} {text:?}",
                        timeout / 1000,
                        if gone { "still shows" } else { "does not show" }
                    ));
                }
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            changes(page).await
        }
        "console" => {
            let lines = page.console_lines();
            if lines.is_empty() {
                Ok("The console is empty.".into())
            } else {
                Ok(clip(&lines.join("\n"), 8_000))
            }
        }
        "eval" | "evaluate" => {
            let expr = input["expression"]
                .as_str()
                .or_else(|| input["script"].as_str())
                .ok_or("eval needs `expression`")?;
            let v = page.eval(expr).await?;
            Ok(clip(
                &match v {
                    Value::String(s) => s,
                    other => serde_json::to_string_pretty(&other).unwrap_or_default(),
                },
                8_000,
            ))
        }
        other => Err(format!(
            "unknown action `{other}` (navigate, snapshot, click, type, press, scroll, back, screenshot, console, eval, tabs, switch, new_tab, wait_for, steps, replay)"
        )),
    }
}

/// Actions that only read the page.
pub fn is_read_only_action(action: &str) -> bool {
    matches!(
        action,
        "snapshot" | "read" | "find" | "search" | "screenshot" | "console" | "tabs"
    )
}

/// The `browser` tool.
pub struct BrowserTool {
    workspace_root: PathBuf,
}

impl BrowserTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for BrowserTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "browser".into(),
            description: "Control a real web browser (Chrome/Edge, opened in its own window). \
                Use it to open and test web pages and apps, fill forms and click through flows. \
                Actions: navigate {url}; snapshot (the page as text with numbered elements); \
                find {query} (only the elements and text that mention it — cheaper than a \
                snapshot when you know what you are looking for); \
                click {ref}; type {ref, text, submit?}; press {key} (Enter, Tab, Escape, \
                ArrowDown…); scroll {direction: down|up}; back; screenshot (saves a PNG); \
                console (log and errors); eval {expression} (run JavaScript); tabs (list open \
                tabs); switch {tab}; new_tab {url}; wait_for {text, gone?, timeout_ms?} (wait \
                until text appears or disappears); steps (actions so far); replay {file} (run \
                saved steps). navigate, back, switch and new_tab return the whole page; \
                click, press, scroll, wait_for and type with submit return only what changed \
                on the same page (call snapshot for everything). An element keeps its number \
                until the page is left. If the user opens a tab, the tool follows it. Never \
                type passwords: ask the user to sign in in the browser window. Desktop apps built on web \
tech (VS Code, Slack, Teams, Outlook, Postman, Discord, Notion) work the same way: open_app \
{app, new?} starts and connects one, apps lists them, and `target: \"<app>\"` on any other \
action drives it instead of the browser."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["navigate", "snapshot", "find", "click", "type", "press", "scroll", "back", "screenshot", "console", "eval", "tabs", "switch", "new_tab", "wait_for", "steps", "replay", "open_app", "apps"]
                    },
                    "tab": {"type": "integer", "description": "Tab number from `tabs` (for switch)"},
                    "target": {"type": "string", "description": "A connected desktop app (e.g. \"vscode\", \"slack\", \"teams\") instead of the browser"},
                    "app": {"type": "string", "description": "open_app: vscode, slack, teams, outlook, postman, discord, notion, or a path to an Electron/WebView2 .exe"},
                    "new": {"type": "boolean", "description": "open_app: a separate copy beside one that's already open (Electron apps)"},
                    "query": {"type": "string", "description": "find: text to look for (case-insensitive)"},
                    "gone": {"type": "boolean", "description": "wait_for: wait until the text is gone"},
                    "timeout_ms": {"type": "integer"},
                    "file": {"type": "string", "description": "replay: steps file, e.g. .dcode-ai/browser-steps/login.json"},
                    "url": {"type": "string"},
                    "ref": {"type": "integer", "description": "Element number from the snapshot"},
                    "text": {"type": "string"},
                    "submit": {"type": "boolean", "description": "Press Enter after typing"},
                    "key": {"type": "string"},
                    "direction": {"type": "string", "enum": ["down", "up"]},
                    "expression": {"type": "string", "description": "JavaScript to evaluate in the page"}
                },
                "required": ["action"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        match run_action(&call.input, &self.workspace_root).await {
            Ok(output) => ToolResult {
                call_id: call.id.clone(),
                success: true,
                output,
                error: None,
            },
            Err(e) => ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(e),
            },
        }
    }
}

/// `/browser` status line.
pub async fn status() -> String {
    let slot = shared().lock().await;
    match slot.as_ref() {
        Some(p) if p.is_alive() => {
            let url = p
                .eval("location.href")
                .await
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            format!("connected on port {} · {url}", p.port)
        }
        _ => match find_browser() {
            Some(b) => format!(
                "not connected · will launch {} on first use (port {DEFAULT_PORT})",
                b.display()
            ),
            None => "not connected · no Chrome/Edge found (set DCODE_AI_BROWSER)".into(),
        },
    }
}

/// `/browser connect <port>`: attach to a browser the user started.
pub async fn connect(port: u16) -> Result<String, String> {
    let page = attach(port).await?;
    let url = page
        .eval("location.href")
        .await
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    *shared().lock().await = Some(page);
    Ok(format!("connected to the browser on port {port} · {url}"))
}

/// `/browser open [url]`: launch dcode-ai's browser (if needed) and connect.
pub async fn open(url: Option<&str>) -> Result<String, String> {
    let launched = if port_alive(DEFAULT_PORT).await {
        None
    } else {
        Some(launch(DEFAULT_PORT, url).await?)
    };
    let page = attach(DEFAULT_PORT).await?;
    if let Some(u) = url {
        restore_site(&page, &with_scheme(u)).await;
        page.call("Page.navigate", json!({"url": with_scheme(u)}))
            .await?;
    }
    *shared().lock().await = Some(page);
    Ok(match launched {
        Some(exe) => format!("opened {exe} (port {DEFAULT_PORT})"),
        None => format!("connected to the browser on port {DEFAULT_PORT}"),
    })
}

/// Sign-in services a site may send you through; their cookies are kept
/// with the site's, or the sign-in would be asked for again.
const SIGN_IN_DOMAINS: &[&str] = &[
    "microsoftonline.com",
    "login.live.com",
    "microsoft.com",
    "accounts.google.com",
    "google.com",
    "okta.com",
    "auth0.com",
    "onelogin.com",
    "github.com",
];

fn sites_dir() -> Option<PathBuf> {
    dcode_ai_common::config::dcode_ai_home_dir().map(|d| d.join("sites"))
}

/// A URL's host, lower-case (`https://acme.sharepoint.com/x` → `acme.sharepoint.com`).
pub fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host = rest.split(['/', '?', '#']).next()?.rsplit('@').next()?;
    let host = host.split(':').next()?.trim().to_ascii_lowercase();
    (!host.is_empty() && host.contains('.')).then_some(host)
}

/// The part of a host cookies are shared on: `acme.sharepoint.com` →
/// `sharepoint.com` is too wide, so the full host and its parent are used.
fn cookie_matches(domain: &str, host: &str) -> bool {
    let d = domain.trim_start_matches('.').to_ascii_lowercase();
    host == d
        || host.ends_with(&format!(".{d}"))
        || d.ends_with(&format!(".{host}"))
        || SIGN_IN_DOMAINS
            .iter()
            .any(|s| d == *s || d.ends_with(&format!(".{s}")))
}

fn site_file(host: &str) -> Option<PathBuf> {
    let safe: String = host
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
        .collect();
    (!safe.is_empty()).then(|| sites_dir().map(|d| d.join(format!("{safe}.json"))))?
}

/// `/browser remember [site]`: keeps the site's sign-in (its cookies and
/// those of the sign-in service, session cookies too) in
/// `~/.dcode-ai/sites/<host>.json`, readable only by you; they are put back
/// whenever the browser opens that site.
pub async fn remember_site(site: Option<&str>) -> Result<String, String> {
    let mut slot = shared().lock().await;
    let (page, _) = ensure_page(&mut slot, false).await?;
    let host = match site {
        Some(s) => host_of(s).or_else(|| host_of(&format!("https://{s}"))),
        None => page
            .eval("location.href")
            .await
            .ok()
            .and_then(|v| v.as_str().and_then(host_of)),
    }
    .ok_or("which site? open it in the browser first, or /browser remember <site>")?;
    let all = page.call("Network.getAllCookies", json!({})).await?;
    let cookies: Vec<Value> = all["cookies"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|c| {
                    c["domain"]
                        .as_str()
                        .is_some_and(|d| cookie_matches(d, &host))
                })
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    if cookies.is_empty() {
        return Err(format!("no cookies for {host} — sign in there first"));
    }
    let file = site_file(&host).ok_or("no home folder")?;
    if let Some(d) = file.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string(&json!({"host": host, "cookies": cookies}))
        .map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&file)
            .map_err(|e| e.to_string())?;
        f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    }
    #[cfg(not(unix))]
    std::fs::write(&file, text).map_err(|e| e.to_string())?;
    Ok(format!(
        "remembered the sign-in for {host} ({} cookies) — recordings there won't ask again; /browser forget {host} removes it",
        cookies.len()
    ))
}

/// `/browser forget <site>`.
pub fn forget_site(site: &str) -> Result<String, String> {
    let host = host_of(site)
        .or_else(|| host_of(&format!("https://{site}")))
        .ok_or("which site?")?;
    let file = site_file(&host).ok_or("no home folder")?;
    std::fs::remove_file(&file).map_err(|_| format!("nothing remembered for {host}"))?;
    Ok(format!("forgot the sign-in for {host}"))
}

/// Sites with a remembered sign-in.
pub fn remembered_sites() -> Vec<String> {
    let mut v: Vec<String> = sites_dir()
        .and_then(|d| std::fs::read_dir(d).ok())
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_suffix(".json")
                .map(str::to_string)
        })
        .collect();
    v.sort();
    v
}

/// Puts a remembered sign-in back before opening `url`.
async fn restore_site(page: &Page, url: &str) {
    let Some(host) = host_of(url) else { return };
    // The host or a parent domain may have been remembered.
    let mut h = host.as_str();
    let file = loop {
        if let Some(f) = site_file(h).filter(|f| f.is_file()) {
            break f;
        }
        match h.split_once('.') {
            Some((_, rest)) if rest.contains('.') => h = rest,
            _ => return,
        }
    };
    let Some(saved) = std::fs::read_to_string(&file)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
    else {
        return;
    };
    let cookies: Vec<Value> = saved["cookies"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|c| {
                    let mut c = c.clone();
                    if let Some(o) = c.as_object_mut() {
                        // setCookies takes these fields; a session cookie
                        // has no expiry.
                        o.retain(|k, _| {
                            matches!(
                                k.as_str(),
                                "name"
                                    | "value"
                                    | "domain"
                                    | "path"
                                    | "secure"
                                    | "httpOnly"
                                    | "sameSite"
                                    | "expires"
                            )
                        });
                        if o.get("expires")
                            .and_then(Value::as_f64)
                            .is_some_and(|e| e <= 0.0)
                        {
                            o.remove("expires");
                        }
                    }
                    c
                })
                .collect()
        })
        .unwrap_or_default();
    if !cookies.is_empty() {
        let _ = page
            .call("Network.setCookies", json!({"cookies": cookies}))
            .await;
    }
}

/// `/browser close`: disconnect, and close the browser when it is the one
/// dcode-ai launched (a browser the user attached stays open).
pub async fn close() -> Result<String, String> {
    let port = shared().lock().await.take().map(|p| p.port);
    if port != Some(DEFAULT_PORT) && !(port.is_none() && port_alive(DEFAULT_PORT).await) {
        return Ok(match port {
            Some(p) => format!("disconnected from the browser on port {p} (left open)"),
            None => "no browser connected".into(),
        });
    }
    let version: Value = http()
        .get(format!("http://127.0.0.1:{DEFAULT_PORT}/json/version"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let ws = version["webSocketDebuggerUrl"]
        .as_str()
        .ok_or("the browser gave no control address")?;
    let (mut socket, _) = tokio_tungstenite::connect_async(ws)
        .await
        .map_err(|e| e.to_string())?;
    let _ = socket
        .send(WsMessage::Text(
            json!({"id": 1, "method": "Browser.close"})
                .to_string()
                .into(),
        ))
        .await;
    let _ = tokio::time::timeout(Duration::from_secs(3), socket.next()).await;
    Ok("closed dcode-ai's browser".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screencast_ack_uses_a_32_bit_id() {
        // Chrome drops messages whose id doesn't fit in an i32; the
        // screencast then stalls after two frames.
        let v: Value = serde_json::from_str(&screencast_ack_message(42, 7)).unwrap();
        assert_eq!(v["id"], 42);
        assert!(v["id"].as_u64().unwrap() <= i32::MAX as u64);
        assert_eq!(v["method"], "Page.screencastFrameAck");
        assert_eq!(v["params"]["sessionId"], 7);
    }

    #[test]
    fn find_lists_only_matching_elements_and_text() {
        let v = json!({"title": "T", "url": "u",
            "elements": ["[4] button \"Save draft\"", "[7] link \"Home\"", "[9] button \"Save\""],
            "text": "Welcome\nRelease 1.2 saved\nOther"});
        let out = format_find(&v, "save");
        assert!(out.contains("[4] button") && out.contains("[9] button"));
        assert!(!out.contains("Home") && !out.contains("Welcome"));
        assert!(out.contains("Release 1.2 saved"));
        assert!(format_find(&v, "zzz").contains("Nothing on the page"));
    }

    #[test]
    fn changes_list_only_what_is_new_on_the_same_page() {
        let old = json!({"title": "T", "url": "u", "elements": ["[1] button \"Save\"", "[2] link \"Home\""],
            "text": "Header\nRow one"});
        let new = json!({"title": "T", "url": "u", "elements": ["[1] button \"Save\"", "[3] button \"Undo\""],
            "text": "Header\nRow one\nSaved!"});
        let out = format_changes(&old, &new);
        assert!(out.contains("[3] button \"Undo\""));
        assert!(
            !out.contains("[1] button"),
            "unchanged elements are left out: {out}"
        );
        assert!(out.contains("1 element(s) gone"));
        assert!(out.contains("Saved!") && !out.contains("Row one"));
        assert!(format_changes(&new, &new).contains("No visible change"));
    }

    #[test]
    fn snapshot_text_lists_elements_and_page_text() {
        let v = json!({
            "title": "Sign in",
            "url": "https://example.com/login",
            "elements": ["[1] input \"Email\" type=email", "[2] button \"Continue\""],
            "more": false,
            "text": "Welcome back\nEmail",
            "cut": false
        });
        let s = format_snapshot(&v);
        assert!(
            s.starts_with("Page: Sign in\nURL: https://example.com/login\n"),
            "{s}"
        );
        assert!(s.contains("[2] button \"Continue\""));
        assert!(s.contains("Page text:\nWelcome back"));
    }

    #[test]
    fn console_events_become_lines() {
        let log = json!({"method": "Runtime.consoleAPICalled", "params": {"type": "warn", "args": [{"type": "string", "value": "slow"}, {"type": "number", "value": 3}]}});
        assert_eq!(console_line(&log).as_deref(), Some("[warn] slow 3"));
        let err = json!({"method": "Runtime.exceptionThrown", "params": {"exceptionDetails": {"exception": {"description": "TypeError: x is undefined\n    at a.js:1"}}}});
        assert_eq!(
            console_line(&err).as_deref(),
            Some("[error] TypeError: x is undefined")
        );
        assert_eq!(
            console_line(&json!({"method": "Page.loadEventFired"})),
            None
        );
    }

    #[test]
    fn urls_get_a_scheme_only_when_missing() {
        assert_eq!(with_scheme("example.com"), "https://example.com");
        assert_eq!(
            with_scheme("example.com:8080/x"),
            "https://example.com:8080/x"
        );
        assert_eq!(with_scheme("localhost:3000"), "http://localhost:3000");
        assert_eq!(with_scheme("http://a.b"), "http://a.b");
        assert_eq!(with_scheme("data:text/html,hi"), "data:text/html,hi");
        assert_eq!(with_scheme("file:///tmp/x.html"), "file:///tmp/x.html");
    }

    #[test]
    fn tab_list_skips_browser_pages_but_keeps_new_tabs() {
        let list = json!([
            {"id": "A", "type": "page", "title": "YouTube", "url": "https://www.youtube.com/", "webSocketDebuggerUrl": "ws://x/devtools/page/A"},
            {"id": "B", "type": "page", "title": "", "url": "chrome://omnibox-popup.top-chrome/", "webSocketDebuggerUrl": "ws://x/devtools/page/B"},
            {"id": "C", "type": "page", "title": "New Tab", "url": "chrome://newtab/", "webSocketDebuggerUrl": "ws://x/devtools/page/C"},
            {"id": "D", "type": "service_worker", "url": "https://a/sw.js", "webSocketDebuggerUrl": "ws://x/devtools/page/D"}
        ]);
        let tabs = tabs_from_list(&list);
        let ids: Vec<&str> = tabs.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["A", "C"]);
        assert!(is_blank("about:blank") && is_blank("chrome://newtab/"));
        assert!(!is_blank("https://www.youtube.com/"));
        assert!(is_read_only_action("tabs") && !is_read_only_action("switch"));
    }

    #[test]
    fn keys_map_to_cdp_fields() {
        assert_eq!(key_fields("Enter")["windowsVirtualKeyCode"], 13);
        assert_eq!(key_fields("Enter")["text"], "\r");
        assert_eq!(key_fields("a")["text"], "a");
        assert_eq!(key_fields("Esc")["key"], "Escape");
        assert!(is_read_only_action("snapshot") && !is_read_only_action("click"));
    }
}

#[cfg(test)]
mod site_tests {
    use super::*;

    #[test]
    fn hosts_and_cookie_domains() {
        assert_eq!(
            host_of("https://acme.sharepoint.com/sites/x?y=1").as_deref(),
            Some("acme.sharepoint.com")
        );
        assert_eq!(
            host_of("user@Example.com:8080/a").as_deref(),
            Some("example.com")
        );
        assert_eq!(host_of("localhost"), None);
        let h = "acme.sharepoint.com";
        assert!(cookie_matches(".acme.sharepoint.com", h));
        assert!(cookie_matches("login.microsoftonline.com", h));
        assert!(!cookie_matches("other.sharepoint.com", h));
        assert!(!cookie_matches("ads.example.net", h));
        assert_eq!(
            site_file("a/../b.com").map(|f| f.file_name().unwrap().to_string_lossy().into_owned()),
            Some("a..b.com.json".into())
        );
    }
}
