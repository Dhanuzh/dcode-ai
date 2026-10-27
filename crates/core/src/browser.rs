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
    next_id: AtomicU64,
    console: Arc<std::sync::Mutex<VecDeque<String>>>,
    /// Debug port, for status and new tabs.
    pub port: u16,
    alive: Arc<std::sync::atomic::AtomicBool>,
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
            next_id: AtomicU64::new(1),
            console,
            port,
            alive,
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
async fn port_alive(port: u16) -> bool {
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

/// Attach to a page of the browser on `port` (opening one if none).
pub async fn attach(port: u16) -> Result<Page, String> {
    let list: Value = http()
        .get(format!("http://127.0.0.1:{port}/json/list"))
        .send()
        .await
        .map_err(|e| format!("no browser on port {port}: {e}"))?
        .json()
        .await
        .map_err(|e| e.to_string())?;
    let page_ws = list.as_array().and_then(|pages| {
        pages
            .iter()
            .filter(|p| p["type"] == "page")
            // Skip the browser's own UI pages (omnibox popup, devtools…).
            .find(|p| {
                let url = p["url"].as_str().unwrap_or("");
                ![
                    "devtools://",
                    "chrome://",
                    "chrome-untrusted://",
                    "edge://",
                    "brave://",
                ]
                .iter()
                .any(|s| url.starts_with(s))
            })
            .and_then(|p| p["webSocketDebuggerUrl"].as_str().map(str::to_string))
    });
    let ws = match page_ws {
        Some(ws) => ws,
        None => {
            let new: Value = http()
                .put(format!("http://127.0.0.1:{port}/json/new?about:blank"))
                .send()
                .await
                .map_err(|e| e.to_string())?
                .json()
                .await
                .map_err(|e| e.to_string())?;
            new["webSocketDebuggerUrl"]
                .as_str()
                .ok_or("the browser did not open a tab")?
                .to_string()
        }
    };
    Page::connect(&ws, port).await
}

/// The connected page, launching dcode-ai's browser first if needed.
async fn ensure_page(slot: &mut Option<Page>) -> Result<&Page, String> {
    if slot.as_ref().is_some_and(|p| !p.is_alive()) {
        *slot = None;
    }
    if slot.is_none() {
        if !port_alive(DEFAULT_PORT).await {
            launch(DEFAULT_PORT, None).await?;
        }
        *slot = Some(attach(DEFAULT_PORT).await?);
    }
    Ok(slot.as_ref().expect("set above"))
}

/// Script that numbers the visible interactive elements (`data-dcode-ref`)
/// and returns the page as JSON: title, URL, elements, text.
const SNAPSHOT_JS: &str = r#"(() => {
  const vis = el => { const r = el.getBoundingClientRect(); const s = getComputedStyle(el);
    return r.width > 0 && r.height > 0 && s.visibility !== 'hidden' && s.display !== 'none'; };
  document.querySelectorAll('[data-dcode-ref]').forEach(e => e.removeAttribute('data-dcode-ref'));
  const sel = 'a[href],button,input:not([type=hidden]),select,textarea,summary,[role=button],[role=link],[role=checkbox],[role=radio],[role=tab],[role=menuitem],[role=option],[role=switch],[contenteditable=""],[contenteditable=true],[onclick]';
  const out = []; let n = 0; let more = false;
  for (const el of document.querySelectorAll(sel)) {
    if (!vis(el) || el.disabled) continue;
    if (n >= MAX) { more = true; break; }
    n++; el.setAttribute('data-dcode-ref', String(n));
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
    out.push(`[${n}] ${role}${name ? ` "${name}"` : ''}${extra}`);
  }
  const text = (document.body ? document.body.innerText : '').replace(/\n{3,}/g, '\n\n');
  return JSON.stringify({ title: document.title, url: location.href, elements: out, more, text: text.slice(0, TEXT), cut: text.length > TEXT });
})()"#;

/// The page as text for the model.
async fn snapshot(page: &Page) -> Result<String, String> {
    let js = SNAPSHOT_JS
        .replace("MAX", &MAX_ELEMENTS.to_string())
        .replace("TEXT", &MAX_TEXT.to_string());
    let raw = page.eval(&js).await?;
    let v: Value = serde_json::from_str(raw.as_str().unwrap_or("{}")).map_err(|e| e.to_string())?;
    Ok(format_snapshot(&v))
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
/// password field.
async fn locate(page: &Page, r: u64) -> Result<(f64, f64, bool), String> {
    let js = format!(
        r#"(() => {{ const el = document.querySelector('[data-dcode-ref="{r}"]');
  if (!el) return null;
  el.scrollIntoView({{block: 'center', inline: 'center'}});
  const b = el.getBoundingClientRect();
  return [b.left + b.width / 2, b.top + b.height / 2, el.type === 'password'];
}})()"#
    );
    match page.eval(&js).await? {
        Value::Array(a) if a.len() == 3 => Ok((
            a[0].as_f64().unwrap_or(0.0),
            a[1].as_f64().unwrap_or(0.0),
            a[2].as_bool().unwrap_or(false),
        )),
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
    let action = input["action"].as_str().unwrap_or("snapshot");
    let mut slot = shared().lock().await;
    let page = ensure_page(&mut slot).await?;
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
            let r = page.call("Page.navigate", json!({"url": url})).await?;
            if let Some(err) = r["errorText"].as_str().filter(|e| !e.is_empty()) {
                return Err(format!("could not open {url}: {err}"));
            }
            page.wait_loaded().await;
            snapshot(page).await
        }
        "snapshot" | "read" => snapshot(page).await,
        "click" => {
            let (x, y, _) = locate(page, r()?).await?;
            click_at(page, x, y).await?;
            page.wait_loaded().await;
            Ok(format!("Clicked.\n\n{}", snapshot(page).await?))
        }
        "type" | "fill" => {
            let n = r()?;
            let text = input["text"].as_str().unwrap_or("");
            let (x, y, password) = locate(page, n).await?;
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
                    snapshot(page).await?
                ));
            }
            Ok("Typed.".into())
        }
        "press" | "key" => {
            let key = input["key"].as_str().unwrap_or("Enter");
            press(page, key).await?;
            page.wait_loaded().await;
            Ok(format!("Pressed {key}.\n\n{}", snapshot(page).await?))
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
            snapshot(page).await
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
            "unknown action `{other}` (navigate, snapshot, click, type, press, scroll, back, screenshot, console, eval)"
        )),
    }
}

/// Actions that only read the page.
pub fn is_read_only_action(action: &str) -> bool {
    matches!(action, "snapshot" | "read" | "screenshot" | "console")
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
                click {ref}; type {ref, text, submit?}; press {key} (Enter, Tab, Escape, \
                ArrowDown…); scroll {direction: down|up}; back; screenshot (saves a PNG); \
                console (log and errors); eval {expression} (run JavaScript). navigate, click, \
                press, scroll and back return the new snapshot. Element numbers change when the \
                page changes — use the latest snapshot. Never type passwords: ask the user to \
                sign in in the browser window."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["navigate", "snapshot", "click", "type", "press", "scroll", "back", "screenshot", "console", "eval"]
                    },
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
    if launched.is_none()
        && let Some(u) = url
    {
        page.call("Page.navigate", json!({"url": u})).await?;
    }
    *shared().lock().await = Some(page);
    Ok(match launched {
        Some(exe) => format!("opened {exe} (port {DEFAULT_PORT})"),
        None => format!("connected to the browser on port {DEFAULT_PORT}"),
    })
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
    fn keys_map_to_cdp_fields() {
        assert_eq!(key_fields("Enter")["windowsVirtualKeyCode"], 13);
        assert_eq!(key_fields("Enter")["text"], "\r");
        assert_eq!(key_fields("a")["text"], "a");
        assert_eq!(key_fields("Esc")["key"], "Escape");
        assert!(is_read_only_action("snapshot") && !is_read_only_action("click"));
    }
}
