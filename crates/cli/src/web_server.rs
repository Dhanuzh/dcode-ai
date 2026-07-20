//! `dcode-ai web` — local web chat for the agent runtime.
//!
//! Serves a single embedded HTML page over loopback HTTP and bridges the
//! browser to a normal service session through the existing IPC layer
//! (Unix socket on Unix, loopback TCP on Windows):
//!
//! - `GET  /`                 → embedded chat page
//! - `GET  /events`           → Server-Sent Events; each `data:` line is one
//!   [`EventEnvelope`] exactly as broadcast on the session IPC socket
//! - `GET  /api/info`         → current session + provider/model/login snapshot
//! - `GET  /api/sessions`     → list all saved session snapshots
//! - `POST /api/command`      → body is one [`AgentCommand`] JSON object
//! - `POST /api/sessions/new` → start a fresh session (stop current)
//! - `POST /api/sessions/switch` → resume a different session by id
//! - `POST /api/sessions/rename` → set a session's display name
//! - `POST /api/sessions/delete` → delete a saved session (not the active one)
//! - `POST /api/sessions/fork`   → clone a session to a new id and switch to it
//! - `POST /api/model`        → switch provider/model, conversation preserved
//! - `POST /api/key`          → store a provider API key (credentials store)
//! - `POST /api/rewind`       → drop back to a past user message, re-run a turn
//!   (powers regenerate and edit-and-resend)
//! - `POST /api/settings`     → set permission mode / extended thinking
//! - `POST /api/upload?name=` → raw file body → session attachments dir
//! - `GET  /api/search?q=`    → full-text search across session event logs
//! - `GET  /api/file?path=`   → serve a session attachment (image previews)
//! - `GET  /api/files`        → flat workspace file list (`@` completion)
//! - `GET  /api/tree?dir=`    → list a workspace directory (file explorer)
//! - `GET  /api/workspace-file?path=` → read a workspace text file (viewer)
//! - `GET  /api/git-diff?path=` → `git diff HEAD` for a workspace file
//!
//! No web framework: requests are parsed with a minimal HTTP/1.1 reader on
//! raw tokio TCP so the feature adds zero dependencies. The server binds
//! 127.0.0.1 only and every route requires the per-run `?t=<token>` secret
//! printed at startup, so other local users/processes can't drive the agent.

use std::path::PathBuf;
use std::sync::Arc;

use dcode_ai_common::config::{DcodeAiConfig, ProviderKind};
use dcode_ai_common::event::AgentCommand;
use dcode_ai_common::session::OrchestrationContext;
use dcode_ai_runtime::ipc::IpcClient;
use dcode_ai_runtime::service::{
    ServiceSessionHandle, ServiceSessionKind, ServiceSessionRequest, spawn_service_session,
};
use dcode_ai_runtime::session_store::SessionStore;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{RwLock, mpsc, oneshot};

const CHAT_PAGE: &str = include_str!("web_chat.html");

/// Cap on request body size. Large enough for pasted screenshots
/// (`/api/upload`); chat commands are far smaller.
const MAX_BODY_BYTES: usize = 12_000_000;

#[derive(Debug, Clone)]
pub struct WebServerOptions {
    pub port: u16,
    pub initial_prompt: Option<String>,
    pub safe: bool,
    pub session_id: Option<String>,
    pub orchestration_context: Option<OrchestrationContext>,
}

// ── Session lifecycle ──────────────────────────────────────────────

/// Runtime info about the currently active session.
struct LiveSession {
    socket_path: PathBuf,
    session_id: String,
    model: String, // cached startup model; build_current_info reads fresh from disk
}

/// Commands that the HTTP handlers can send to the main event loop to
/// manage session lifecycle.
enum SessionCommand {
    Switch {
        session_id: String,
        response: oneshot::Sender<Result<String, String>>,
    },
    New {
        response: oneshot::Sender<Result<String, String>>,
    },
    /// Change provider and/or model, then resume the SAME session so the
    /// conversation continues. Resume takes the model from the persisted
    /// snapshot and the provider from config, so both are updated here.
    SetModel {
        provider: Option<String>,
        model: Option<String>,
        response: oneshot::Sender<Result<String, String>>,
    },
    /// Rewind the conversation to a past user message (dropping it and
    /// everything after) and resume the same session. The client re-sends the
    /// (possibly edited) prompt once its event stream is reconnected, which
    /// avoids racing the resumed turn's first events. Powers regenerate and
    /// edit-and-resend.
    Rewind {
        index_from_end: usize,
        expected_text: String,
        response: oneshot::Sender<Result<String, String>>,
    },
    /// Resume the current session so a just-updated config (permission mode,
    /// extended thinking) takes effect, preserving the session's model and
    /// conversation. The config is mutated by the HTTP handler before this.
    ApplySettings {
        response: oneshot::Sender<Result<String, String>>,
    },
    /// Clone a session (its snapshot + event log) under a new id and switch to
    /// it. Runs the clone AFTER shutting down the live session so forking the
    /// active session captures its freshest state, not a stale checkpoint.
    Fork {
        source_id: String,
        response: oneshot::Sender<Result<String, String>>,
    },
}

/// Shared mutable state accessible from all HTTP handler tasks.
struct AppState {
    live: RwLock<Option<LiveSession>>,
    /// Mutable so /api/model can switch provider/model for later sessions.
    config: RwLock<DcodeAiConfig>,
    workspace_root: PathBuf,
    _token: String,
    session_cmd_tx: mpsc::UnboundedSender<SessionCommand>,
}

fn start_session_thread(
    config: DcodeAiConfig,
    workspace_root: PathBuf,
    options: WebServerOptions,
    kind: ServiceSessionKind,
) -> Result<(LiveSession, std::thread::JoinHandle<()>), String> {
    let request = ServiceSessionRequest {
        config: config.clone(),
        workspace_root: workspace_root.clone(),
        safe_mode: options.safe,
        initial_prompt: options.initial_prompt,
        orchestration_context: options.orchestration_context,
        kind,
    };

    let mut handle: ServiceSessionHandle = spawn_service_session(request)?;
    let info = handle.info().clone();
    let socket_path = info
        .socket_path
        .clone()
        .ok_or_else(|| "session started without an IPC endpoint".to_string())?;

    let live = LiveSession {
        socket_path,
        session_id: info.session_id,
        model: info.model,
    };

    let join_handle = handle
        .take_join_handle()
        .ok_or_else(|| "missing session thread join handle".to_string())?;

    Ok((live, join_handle))
}

pub async fn run_web_server(
    config: DcodeAiConfig,
    workspace_root: PathBuf,
    options: WebServerOptions,
) -> anyhow::Result<()> {
    // ── start initial session ──────────────────────────────────────
    let initial_kind = match &options.session_id {
        Some(sid) => ServiceSessionKind::Resume {
            session_id: sid.clone(),
        },
        None => ServiceSessionKind::New { session_id: None },
    };

    let (live, current_join) = start_session_thread(
        config.clone(),
        workspace_root.clone(),
        options.clone(),
        initial_kind,
    )
    .map_err(|e| anyhow::anyhow!("{}", e))?;
    let mut current_join = Some(current_join);

    let token = format!("{:032x}", rand::random::<u128>());
    let (session_cmd_tx, mut session_cmd_rx) = mpsc::unbounded_channel::<SessionCommand>();

    let state = Arc::new(AppState {
        live: RwLock::new(Some(live)),
        config: RwLock::new(config.clone()),
        workspace_root: workspace_root.clone(),
        _token: token.clone(),
        session_cmd_tx,
    });

    // ── print startup banner ───────────────────────────────────────
    {
        let guard = state.live.read().await;
        if let Some(ls) = guard.as_ref() {
            println!("dcode-ai web chat");
            println!("  session:   {}", ls.session_id);
            println!("  model:     {}", ls.model);
            println!("  workspace: {}", workspace_root.display());
            println!();
            println!("  open:  http://127.0.0.1:{}/?t={}", options.port, token);
            println!();
            println!("Press Ctrl+C to stop.");
        }
    }

    let listener = TcpListener::bind(("127.0.0.1", options.port)).await?;

    // ── main event loop ────────────────────────────────────────────
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _peer)) = accepted else { continue };
                let state = state.clone();
                let token = token.clone();
                tokio::spawn(async move {
                    let _ = handle_http(stream, &state, &token).await;
                });
            }
            cmd = session_cmd_rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    SessionCommand::Switch { session_id, response } => {
                        let result = do_switch_session(
                            &state,
                            &mut current_join,
                            &workspace_root,
                            ServiceSessionKind::Resume { session_id },
                        ).await;
                        let _ = response.send(result);
                    }
                    SessionCommand::New { response } => {
                        let result = do_switch_session(
                            &state,
                            &mut current_join,
                            &workspace_root,
                            ServiceSessionKind::New { session_id: None },
                        ).await;
                        let _ = response.send(result);
                    }
                    SessionCommand::SetModel { provider, model, response } => {
                        let result = do_set_model(
                            &state,
                            &mut current_join,
                            &workspace_root,
                            provider,
                            model,
                        ).await;
                        let _ = response.send(result);
                    }
                    SessionCommand::Rewind { index_from_end, expected_text, response } => {
                        let result = do_rewind(
                            &state,
                            &mut current_join,
                            &workspace_root,
                            index_from_end,
                            &expected_text,
                        ).await;
                        let _ = response.send(result);
                    }
                    SessionCommand::ApplySettings { response } => {
                        // Config was already updated by the handler; resume the
                        // same session (model preserved) to pick it up.
                        let sid = {
                            let guard = state.live.read().await;
                            guard.as_ref().map(|ls| ls.session_id.clone())
                        };
                        shutdown_current(&state, &mut current_join).await;
                        let kind = match sid {
                            Some(session_id) => ServiceSessionKind::Resume { session_id },
                            None => ServiceSessionKind::New { session_id: None },
                        };
                        let result =
                            start_kind(&state, &mut current_join, &workspace_root, kind, None).await;
                        let _ = response.send(result);
                    }
                    SessionCommand::Fork { source_id, response } => {
                        let result = do_fork(
                            &state,
                            &mut current_join,
                            &workspace_root,
                            &source_id,
                        ).await;
                        let _ = response.send(result);
                    }
                }
            }
            _ = tokio::signal::ctrl_c() => {
                // Shutdown the current session if any
                let guard = state.live.read().await;
                if let Some(ls) = guard.as_ref() {
                    let client = IpcClient::new(ls.socket_path.clone());
                    let _ = client.send_command(&AgentCommand::Shutdown).await;
                }
                drop(guard);
                break;
            }
        }
    }

    // Wait for the final session thread to finish.
    if let Some(jh) = current_join.take() {
        let _ = tokio::task::spawn_blocking(move || {
            let _ = jh.join();
        })
        .await;
    }

    Ok(())
}

/// Shut down the live session (if any) and wait for its thread to exit, so
/// its final state save completes before anyone touches the snapshot.
async fn shutdown_current(
    state: &AppState,
    current_join: &mut Option<std::thread::JoinHandle<()>>,
) {
    {
        let mut guard = state.live.write().await;
        if let Some(old) = guard.take() {
            let client = IpcClient::new(old.socket_path.clone());
            let _ = client.send_command(&AgentCommand::Shutdown).await;
        }
    }
    if let Some(jh) = current_join.take() {
        let _ = tokio::task::spawn_blocking(move || {
            let _ = jh.join();
        })
        .await;
    }
}

/// Start a session of the given kind with the current config and publish it
/// as the live session. `initial_prompt`, if set, is run automatically once
/// the session is ready (used by rewind to re-run an edited turn).
async fn start_kind(
    state: &AppState,
    current_join: &mut Option<std::thread::JoinHandle<()>>,
    workspace_root: &std::path::Path,
    kind: ServiceSessionKind,
    initial_prompt: Option<String>,
) -> Result<String, String> {
    let config = state.config.read().await.clone();
    let (new_live, join_handle) = start_session_thread(
        config,
        workspace_root.to_path_buf(),
        WebServerOptions {
            port: 0,
            initial_prompt,
            safe: false,
            session_id: None,
            orchestration_context: None,
        },
        kind,
    )?;

    let new_session_id = new_live.session_id.clone();
    *current_join = Some(join_handle);
    {
        let mut guard = state.live.write().await;
        *guard = Some(new_live);
    }
    Ok(new_session_id)
}

/// Kill the current session and start a new one of the given kind.
async fn do_switch_session(
    state: &AppState,
    current_join: &mut Option<std::thread::JoinHandle<()>>,
    workspace_root: &std::path::Path,
    kind: ServiceSessionKind,
) -> Result<String, String> {
    shutdown_current(state, current_join).await;
    start_kind(state, current_join, workspace_root, kind, None).await
}

/// Switch provider and/or model, keeping the current conversation: update the
/// config (provider comes from there on resume), rewrite the persisted
/// snapshot's model (resume takes the model from the snapshot), then restart
/// the session with `Resume` on the same id.
async fn do_set_model(
    state: &AppState,
    current_join: &mut Option<std::thread::JoinHandle<()>>,
    workspace_root: &std::path::Path,
    provider: Option<String>,
    model: Option<String>,
) -> Result<String, String> {
    let resolved_model = {
        let mut cfg = state.config.write().await;
        if let Some(p) = provider.as_deref() {
            if let Some(base_url) = local_preset_base(p) {
                // Local OpenAI-compatible server (web mirror of `/connect`):
                // the sentinel key keeps the OpenAI provider from demanding a
                // real one; the live model list then comes from {base}/models.
                cfg.set_default_provider(ProviderKind::OpenAi);
                cfg.provider.openai.base_url = base_url.to_string();
                cfg.provider.openai.api_key = Some("local".to_string());
                cfg.sync_default_model_from_provider();
            } else {
                let kind = ProviderKind::from_cli_name(p)
                    .ok_or_else(|| format!("unknown provider: {p}"))?;
                cfg.set_default_provider(kind);
            }
        }
        if let Some(m) = model.as_deref().filter(|m| !m.trim().is_empty()) {
            cfg.apply_model_override(m);
        }
        cfg.model.default_model.clone()
    };

    let current_id = {
        let guard = state.live.read().await;
        guard.as_ref().map(|ls| ls.session_id.clone())
    };

    // Shut the session down FIRST: its finish() saves the snapshot, and any
    // model patch written before that save would be silently overwritten
    // (the "switch sometimes keeps the old provider/model" bug).
    shutdown_current(state, current_join).await;

    let kind = match current_id {
        Some(sid) => {
            // Persist the new model into the snapshot so resume picks it up.
            let store = SessionStore::new(workspace_root.join(".dcode-ai").join("sessions"));
            match store.load(&sid).await {
                Ok(mut session) => {
                    session.meta.model = resolved_model.clone();
                    store
                        .save(&session)
                        .await
                        .map_err(|e| format!("failed to persist model change: {e}"))?;
                    ServiceSessionKind::Resume { session_id: sid }
                }
                // Snapshot unreadable (e.g. brand-new session not yet saved):
                // fall back to a fresh session with the new config.
                Err(_) => ServiceSessionKind::New { session_id: None },
            }
        }
        None => ServiceSessionKind::New { session_id: None },
    };

    start_kind(state, current_join, workspace_root, kind, None).await
}

/// The transcript-facing text of a message — the SAME `event_preview` the
/// `MessageReceived` event carried, so the frontend's recorded text matches
/// (for user messages this compacts inlined `@file` mentions back to `@path`).
fn message_text(msg: &dcode_ai_common::message::Message) -> String {
    msg.event_preview()
}

/// Two trimmed texts refer to the same message if either contains the other
/// (the stored form may inline `@file` mentions the transcript shows compact).
fn texts_match(a: &str, b: &str) -> bool {
    let (a, b) = (a.trim(), b.trim());
    a == b || a.contains(b) || b.contains(a)
}

/// Rewind the active session to the user message `index_from_end` back
/// (0 = most recent), dropping it and everything after from BOTH the snapshot
/// and the event log (so history replay matches the model context), then
/// resume the session. The client re-sends the prompt after reconnecting.
async fn do_rewind(
    state: &AppState,
    current_join: &mut Option<std::thread::JoinHandle<()>>,
    workspace_root: &std::path::Path,
    index_from_end: usize,
    expected_text: &str,
) -> Result<String, String> {
    use dcode_ai_common::message::Role;

    // Capture the id before shutdown clears the live slot.
    let Some(sid) = ({
        let guard = state.live.read().await;
        guard.as_ref().map(|ls| ls.session_id.clone())
    }) else {
        return Err("no active session to rewind".into());
    };

    // Snapshot is only saved on shutdown, so stop the session first.
    shutdown_current(state, current_join).await;

    let sessions_dir = workspace_root.join(".dcode-ai").join("sessions");
    let store = SessionStore::new(sessions_dir.clone());
    let mut session = store
        .load(&sid)
        .await
        .map_err(|e| format!("failed to load session: {e}"))?;

    // Locate the target user message in the snapshot.
    let user_positions: Vec<usize> = session
        .messages
        .iter()
        .enumerate()
        .filter(|(_, m)| matches!(m.role, Role::User))
        .map(|(i, _)| i)
        .collect();
    if index_from_end >= user_positions.len() {
        return Err("that message is no longer in the model context".into());
    }
    let pos = user_positions[user_positions.len() - 1 - index_from_end];
    if !texts_match(&message_text(&session.messages[pos]), expected_text) {
        return Err("transcript and model history diverged — not rewinding".into());
    }
    session.messages.truncate(pos);
    store
        .save(&session)
        .await
        .map_err(|e| format!("failed to save rewound session: {e}"))?;

    // Truncate the event log to the matching user MessageReceived line so
    // replay reflects the rewind. Best-effort: a mismatch leaves the log intact
    // rather than corrupting it (the model context is already correct).
    let log_path = sessions_dir.join(format!("{sid}.events.jsonl"));
    if let Ok(text) = tokio::fs::read_to_string(&log_path).await {
        let lines: Vec<&str> = text.lines().collect();
        let user_line_idxs: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter_map(|(i, line)| {
                let v: serde_json::Value = serde_json::from_str(line).ok()?;
                let ev = v.get("event")?;
                (ev.get("type")?.as_str()? == "MessageReceived"
                    && ev.get("role")?.as_str()? == "user")
                    .then_some(i)
            })
            .collect();
        if index_from_end < user_line_idxs.len() {
            let cut = user_line_idxs[user_line_idxs.len() - 1 - index_from_end];
            let kept = lines[..cut].join("\n");
            let kept = if kept.is_empty() {
                kept
            } else {
                format!("{kept}\n")
            };
            let _ = tokio::fs::write(&log_path, kept).await;
        }
    }

    start_kind(
        state,
        current_join,
        workspace_root,
        ServiceSessionKind::Resume { session_id: sid },
        None,
    )
    .await
}

/// Clone `source_id`'s snapshot + event log under a new id, then switch to it.
/// Shuts the live session down first so forking the active session captures
/// its freshest state.
async fn do_fork(
    state: &AppState,
    current_join: &mut Option<std::thread::JoinHandle<()>>,
    workspace_root: &std::path::Path,
    source_id: &str,
) -> Result<String, String> {
    use dcode_ai_common::session::SessionStatus;

    shutdown_current(state, current_join).await;

    let sessions_dir = workspace_root.join(".dcode-ai").join("sessions");
    let store = SessionStore::new(sessions_dir.clone());
    let mut session = store
        .load(source_id)
        .await
        .map_err(|e| format!("failed to load session: {e}"))?;

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let new_id = format!("session-fork-{nanos}");

    session.meta.session_name = Some(match session.meta.session_name {
        Some(name) => format!("{name} (fork)"),
        None => "Fork".to_string(),
    });
    session.meta.parent_session_id = Some(session.meta.id.clone());
    session.meta.id = new_id.clone();
    session.meta.created_at = chrono::Utc::now();
    session.meta.updated_at = chrono::Utc::now();
    session.meta.pid = None;
    session.meta.socket_path = None;
    session.meta.status = SessionStatus::Completed;
    store
        .save(&session)
        .await
        .map_err(|e| format!("failed to save fork: {e}"))?;

    // Copy the event log so the fork replays its inherited history.
    let src_log = sessions_dir.join(format!("{source_id}.events.jsonl"));
    let dst_log = sessions_dir.join(format!("{new_id}.events.jsonl"));
    let _ = tokio::fs::copy(&src_log, &dst_log).await;

    start_kind(
        state,
        current_join,
        workspace_root,
        ServiceSessionKind::Resume { session_id: new_id },
        None,
    )
    .await
}

// ── HTTP handling ──────────────────────────────────────────────────

struct Request {
    method: String,
    path: String,
    query: String,
    body: Vec<u8>,
    /// Raw `Cookie:` header value, if present (used for token auth so the
    /// token need not ride in the URL after the first load).
    cookie: String,
}

async fn read_request(reader: &mut BufReader<tokio::io::ReadHalf<TcpStream>>) -> Option<Request> {
    let mut request_line = String::new();
    if reader.read_line(&mut request_line).await.ok()? == 0 {
        return None;
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target, String::new()),
    };

    let mut content_length: usize = 0;
    let mut cookie = String::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await.ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim();
            if name.eq_ignore_ascii_case("content-length") {
                content_length = value.trim().parse().unwrap_or(0);
            } else if name.eq_ignore_ascii_case("cookie") {
                cookie = value.trim().to_string();
            }
        }
    }
    if content_length > MAX_BODY_BYTES {
        return None;
    }

    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).await.ok()?;
    }
    Some(Request {
        method,
        path,
        query,
        body,
        cookie,
    })
}

/// Extract a cookie value from a raw `Cookie:` header.
fn cookie_value<'a>(cookie_header: &'a str, key: &str) -> Option<&'a str> {
    cookie_header
        .split(';')
        .filter_map(|pair| pair.split_once('='))
        .map(|(k, v)| (k.trim(), v.trim()))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

/// Minimal application/x-www-form-urlencoded decoder for query values.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                {
                    Some(v) => {
                        out.push(v);
                        i += 3;
                    }
                    None => {
                        out.push(b'%');
                        i += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// (mime, is_image) by file extension — mirrors the TUI attachment staging.
fn media_type_for(ext: &str) -> (&'static str, bool) {
    match ext.to_ascii_lowercase().as_str() {
        "png" => ("image/png", true),
        "jpg" | "jpeg" => ("image/jpeg", true),
        "webp" => ("image/webp", true),
        "gif" => ("image/gif", true),
        "svg" => ("image/svg+xml", false), // not accepted as native model input
        "txt" | "md" | "log" => ("text/plain", false),
        "json" => ("application/json", false),
        "pdf" => ("application/pdf", false),
        _ => ("application/octet-stream", false),
    }
}

/// Build a fresh info JSON on every call by reading the session snapshot
/// from disk (which persists model changes from `/model` overrides).
async fn build_current_info(state: &AppState) -> String {
    let cfg = state.config.read().await.clone();
    // Live catalog for the active provider. Disk-cached by the API, so this is
    // a fast local read after the first fetch; on failure (offline / no key)
    // we just fall back to the curated list inside `build_info`.
    let live_models = dcode_ai_runtime::model_limits_api::fetch_provider_model_ids(&cfg)
        .await
        .unwrap_or_default();
    let mut info = build_info(&cfg, &live_models);

    let guard = state.live.read().await;
    if let Some(ls) = guard.as_ref() {
        // Try to get the current model from the persisted session snapshot.
        let current_model = {
            let session_dir = state.workspace_root.join(".dcode-ai").join("sessions");
            let store = SessionStore::new(session_dir);
            match store.load_snapshot(&ls.session_id).await {
                Ok(snap) => snap.model,
                Err(_) => ls.model.clone(), // fallback to cached startup model
            }
        };

        // Context-window size for the ctx gauge: prefer the API-cached value,
        // fall back to the static per-model table.
        let context_window =
            dcode_ai_runtime::model_limits_api::cached_context_window(&cfg, &current_model)
                .unwrap_or_else(|| {
                    dcode_ai_runtime::model_limits::detect_context_window(&current_model)
                });

        if let Some(obj) = info.as_object_mut() {
            obj.insert(
                "session".into(),
                serde_json::json!({
                    "session_id": ls.session_id,
                    "model": current_model,
                    "context_window": context_window,
                }),
            );
        }
    }
    drop(guard);
    info.to_string()
}

/// Local OpenAI-compatible server presets, mirroring the TUI's
/// `/connect ollama|lmstudio|vllm` (see `repl.rs::local_preset_for`).
/// (dropdown key, display name, base URL)
const LOCAL_PRESETS: &[(&str, &str, &str)] = &[
    ("ollama", "Ollama (local)", "http://localhost:11434/v1"),
    ("lmstudio", "LM Studio (local)", "http://localhost:1234/v1"),
    ("vllm", "vLLM (local)", "http://localhost:8000/v1"),
];

fn local_preset_base(key: &str) -> Option<&'static str> {
    LOCAL_PRESETS
        .iter()
        .find(|(preset_key, _, _)| *preset_key == key)
        .map(|(_, _, base)| *base)
}

/// A provider name that round-trips through [`ProviderKind::from_cli_name`].
/// NOT `to_config_key()`: that maps Antigravity to "openai" (shared config
/// block), which would make the dropdown switch the wrong provider.
fn switch_key(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::OpenAi => "openai",
        ProviderKind::Anthropic => "anthropic",
        ProviderKind::OpenRouter => "openrouter",
        ProviderKind::Antigravity => "antigravity",
        ProviderKind::OpenCodeZen => "opencodezen",
    }
}

/// Small curated fallback so a provider's dropdown isn't empty before its live
/// catalog has been fetched (only the active provider gets live models).
fn fallback_models(kind: ProviderKind) -> Vec<&'static str> {
    match kind {
        ProviderKind::OpenCodeZen => vec!["big-pickle", "minimax-m2.5"],
        ProviderKind::OpenAi => vec!["gpt-5.2", "gpt-4o", "o3-mini"],
        ProviderKind::Anthropic => {
            vec![
                "claude-opus-4-8",
                "claude-sonnet-4-6-20250514",
                "claude-haiku-4-5-20251001",
            ]
        }
        ProviderKind::OpenRouter => {
            vec![
                "anthropic/claude-sonnet-4-6",
                "openai/gpt-5.2",
                "google/gemini-2.5-pro",
            ]
        }
        ProviderKind::Antigravity => vec!["gemini-2.5-pro", "gemini-2.5-flash"],
    }
}

/// Session/provider snapshot for the page sidebar. Never includes key
/// material — only presence flags, env-var names, and non-secret identifiers.
/// `live_models` are the real ids fetched from the active provider's API; they
/// replace the fallback list for the currently-selected provider only.
fn build_info(config: &DcodeAiConfig, live_models: &[String]) -> serde_json::Value {
    // When a local preset is connected, the default provider is OpenAi with a
    // localhost base_url — the preset entry should show as selected, not the
    // plain OpenAI row.
    let active_base = config
        .provider
        .base_url_for(config.provider.default)
        .trim_end_matches('/')
        .to_string();
    let active_preset: Option<&str> = (config.provider.default == ProviderKind::OpenAi)
        .then(|| {
            LOCAL_PRESETS
                .iter()
                .find(|(_, _, base)| base.trim_end_matches('/') == active_base)
                .map(|(key, _, _)| *key)
        })
        .flatten();

    let mut providers: Vec<serde_json::Value> = ProviderKind::ALL
        .into_iter()
        .map(|kind| {
            let is_active = kind == config.provider.default
                && !(kind == ProviderKind::OpenAi && active_preset.is_some());
            // Live catalog for the active provider (deduped, configured model
            // first so the current selection is always present); curated
            // fallback for the rest.
            let models: Vec<String> = if is_active && !live_models.is_empty() {
                let configured = config.provider.model_for(kind).to_string();
                let mut seen = std::collections::HashSet::new();
                std::iter::once(configured)
                    .chain(live_models.iter().cloned())
                    .filter(|m| !m.trim().is_empty() && seen.insert(m.clone()))
                    .collect()
            } else {
                fallback_models(kind)
                    .into_iter()
                    .map(String::from)
                    .collect()
            };
            serde_json::json!({
                "name": kind.display_name(),
                "key": switch_key(kind),
                "selected": is_active,
                "model": config.provider.model_for(kind),
                "models": models,
                "models_live": is_active && !live_models.is_empty(),
                "base_url": config.provider.base_url_for(kind),
                "api_key_env": config.provider.api_key_env_for(kind),
                "api_key_present": config.provider.api_key_present_for(kind),
            })
        })
        .collect();

    // Local OpenAI-compatible presets (web mirror of `/connect ollama` etc.).
    // No key needed; the live model list arrives once the preset is selected
    // (fetched from the local server's /models).
    for (key, name, base) in LOCAL_PRESETS {
        let is_active = active_preset == Some(*key);
        let models: Vec<String> = if is_active && !live_models.is_empty() {
            live_models.to_vec()
        } else {
            Vec::new()
        };
        providers.push(serde_json::json!({
            "name": name,
            "key": key,
            "selected": is_active,
            "model": if is_active { config.provider.openai.model.clone() } else { String::new() },
            "models": models,
            "models_live": is_active && !live_models.is_empty(),
            "base_url": base,
            "api_key_env": "",
            "api_key_present": true,
        }));
    }

    serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "default_provider": config.provider.default.display_name(),
        "default_model": config.model.default_model,
        "permission_mode": format!("{:?}", config.permissions.mode),
        "thinking": config.model.enable_thinking,
        "max_tokens": config.model.max_tokens,
        "temperature": active_provider_temperature(config),
        "shell": dcode_ai_common::shell::workspace_shell().display_name(),
        "providers": providers,
    })
}

/// Temperature of the currently-selected provider (Antigravity shares the
/// OpenAI config block).
fn active_provider_temperature(config: &DcodeAiConfig) -> f32 {
    match config.provider.default {
        ProviderKind::OpenAi | ProviderKind::Antigravity => config.provider.openai.temperature,
        ProviderKind::Anthropic => config.provider.anthropic.temperature,
        ProviderKind::OpenRouter => config.provider.openrouter.temperature,
        ProviderKind::OpenCodeZen => config.provider.opencodezen.temperature,
    }
}

/// Build the session list payload from the session store on disk.
/// Directory/file names never shown in the file-tree explorer.
fn is_ignored_entry(name: &str) -> bool {
    matches!(
        name,
        ".git"
            | "target"
            | "node_modules"
            | ".dcode-ai"
            | "dist"
            | "build"
            | ".next"
            | ".venv"
            | "__pycache__"
            | ".idea"
            | ".vscode"
            | ".DS_Store"
    )
}

/// Resolve `rel` under `root` and confirm (via canonicalization) it stays
/// inside the workspace — blocks `..`/symlink escapes.
async fn resolve_in_workspace(root: &std::path::Path, rel: &str) -> Option<PathBuf> {
    let rel = rel.trim_start_matches(['/', '\\']);
    if rel.split(['/', '\\']).any(|c| c == "..") {
        return None;
    }
    let candidate = if rel.is_empty() {
        root.to_path_buf()
    } else {
        root.join(rel)
    };
    let root_real = tokio::fs::canonicalize(root).await.ok()?;
    let target_real = tokio::fs::canonicalize(&candidate).await.ok()?;
    target_real.starts_with(&root_real).then_some(target_real)
}

/// Immediate children of a workspace directory (dirs first, then files, both
/// alphabetical). Never escapes the workspace root.
async fn list_dir(root: &std::path::Path, rel: &str) -> serde_json::Value {
    let Some(dir) = resolve_in_workspace(root, rel).await else {
        return serde_json::json!({ "entries": [], "error": "forbidden" });
    };
    let base_rel = rel.trim_start_matches(['/', '\\']).trim_end_matches('/');
    let mut dirs: Vec<serde_json::Value> = Vec::new();
    let mut files: Vec<serde_json::Value> = Vec::new();
    if let Ok(mut rd) = tokio::fs::read_dir(&dir).await {
        while let Ok(Some(entry)) = rd.next_entry().await {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') && name != ".env" || is_ignored_entry(&name) {
                continue;
            }
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            let child_rel = if base_rel.is_empty() {
                name.clone()
            } else {
                format!("{base_rel}/{name}")
            };
            let node = serde_json::json!({ "name": name, "path": child_rel, "dir": is_dir });
            if is_dir {
                dirs.push(node)
            } else {
                files.push(node)
            }
        }
    }
    fn name_key(v: &serde_json::Value) -> String {
        v["name"].as_str().unwrap_or("").to_lowercase()
    }
    dirs.sort_by_key(name_key);
    files.sort_by_key(name_key);
    dirs.extend(files);
    serde_json::json!({ "entries": dirs })
}

/// Read a workspace text file for the viewer. Size-capped; rejects paths that
/// escape the workspace and obvious binary content.
async fn read_workspace_file(
    root: &std::path::Path,
    rel: &str,
) -> Result<String, (&'static str, &'static str)> {
    let path = resolve_in_workspace(root, rel)
        .await
        .ok_or(("403 Forbidden", "forbidden"))?;
    let meta = tokio::fs::metadata(&path)
        .await
        .map_err(|_| ("404 Not Found", "not found"))?;
    if !meta.is_file() {
        return Err(("400 Bad Request", "not a file"));
    }
    if meta.len() > 512_000 {
        return Err((
            "413 Payload Too Large",
            "file too large to preview (>500 KB)",
        ));
    }
    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|_| ("404 Not Found", "not found"))?;
    if bytes.contains(&0) {
        return Err((
            "415 Unsupported Media Type",
            "binary file — not previewable",
        ));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// `git diff HEAD -- <path>` for a workspace file (staged + unstaged changes
/// vs the last commit). Read-only; path is validated to stay in the workspace.
async fn git_diff(
    root: &std::path::Path,
    rel: &str,
) -> Result<String, (&'static str, &'static str)> {
    let path = resolve_in_workspace(root, rel)
        .await
        .ok_or(("403 Forbidden", "forbidden"))?;
    // Pass the path relative to the repo root; `-C` sets the working dir so the
    // command targets the workspace's repo.
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["diff", "--no-color", "HEAD", "--"])
        .arg(&path)
        .output()
        .await
        .map_err(|_| ("500 Internal Server Error", "git not found"))?;
    if !out.status.success() {
        // Non-zero usually means "not a git repo" or "no such commit (HEAD)".
        let err = String::from_utf8_lossy(&out.stderr);
        if err.contains("not a git repository") {
            return Err(("409 Conflict", "not a git repository"));
        }
        // Fall back to a plain working-tree diff (e.g. repo with no commits yet).
        let out2 = tokio::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["diff", "--no-color", "--"])
            .arg(&path)
            .output()
            .await
            .map_err(|_| ("500 Internal Server Error", "git failed"))?;
        return Ok(String::from_utf8_lossy(&out2.stdout).into_owned());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

async fn build_sessions_json(state: &AppState) -> serde_json::Value {
    let session_dir = state.workspace_root.join(".dcode-ai").join("sessions");
    let store = SessionStore::new(session_dir);
    let (snapshots, _unreadable) = store.load_all_snapshots().await.unwrap_or_default();

    // Sort by updated_at descending (most recent first)
    let mut sorted = snapshots;
    sorted.sort_by_key(|s| std::cmp::Reverse(s.updated_at));

    // Get current session id
    let current_id = {
        let guard = state.live.read().await;
        guard.as_ref().map(|ls| ls.session_id.clone())
    };

    let list: Vec<serde_json::Value> = sorted
        .into_iter()
        .map(|snap| {
            serde_json::json!({
                "id": snap.id,
                "session_name": snap.session_name,
                "model": snap.model,
                "status": snap.status,
                "created_at": snap.created_at.to_rfc3339(),
                "updated_at": snap.updated_at.to_rfc3339(),
                "current": current_id.as_deref() == Some(&snap.id),
            })
        })
        .collect();

    serde_json::json!({ "sessions": list })
}

async fn handle_http(stream: TcpStream, state: &AppState, token: &str) -> std::io::Result<()> {
    // Disable Nagle's algorithm for real-time SSE event streaming.
    let _ = stream.set_nodelay(true);
    let (read_half, mut writer) = tokio::io::split(stream);
    let mut reader = BufReader::new(read_half);
    let Some(request) = read_request(&mut reader).await else {
        return Ok(());
    };

    // Authenticate via the URL token (first load) or the cookie set from it
    // (every request after). The cookie lets the token leave the URL — see the
    // Set-Cookie on `GET /` below and the history.replaceState on the page.
    let authed = query_param(&request.query, "t") == Some(token)
        || cookie_value(&request.cookie, "dcode_ai_token") == Some(token);
    if !authed {
        return write_response(&mut writer, "403 Forbidden", "text/plain", b"forbidden").await;
    }

    match (request.method.as_str(), request.path.as_str()) {
        ("GET", "/") | ("GET", "/index.html") => {
            // Pin the token to an HttpOnly, same-site cookie so subsequent
            // requests (including EventSource, which can't send headers)
            // authenticate without the token in the URL.
            let set_cookie =
                format!("dcode_ai_token={token}; HttpOnly; SameSite=Strict; Path=/; Max-Age=86400");
            write_response_with_headers(
                &mut writer,
                "200 OK",
                "text/html; charset=utf-8",
                CHAT_PAGE.as_bytes(),
                &[("Set-Cookie", set_cookie.as_str())],
            )
            .await
        }
        ("GET", "/events") => {
            let socket_path = {
                let guard = state.live.read().await;
                guard.as_ref().map(|ls| ls.socket_path.clone())
            };
            match socket_path {
                Some(path) => stream_events(&mut writer, &path).await,
                None => {
                    write_response(
                        &mut writer,
                        "503 Service Unavailable",
                        "text/plain",
                        b"no active session",
                    )
                    .await
                }
            }
        }
        ("GET", "/api/info") => {
            let body = build_current_info(state).await;
            write_response(&mut writer, "200 OK", "application/json", body.as_bytes()).await
        }
        ("GET", "/api/history") => {
            let session_id = {
                let guard = state.live.read().await;
                guard.as_ref().map(|ls| ls.session_id.clone())
            };
            match session_id {
                Some(id) => {
                    let log_path = state
                        .workspace_root
                        .join(".dcode-ai")
                        .join("sessions")
                        .join(format!("{}.events.jsonl", id));
                    if let Ok(contents) = tokio::fs::read(&log_path).await {
                        write_response(&mut writer, "200 OK", "application/x-ndjson", &contents)
                            .await
                    } else {
                        write_response(&mut writer, "200 OK", "application/x-ndjson", b"").await
                    }
                }
                None => {
                    write_response(&mut writer, "503 Service Unavailable", "text/plain", b"").await
                }
            }
        }
        ("GET", "/api/sessions") => {
            let body = build_sessions_json(state).await;
            write_response(
                &mut writer,
                "200 OK",
                "application/json",
                body.to_string().as_bytes(),
            )
            .await
        }
        ("POST", "/api/command") => {
            let Ok(command) = serde_json::from_slice::<AgentCommand>(&request.body) else {
                return write_response(
                    &mut writer,
                    "400 Bad Request",
                    "application/json",
                    br#"{"ok":false,"error":"invalid AgentCommand"}"#,
                )
                .await;
            };
            let socket_path = {
                let guard = state.live.read().await;
                guard.as_ref().map(|ls| ls.socket_path.clone())
            };
            match socket_path {
                Some(path) => {
                    let client = IpcClient::new(path);
                    match client.send_command(&command).await {
                        Ok(()) => {
                            write_response(
                                &mut writer,
                                "200 OK",
                                "application/json",
                                br#"{"ok":true}"#,
                            )
                            .await
                        }
                        Err(err) => {
                            let body = serde_json::json!({ "ok": false, "error": err.to_string() });
                            write_response(
                                &mut writer,
                                "502 Bad Gateway",
                                "application/json",
                                body.to_string().as_bytes(),
                            )
                            .await
                        }
                    }
                }
                None => {
                    write_response(
                        &mut writer,
                        "503 Service Unavailable",
                        "application/json",
                        br#"{"ok":false,"error":"no active session"}"#,
                    )
                    .await
                }
            }
        }
        ("POST", "/api/sessions/new") => {
            let (resp_tx, resp_rx) = oneshot::channel();
            if state
                .session_cmd_tx
                .send(SessionCommand::New { response: resp_tx })
                .is_err()
            {
                return write_response(
                    &mut writer,
                    "500 Internal Server Error",
                    "application/json",
                    br#"{"ok":false,"error":"server shutting down"}"#,
                )
                .await;
            }
            match resp_rx.await {
                Ok(Ok(new_id)) => {
                    let body = serde_json::json!({"ok": true, "session_id": new_id});
                    write_response(
                        &mut writer,
                        "200 OK",
                        "application/json",
                        body.to_string().as_bytes(),
                    )
                    .await
                }
                Ok(Err(e)) => {
                    let body = serde_json::json!({"ok": false, "error": e});
                    write_response(
                        &mut writer,
                        "500 Internal Server Error",
                        "application/json",
                        body.to_string().as_bytes(),
                    )
                    .await
                }
                Err(_) => {
                    write_response(
                        &mut writer,
                        "500 Internal Server Error",
                        "application/json",
                        br#"{"ok":false,"error":"session switch failed"}"#,
                    )
                    .await
                }
            }
        }
        ("POST", "/api/upload") => {
            if request.body.is_empty() {
                return write_response(
                    &mut writer,
                    "400 Bad Request",
                    "application/json",
                    br#"{"ok":false,"error":"empty body"}"#,
                )
                .await;
            }
            let session_id = {
                let guard = state.live.read().await;
                guard.as_ref().map(|ls| ls.session_id.clone())
            };
            let Some(sid) = session_id else {
                return write_response(
                    &mut writer,
                    "503 Service Unavailable",
                    "application/json",
                    br#"{"ok":false,"error":"no active session"}"#,
                )
                .await;
            };
            let name = query_param(&request.query, "name")
                .map(percent_decode)
                .unwrap_or_else(|| "file.bin".to_string());
            let base = name
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or("file.bin")
                .to_string();
            let ext: String = base
                .rsplit_once('.')
                .map(|(_, e)| e.chars().filter(|c| c.is_ascii_alphanumeric()).collect())
                .filter(|e: &String| !e.is_empty())
                .unwrap_or_else(|| "bin".to_string());
            let (media_type, is_image) = media_type_for(&ext);
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            let filename = format!("upload-{nanos}.{ext}");
            let dir = state
                .workspace_root
                .join(".dcode-ai")
                .join("sessions")
                .join(&sid)
                .join("attachments");
            if let Err(e) = tokio::fs::create_dir_all(&dir).await {
                let body = serde_json::json!({"ok": false, "error": e.to_string()});
                return write_response(
                    &mut writer,
                    "500 Internal Server Error",
                    "application/json",
                    body.to_string().as_bytes(),
                )
                .await;
            }
            if let Err(e) = tokio::fs::write(dir.join(&filename), &request.body).await {
                let body = serde_json::json!({"ok": false, "error": e.to_string()});
                return write_response(
                    &mut writer,
                    "500 Internal Server Error",
                    "application/json",
                    body.to_string().as_bytes(),
                )
                .await;
            }
            let rel = format!(".dcode-ai/sessions/{sid}/attachments/{filename}");
            let body = serde_json::json!({
                "ok": true,
                "path": rel,
                "media_type": media_type,
                "is_image": is_image,
                "name": base,
            });
            write_response(
                &mut writer,
                "200 OK",
                "application/json",
                body.to_string().as_bytes(),
            )
            .await
        }
        ("GET", "/api/file") => {
            let Some(path) = query_param(&request.query, "path").map(percent_decode) else {
                return write_response(
                    &mut writer,
                    "400 Bad Request",
                    "text/plain",
                    b"missing path",
                )
                .await;
            };
            // Only session attachments are servable — never arbitrary workspace
            // files. Prefix check is a cheap first gate; the authoritative check
            // canonicalizes the resolved path and confirms containment, so
            // symlinks and `..` cannot escape the sessions dir.
            if !path.starts_with(".dcode-ai/sessions/") {
                return write_response(&mut writer, "403 Forbidden", "text/plain", b"forbidden")
                    .await;
            }
            let base = state.workspace_root.join(".dcode-ai").join("sessions");
            let requested = state.workspace_root.join(&path);
            match (
                tokio::fs::canonicalize(&base).await,
                tokio::fs::canonicalize(&requested).await,
            ) {
                (Ok(base_real), Ok(target_real)) if target_real.starts_with(&base_real) => {
                    let ext = path.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
                    let (mime, _) = media_type_for(ext);
                    match tokio::fs::read(&target_real).await {
                        Ok(bytes) => write_response(&mut writer, "200 OK", mime, &bytes).await,
                        Err(_) => {
                            write_response(&mut writer, "404 Not Found", "text/plain", b"not found")
                                .await
                        }
                    }
                }
                // Resolves outside the sessions dir → traversal attempt.
                (Ok(_), Ok(_)) => {
                    write_response(&mut writer, "403 Forbidden", "text/plain", b"forbidden").await
                }
                // Target (or base) doesn't exist / unreadable.
                _ => write_response(&mut writer, "404 Not Found", "text/plain", b"not found").await,
            }
        }
        ("GET", "/api/search") => {
            let q = query_param(&request.query, "q")
                .map(percent_decode)
                .unwrap_or_default()
                .to_lowercase();
            let mut matches: Vec<serde_json::Value> = Vec::new();
            if q.trim().len() >= 2 {
                let dir = state.workspace_root.join(".dcode-ai").join("sessions");
                if let Ok(mut rd) = tokio::fs::read_dir(&dir).await {
                    while let Ok(Some(entry)) = rd.next_entry().await {
                        if matches.len() >= 20 {
                            break;
                        }
                        let name = entry.file_name().to_string_lossy().into_owned();
                        let Some(sid) = name.strip_suffix(".events.jsonl") else {
                            continue;
                        };
                        let Ok(text) = tokio::fs::read_to_string(entry.path()).await else {
                            continue;
                        };
                        for line in text.lines() {
                            if !line.contains("MessageReceived") {
                                continue;
                            }
                            let lower = line.to_lowercase();
                            let Some(pos) = lower.find(&q) else { continue };
                            // Char-based snippet to stay on UTF-8 boundaries.
                            let cpos = lower[..pos].chars().count();
                            let snippet: String = lower
                                .chars()
                                .skip(cpos.saturating_sub(40))
                                .take(q.chars().count() + 90)
                                .collect();
                            matches.push(serde_json::json!({
                                "session_id": sid,
                                "snippet": snippet,
                            }));
                            break; // one hit per session is enough for the list
                        }
                    }
                }
            }
            let body = serde_json::json!({ "matches": matches });
            write_response(
                &mut writer,
                "200 OK",
                "application/json",
                body.to_string().as_bytes(),
            )
            .await
        }
        ("GET", "/api/files") => {
            // Flat workspace file list for `@` mention completion (bounded
            // walk, same discovery the TUI composer uses).
            let files = crate::file_mentions::discover_workspace_files(&state.workspace_root);
            let body = serde_json::json!({ "files": files });
            write_response(
                &mut writer,
                "200 OK",
                "application/json",
                body.to_string().as_bytes(),
            )
            .await
        }
        ("GET", "/api/tree") => {
            // Immediate children of `dir` (relative to workspace root; empty =
            // root). Lazy per-directory listing scales to large repos.
            let rel = query_param(&request.query, "dir")
                .map(percent_decode)
                .unwrap_or_default();
            let body = list_dir(&state.workspace_root, &rel).await;
            write_response(
                &mut writer,
                "200 OK",
                "application/json",
                body.to_string().as_bytes(),
            )
            .await
        }
        ("GET", "/api/workspace-file") => {
            let Some(rel) = query_param(&request.query, "path").map(percent_decode) else {
                return write_response(
                    &mut writer,
                    "400 Bad Request",
                    "text/plain",
                    b"missing path",
                )
                .await;
            };
            match read_workspace_file(&state.workspace_root, &rel).await {
                Ok(text) => {
                    write_response(
                        &mut writer,
                        "200 OK",
                        "text/plain; charset=utf-8",
                        text.as_bytes(),
                    )
                    .await
                }
                Err((status, msg)) => {
                    write_response(&mut writer, status, "text/plain", msg.as_bytes()).await
                }
            }
        }
        ("GET", "/api/git-diff") => {
            let Some(rel) = query_param(&request.query, "path").map(percent_decode) else {
                return write_response(
                    &mut writer,
                    "400 Bad Request",
                    "text/plain",
                    b"missing path",
                )
                .await;
            };
            match git_diff(&state.workspace_root, &rel).await {
                Ok(diff) => {
                    write_response(
                        &mut writer,
                        "200 OK",
                        "text/plain; charset=utf-8",
                        diff.as_bytes(),
                    )
                    .await
                }
                Err((status, msg)) => {
                    write_response(&mut writer, status, "text/plain", msg.as_bytes()).await
                }
            }
        }
        ("POST", "/api/key") => {
            #[derive(serde::Deserialize)]
            struct KeyBody {
                provider: String,
                api_key: String,
            }
            let Ok(body) = serde_json::from_slice::<KeyBody>(&request.body) else {
                return write_response(
                    &mut writer,
                    "400 Bad Request",
                    "application/json",
                    br#"{"ok":false,"error":"missing provider/api_key"}"#,
                )
                .await;
            };
            let Some(kind) = ProviderKind::from_cli_name(&body.provider) else {
                return write_response(
                    &mut writer,
                    "400 Bad Request",
                    "application/json",
                    br#"{"ok":false,"error":"unknown provider"}"#,
                )
                .await;
            };
            if body.api_key.trim().is_empty() {
                return write_response(
                    &mut writer,
                    "400 Bad Request",
                    "application/json",
                    br#"{"ok":false,"error":"empty key"}"#,
                )
                .await;
            }
            let env_name = {
                let mut cfg = state.config.write().await;
                cfg.set_provider_api_key(kind, body.api_key.trim());
                cfg.provider.api_key_env_for(kind).to_string()
            };
            // Persist in the 0600 credentials store (same one the TUI login
            // uses); resolve_api_key falls back to it in every workspace.
            match dcode_ai_common::credentials::set(&env_name, body.api_key.trim()) {
                Ok(_) => {
                    write_response(&mut writer, "200 OK", "application/json", br#"{"ok":true}"#)
                        .await
                }
                Err(e) => {
                    let body = serde_json::json!({"ok": false, "error": e});
                    write_response(
                        &mut writer,
                        "500 Internal Server Error",
                        "application/json",
                        body.to_string().as_bytes(),
                    )
                    .await
                }
            }
        }
        ("POST", "/api/settings") => {
            #[derive(serde::Deserialize)]
            struct SettingsBody {
                permission_mode: Option<String>,
                thinking: Option<bool>,
                max_tokens: Option<u32>,
                temperature: Option<f32>,
            }
            let Ok(body) = serde_json::from_slice::<SettingsBody>(&request.body) else {
                return write_response(
                    &mut writer,
                    "400 Bad Request",
                    "application/json",
                    br#"{"ok":false,"error":"invalid body"}"#,
                )
                .await;
            };
            {
                use dcode_ai_common::config::PermissionMode;
                let mut cfg = state.config.write().await;
                if let Some(mode) = body.permission_mode.as_deref() {
                    let parsed = match mode {
                        "default" => PermissionMode::Default,
                        "plan" => PermissionMode::Plan,
                        "accept-edits" | "acceptedits" => PermissionMode::AcceptEdits,
                        "dont-ask" | "dontask" => PermissionMode::DontAsk,
                        "bypass-permissions" | "bypasspermissions" | "bypass" => {
                            PermissionMode::BypassPermissions
                        }
                        _ => {
                            drop(cfg);
                            return write_response(
                                &mut writer,
                                "400 Bad Request",
                                "application/json",
                                br#"{"ok":false,"error":"unknown permission_mode"}"#,
                            )
                            .await;
                        }
                    };
                    cfg.permissions.mode = parsed;
                }
                if let Some(thinking) = body.thinking {
                    cfg.model.enable_thinking = thinking;
                }
                if let Some(mt) = body.max_tokens {
                    cfg.model.max_tokens = mt.clamp(256, 200_000);
                }
                if let Some(temp) = body.temperature {
                    let t = temp.clamp(0.0, 2.0);
                    match cfg.provider.default {
                        ProviderKind::OpenAi | ProviderKind::Antigravity => {
                            cfg.provider.openai.temperature = t
                        }
                        ProviderKind::Anthropic => cfg.provider.anthropic.temperature = t,
                        ProviderKind::OpenRouter => cfg.provider.openrouter.temperature = t,
                        ProviderKind::OpenCodeZen => cfg.provider.opencodezen.temperature = t,
                    }
                }
            }
            let (resp_tx, resp_rx) = oneshot::channel();
            if state
                .session_cmd_tx
                .send(SessionCommand::ApplySettings { response: resp_tx })
                .is_err()
            {
                return write_response(
                    &mut writer,
                    "500 Internal Server Error",
                    "application/json",
                    br#"{"ok":false,"error":"server shutting down"}"#,
                )
                .await;
            }
            match resp_rx.await {
                Ok(Ok(session_id)) => {
                    let body = serde_json::json!({"ok": true, "session_id": session_id});
                    write_response(
                        &mut writer,
                        "200 OK",
                        "application/json",
                        body.to_string().as_bytes(),
                    )
                    .await
                }
                Ok(Err(e)) => {
                    let body = serde_json::json!({"ok": false, "error": e});
                    write_response(
                        &mut writer,
                        "500 Internal Server Error",
                        "application/json",
                        body.to_string().as_bytes(),
                    )
                    .await
                }
                Err(_) => {
                    write_response(
                        &mut writer,
                        "500 Internal Server Error",
                        "application/json",
                        br#"{"ok":false,"error":"settings apply failed"}"#,
                    )
                    .await
                }
            }
        }
        ("POST", "/api/rewind") => {
            #[derive(serde::Deserialize)]
            struct RewindBody {
                index_from_end: usize,
                expected_text: String,
            }
            let Ok(body) = serde_json::from_slice::<RewindBody>(&request.body) else {
                return write_response(
                    &mut writer,
                    "400 Bad Request",
                    "application/json",
                    br#"{"ok":false,"error":"invalid body"}"#,
                )
                .await;
            };
            let (resp_tx, resp_rx) = oneshot::channel();
            if state
                .session_cmd_tx
                .send(SessionCommand::Rewind {
                    index_from_end: body.index_from_end,
                    expected_text: body.expected_text,
                    response: resp_tx,
                })
                .is_err()
            {
                return write_response(
                    &mut writer,
                    "500 Internal Server Error",
                    "application/json",
                    br#"{"ok":false,"error":"server shutting down"}"#,
                )
                .await;
            }
            match resp_rx.await {
                Ok(Ok(session_id)) => {
                    let body = serde_json::json!({"ok": true, "session_id": session_id});
                    write_response(
                        &mut writer,
                        "200 OK",
                        "application/json",
                        body.to_string().as_bytes(),
                    )
                    .await
                }
                Ok(Err(e)) => {
                    let body = serde_json::json!({"ok": false, "error": e});
                    write_response(
                        &mut writer,
                        "409 Conflict",
                        "application/json",
                        body.to_string().as_bytes(),
                    )
                    .await
                }
                Err(_) => {
                    write_response(
                        &mut writer,
                        "500 Internal Server Error",
                        "application/json",
                        br#"{"ok":false,"error":"rewind failed"}"#,
                    )
                    .await
                }
            }
        }
        ("POST", "/api/model") => {
            #[derive(serde::Deserialize)]
            struct ModelBody {
                provider: Option<String>,
                model: Option<String>,
            }
            let Ok(body) = serde_json::from_slice::<ModelBody>(&request.body) else {
                return write_response(
                    &mut writer,
                    "400 Bad Request",
                    "application/json",
                    br#"{"ok":false,"error":"invalid body"}"#,
                )
                .await;
            };
            let (resp_tx, resp_rx) = oneshot::channel();
            if state
                .session_cmd_tx
                .send(SessionCommand::SetModel {
                    provider: body.provider,
                    model: body.model,
                    response: resp_tx,
                })
                .is_err()
            {
                return write_response(
                    &mut writer,
                    "500 Internal Server Error",
                    "application/json",
                    br#"{"ok":false,"error":"server shutting down"}"#,
                )
                .await;
            }
            match resp_rx.await {
                Ok(Ok(session_id)) => {
                    let body = serde_json::json!({"ok": true, "session_id": session_id});
                    write_response(
                        &mut writer,
                        "200 OK",
                        "application/json",
                        body.to_string().as_bytes(),
                    )
                    .await
                }
                Ok(Err(e)) => {
                    let body = serde_json::json!({"ok": false, "error": e});
                    write_response(
                        &mut writer,
                        "500 Internal Server Error",
                        "application/json",
                        body.to_string().as_bytes(),
                    )
                    .await
                }
                Err(_) => {
                    write_response(
                        &mut writer,
                        "500 Internal Server Error",
                        "application/json",
                        br#"{"ok":false,"error":"model switch failed"}"#,
                    )
                    .await
                }
            }
        }
        ("POST", "/api/sessions/rename") => {
            #[derive(serde::Deserialize)]
            struct RenameBody {
                session_id: String,
                name: String,
            }
            let Ok(body) = serde_json::from_slice::<RenameBody>(&request.body) else {
                return write_response(
                    &mut writer,
                    "400 Bad Request",
                    "application/json",
                    br#"{"ok":false,"error":"missing session_id/name"}"#,
                )
                .await;
            };
            let store = SessionStore::new(state.workspace_root.join(".dcode-ai").join("sessions"));
            let result = match store.load(&body.session_id).await {
                Ok(mut session) => {
                    let name = body.name.trim();
                    session.meta.session_name = (!name.is_empty()).then(|| name.to_string());
                    store.save(&session).await.map_err(|e| e.to_string())
                }
                Err(e) => Err(e.to_string()),
            };
            match result {
                Ok(()) => {
                    write_response(&mut writer, "200 OK", "application/json", br#"{"ok":true}"#)
                        .await
                }
                Err(e) => {
                    let body = serde_json::json!({"ok": false, "error": e});
                    write_response(
                        &mut writer,
                        "500 Internal Server Error",
                        "application/json",
                        body.to_string().as_bytes(),
                    )
                    .await
                }
            }
        }
        ("POST", "/api/sessions/delete") => {
            #[derive(serde::Deserialize)]
            struct DeleteBody {
                session_id: String,
            }
            let Ok(body) = serde_json::from_slice::<DeleteBody>(&request.body) else {
                return write_response(
                    &mut writer,
                    "400 Bad Request",
                    "application/json",
                    br#"{"ok":false,"error":"missing session_id"}"#,
                )
                .await;
            };
            let is_current = {
                let guard = state.live.read().await;
                guard
                    .as_ref()
                    .is_some_and(|ls| ls.session_id == body.session_id)
            };
            if is_current {
                return write_response(
                    &mut writer,
                    "409 Conflict",
                    "application/json",
                    br#"{"ok":false,"error":"cannot delete the active session; switch first"}"#,
                )
                .await;
            }
            let store = SessionStore::new(state.workspace_root.join(".dcode-ai").join("sessions"));
            match store.delete(&body.session_id).await {
                Ok(()) => {
                    write_response(&mut writer, "200 OK", "application/json", br#"{"ok":true}"#)
                        .await
                }
                Err(e) => {
                    let body = serde_json::json!({"ok": false, "error": e.to_string()});
                    write_response(
                        &mut writer,
                        "500 Internal Server Error",
                        "application/json",
                        body.to_string().as_bytes(),
                    )
                    .await
                }
            }
        }
        ("POST", "/api/sessions/fork") => {
            #[derive(serde::Deserialize)]
            struct ForkBody {
                session_id: String,
            }
            let Ok(body) = serde_json::from_slice::<ForkBody>(&request.body) else {
                return write_response(
                    &mut writer,
                    "400 Bad Request",
                    "application/json",
                    br#"{"ok":false,"error":"missing session_id"}"#,
                )
                .await;
            };
            let (resp_tx, resp_rx) = oneshot::channel();
            if state
                .session_cmd_tx
                .send(SessionCommand::Fork {
                    source_id: body.session_id,
                    response: resp_tx,
                })
                .is_err()
            {
                return write_response(
                    &mut writer,
                    "500 Internal Server Error",
                    "application/json",
                    br#"{"ok":false,"error":"server shutting down"}"#,
                )
                .await;
            }
            match resp_rx.await {
                Ok(Ok(session_id)) => {
                    let body = serde_json::json!({"ok": true, "session_id": session_id});
                    write_response(
                        &mut writer,
                        "200 OK",
                        "application/json",
                        body.to_string().as_bytes(),
                    )
                    .await
                }
                Ok(Err(e)) => {
                    let body = serde_json::json!({"ok": false, "error": e});
                    write_response(
                        &mut writer,
                        "500 Internal Server Error",
                        "application/json",
                        body.to_string().as_bytes(),
                    )
                    .await
                }
                Err(_) => {
                    write_response(
                        &mut writer,
                        "500 Internal Server Error",
                        "application/json",
                        br#"{"ok":false,"error":"fork failed"}"#,
                    )
                    .await
                }
            }
        }
        ("POST", "/api/sessions/switch") => {
            #[derive(serde::Deserialize)]
            struct SwitchBody {
                session_id: String,
            }
            let Ok(switch_body) = serde_json::from_slice::<SwitchBody>(&request.body) else {
                return write_response(
                    &mut writer,
                    "400 Bad Request",
                    "application/json",
                    br#"{"ok":false,"error":"missing session_id"}"#,
                )
                .await;
            };
            let (resp_tx, resp_rx) = oneshot::channel();
            if state
                .session_cmd_tx
                .send(SessionCommand::Switch {
                    session_id: switch_body.session_id,
                    response: resp_tx,
                })
                .is_err()
            {
                return write_response(
                    &mut writer,
                    "500 Internal Server Error",
                    "application/json",
                    br#"{"ok":false,"error":"server shutting down"}"#,
                )
                .await;
            }
            match resp_rx.await {
                Ok(Ok(new_id)) => {
                    let body = serde_json::json!({"ok": true, "session_id": new_id});
                    write_response(
                        &mut writer,
                        "200 OK",
                        "application/json",
                        body.to_string().as_bytes(),
                    )
                    .await
                }
                Ok(Err(e)) => {
                    let body = serde_json::json!({"ok": false, "error": e});
                    write_response(
                        &mut writer,
                        "500 Internal Server Error",
                        "application/json",
                        body.to_string().as_bytes(),
                    )
                    .await
                }
                Err(_) => {
                    write_response(
                        &mut writer,
                        "500 Internal Server Error",
                        "application/json",
                        br#"{"ok":false,"error":"session switch failed"}"#,
                    )
                    .await
                }
            }
        }
        _ => write_response(&mut writer, "404 Not Found", "text/plain", b"not found").await,
    }
}

async fn write_response(
    writer: &mut tokio::io::WriteHalf<TcpStream>,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    write_response_with_headers(writer, status, content_type, body, &[]).await
}

/// `write_response` plus caller-supplied extra headers (e.g. `Set-Cookie`).
async fn write_response_with_headers(
    writer: &mut tokio::io::WriteHalf<TcpStream>,
    status: &str,
    content_type: &str,
    body: &[u8],
    extra_headers: &[(&str, &str)],
) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in extra_headers {
        head.push_str(name);
        head.push_str(": ");
        head.push_str(value);
        head.push_str("\r\n");
    }
    head.push_str("\r\n");
    writer.write_all(head.as_bytes()).await?;
    writer.write_all(body).await?;
    writer.shutdown().await
}

/// Forward session events as SSE until the browser disconnects.
/// TCP_NODELAY is set on the original stream to prevent Nagle's algorithm
/// from buffering small writes — without it token-by-token SSE is delayed.
async fn stream_events(
    writer: &mut tokio::io::WriteHalf<TcpStream>,
    socket_path: &std::path::Path,
) -> std::io::Result<()> {
    let client = IpcClient::new(socket_path.to_path_buf());
    let mut events = match client.connect().await {
        Ok(rx) => rx,
        Err(err) => {
            let body = format!("session IPC connect failed: {err}");
            return write_response(writer, "502 Bad Gateway", "text/plain", body.as_bytes()).await;
        }
    };

    writer
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: keep-alive\r\n\r\n",
        )
        .await?;
    writer.flush().await?;

    let mut keepalive = tokio::time::interval(std::time::Duration::from_secs(15));
    keepalive.reset();
    loop {
        tokio::select! {
            envelope = events.recv() => {
                let Some(envelope) = envelope else { break };
                let Ok(json) = serde_json::to_string(&envelope) else { continue };
                writer.write_all(b"data: ").await?;
                writer.write_all(json.as_bytes()).await?;
                writer.write_all(b"\n\n").await?;
                writer.flush().await?;
            }
            _ = keepalive.tick() => {
                // SSE comment line; keeps idle connections from timing out.
                writer.write_all(b": ping\n\n").await?;
                writer.flush().await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_decode_handles_encoded_and_plus() {
        assert_eq!(percent_decode("a%20b"), "a b");
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("caf%C3%A9"), "café");
        // Malformed escapes pass through instead of panicking.
        assert_eq!(percent_decode("100%"), "100%");
        assert_eq!(percent_decode("%zz"), "%zz");
    }

    #[test]
    fn cookie_value_finds_named_cookie() {
        let header = "foo=1; dcode_ai_token=abc123; bar=2";
        assert_eq!(cookie_value(header, "dcode_ai_token"), Some("abc123"));
        assert_eq!(cookie_value(header, "foo"), Some("1"));
        assert_eq!(cookie_value(header, "missing"), None);
        assert_eq!(cookie_value("", "dcode_ai_token"), None);
    }

    #[test]
    fn query_param_extracts_values() {
        assert_eq!(query_param("t=abc&x=1", "t"), Some("abc"));
        assert_eq!(query_param("t=abc&x=1", "x"), Some("1"));
        assert_eq!(query_param("t=abc", "missing"), None);
    }

    #[test]
    fn switch_keys_round_trip_through_from_cli_name() {
        // The dropdown sends these back; each must resolve to the SAME kind
        // (the Antigravity/OpenAI shared-config-block trap).
        for kind in ProviderKind::ALL {
            assert_eq!(
                ProviderKind::from_cli_name(switch_key(kind)),
                Some(kind),
                "switch key for {kind:?} must round-trip"
            );
        }
    }

    #[test]
    fn texts_match_accepts_containment_both_ways() {
        assert!(texts_match("hello", "hello"));
        assert!(texts_match("  hello ", "hello"));
        // Stored form may be an expansion of the transcript form and vice versa.
        assert!(texts_match(
            "see @src/main.rs for details",
            "see @src/main.rs"
        ));
        assert!(texts_match(
            "see @src/main.rs",
            "see @src/main.rs for details"
        ));
        assert!(!texts_match("hello", "goodbye"));
    }

    #[test]
    fn media_types_cover_images() {
        assert_eq!(media_type_for("png"), ("image/png", true));
        assert_eq!(media_type_for("JPG"), ("image/jpeg", true));
        assert_eq!(media_type_for("exe"), ("application/octet-stream", false));
        // SVG is previewable but not a native model input.
        assert!(!media_type_for("svg").1);
    }

    #[test]
    fn tree_listing_hides_noise_dirs() {
        for name in [".git", "target", "node_modules", ".dcode-ai"] {
            assert!(is_ignored_entry(name), "{name} must be hidden");
        }
        assert!(!is_ignored_entry("src"));
        assert!(!is_ignored_entry("Cargo.toml"));
    }

    #[test]
    fn fallback_model_lists_are_never_empty() {
        for kind in ProviderKind::ALL {
            assert!(!fallback_models(kind).is_empty());
        }
    }

    #[tokio::test]
    async fn resolve_in_workspace_blocks_escapes() {
        let dir = std::env::temp_dir().join(format!("dcode-webtest-{}", std::process::id()));
        tokio::fs::create_dir_all(dir.join("sub")).await.unwrap();
        tokio::fs::write(dir.join("sub").join("f.txt"), b"x")
            .await
            .unwrap();

        assert!(resolve_in_workspace(&dir, "sub/f.txt").await.is_some());
        assert!(resolve_in_workspace(&dir, "..").await.is_none());
        assert!(
            resolve_in_workspace(&dir, "sub/../../etc/passwd")
                .await
                .is_none()
        );
        assert!(resolve_in_workspace(&dir, "missing.txt").await.is_none());

        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
}
