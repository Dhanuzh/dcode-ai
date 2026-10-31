//! Event fanout: session log, IPC, and TUI state (no stdout streaming).

use crate::ipc_pending::{ApprovalPendingMap, QuestionPendingMap};
use crate::tui::state::TuiSessionState;
use dcode_ai_common::event::{AgentEvent, EventEnvelope};
use dcode_ai_runtime::ipc::IpcHandle;
use dcode_ai_runtime::supervisor;
use std::sync::{Arc, Mutex};
use tokio::{fs::OpenOptions, io::AsyncWriteExt};

struct IpcFanout {
    tx: tokio::sync::broadcast::Sender<String>,
}

/// Disk + IPC + TUI state; starts IPC command consumer when needed.
pub fn spawn_tui_bridge(
    mut rx: tokio::sync::mpsc::Receiver<AgentEvent>,
    log_path: std::path::PathBuf,
    ipc_handle: Option<IpcHandle>,
    approval_pending: Option<ApprovalPendingMap>,
    question_pending: Option<QuestionPendingMap>,
    state: Arc<Mutex<TuiSessionState>>,
) -> tokio::task::JoinHandle<()> {
    let (event_tx_ipc, command_rx) = match ipc_handle {
        Some(h) => {
            let (etx, crx) = h.into_parts();
            (Some(etx), Some(crx))
        }
        None => (None, None),
    };

    if let Some(crx) = command_rx {
        supervisor::spawn_command_consumer(crx, approval_pending, question_pending, None);
    }

    let ipc = event_tx_ipc.map(|tx| IpcFanout { tx });

    tokio::spawn(async move {
        let mut log_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .await
            .ok();

        // The log follows the session: `/new`, `/fork` and the like change
        // the session id on the same event stream, and each session's
        // events belong in its own `<id>.events.jsonl` (resume replays it).
        let sessions_dir = log_path.parent().map(std::path::Path::to_path_buf);
        let mut logging_for = log_path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".events.jsonl"))
            .map(str::to_string);
        let mut event_id: u64 = 0;
        while let Some(event) = rx.recv().await {
            // A session's start goes in its own log: switch before writing
            // it (the TUI state only learns the new id when it applies it).
            let current = match &event {
                AgentEvent::SessionStarted { session_id, .. } => Some(session_id.clone()),
                _ => state.lock().ok().map(|g| g.session_id.clone()),
            };
            // (Re)open when the session changed, or when the first open
            // failed because the sessions folder did not exist yet.
            if let (Some(dir), Some(id)) = (&sessions_dir, current.filter(|i| !i.is_empty()))
                && (logging_for.as_deref() != Some(id.as_str()) || log_file.is_none())
            {
                let _ = tokio::fs::create_dir_all(dir).await;
                log_file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join(format!("{id}.events.jsonl")))
                    .await
                    .ok();
                logging_for = Some(id);
            }
            event_id += 1;
            let envelope = EventEnvelope::new(event_id, event.clone());

            if let Some(ref fan) = ipc {
                let line = serde_json::to_string(&envelope).unwrap_or_default();
                let _ = fan.tx.send(line);
            }

            if let Some(file) = log_file.as_mut()
                && let Ok(line) = serde_json::to_string(&envelope)
            {
                let _ = file.write_all(line.as_bytes()).await;
                let _ = file.write_all(b"\n").await;
            }

            if let Ok(mut g) = state.lock() {
                g.apply_event(&event);
            }
        }
    })
}
