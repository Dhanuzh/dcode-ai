//! A guest on someone's session (`dcode-ai join <host:port/key>`) gets the
//! same terminal UI as the host: the same state, rendering, dashboard and
//! office. Only the ends differ: events come from the invite connection
//! instead of a local agent, and what the guest types goes to the host's
//! session (`say`, `approve`, `stop`) instead of to a model.

use crate::invite::{Guest, GuestUpdate};
use crate::tui::app::{ApprovalAnswer, TuiCmd, run_blocking};
use crate::tui::state::{DisplayBlock, TuiSessionState};
use std::sync::{Arc, Mutex};

/// What the guest's own slash commands do.
fn guest_command(text: &str, g: &mut TuiSessionState) -> GuestAction {
    let word = text.split_whitespace().next().unwrap_or("");
    match word {
        "/quit" | "/exit" | "/leave" => GuestAction::Leave,
        "/stop" => GuestAction::Stop,
        "/office" => {
            g.office_open = true;
            GuestAction::None
        }
        "/timeline" => {
            g.timeline_open = true;
            GuestAction::None
        }
        "/help" => {
            g.push_block(DisplayBlock::System(
                "[join] you're a guest on this session: type to send a message · /office · /timeline · /stop (if the host allows it) · /quit to leave".into(),
            ));
            GuestAction::None
        }
        _ => {
            g.push_block(DisplayBlock::System(format!(
                "[join] {word} isn't available while joined (it would change the host's session) — /help"
            )));
            GuestAction::None
        }
    }
}

enum GuestAction {
    None,
    Leave,
    Stop,
}

/// Run the TUI for a guest until they leave or the host ends it.
pub async fn run(guest: Guest, theme: Option<&str>) -> anyhow::Result<()> {
    crate::tui::theme::set_by_name(theme);
    let workspace = std::env::current_dir().unwrap_or_default();
    let title = guest.title.clone();
    let host = guest.host.clone();
    let let_approve = guest.let_approve;
    let state = Arc::new(Mutex::new(TuiSessionState::new(
        guest.session_id.clone(),
        guest.model.clone(),
        format!("guest of {host}"),
        "Joined".into(),
        workspace,
        false,
    )));
    if let Ok(mut g) = state.lock() {
        g.mouse_capture_on = true;
        if !host.is_empty() {
            g.office.set_owner(&host);
        }
        g.office.me = Some(guest.me.clone());
        g.push_block(DisplayBlock::System(format!(
            "[join] you joined {title}{} — type to send a message · /office · /quit to leave{}",
            if host.is_empty() {
                String::new()
            } else {
                format!(" (hosted by {host})")
            },
            if let_approve {
                " · you may approve and stop turns"
            } else {
                " · approvals stay with the host"
            }
        )));
    }
    let (send, mut updates) = guest.into_parts();

    // Events → the same state the host's TUI uses.
    let feed_state = state.clone();
    let feed = tokio::spawn(async move {
        while let Some(update) = updates.recv().await {
            let Ok(mut g) = feed_state.lock() else { break };
            match update {
                GuestUpdate::History(events) => {
                    drop(g);
                    crate::tui::replay::apply_history(&events, &feed_state);
                    continue;
                }
                GuestUpdate::Event(env) => g.apply_event(&env.event),
                GuestUpdate::Note(text) => {
                    g.push_block(DisplayBlock::System(format!("[join] {text}")))
                }
            }
        }
        if let Ok(mut g) = feed_state.lock() {
            g.push_block(DisplayBlock::System(
                "[join] disconnected from the host — press Ctrl+C to leave".into(),
            ));
        }
    });

    let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel::<TuiCmd>();
    let (answer_tx, mut answer_rx) = tokio::sync::mpsc::unbounded_channel();
    let (approval_tx, mut approval_rx) = tokio::sync::mpsc::unbounded_channel::<ApprovalAnswer>();
    let ui_state = state.clone();
    let ui = tokio::task::spawn_blocking(move || {
        run_blocking(
            ui_state,
            cmd_tx,
            Some(answer_tx),
            Some(approval_tx),
            false,
            None,
            true,
            3,
        )
    });

    let note = |state: &Arc<Mutex<TuiSessionState>>, text: &str| {
        if let Ok(mut g) = state.lock() {
            g.push_block(DisplayBlock::System(format!("[join] {text}")));
        }
    };
    // Typed while the host's agent is busy: the TUI queues it locally; the
    // host's session queues it too, so send it straight away.
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(200));
    loop {
        tokio::select! {
            _ = tick.tick() => {
                let queued: Vec<String> = match state.lock() {
                    Ok(mut g) => g.followup_queue.drain(..).collect(),
                    Err(_) => Vec::new(),
                };
                let mut leave = false;
                for text in queued {
                    if text.trim_start().starts_with('/') {
                        let action = match state.lock() {
                            Ok(mut g) => guest_command(text.trim(), &mut g),
                            Err(_) => GuestAction::None,
                        };
                        match action {
                            GuestAction::Leave => leave = true,
                            GuestAction::Stop => {
                                send.stop_turn();
                            }
                            GuestAction::None => {}
                        }
                    } else if !text.trim().is_empty() {
                        send.say(text.trim());
                    }
                }
                if leave {
                    break;
                }
            }
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    TuiCmd::Submit(text) => {
                        let text = text.trim().to_string();
                        if text.is_empty() {
                            continue;
                        }
                        if text.starts_with('/') {
                            let action = match state.lock() {
                                Ok(mut g) => guest_command(&text, &mut g),
                                Err(_) => GuestAction::None,
                            };
                            match action {
                                GuestAction::Leave => break,
                                GuestAction::Stop => { send.stop_turn(); }
                                GuestAction::None => {}
                            }
                        } else if !send.say(&text) {
                            note(&state, "not connected any more");
                        }
                    }
                    TuiCmd::CancelTurn => {
                        send.stop_turn();
                    }
                    TuiCmd::Exit => break,
                    TuiCmd::QuestionAnswer(_) => {
                        note(&state, "only the host can answer the agent's questions");
                    }
                    _ => note(&state, "that isn't available while joined"),
                }
            }
            a = approval_rx.recv() => {
                match a {
                    Some(ApprovalAnswer::Verdict { call_id, approved }) => {
                        send.approve(&call_id, approved);
                    }
                    Some(_) => note(&state, "only a plain yes or no works while joined"),
                    None => {}
                }
            }
            q = answer_rx.recv() => {
                if q.is_some() {
                    note(&state, "only the host can answer the agent's questions");
                }
            }
        }
    }
    if let Ok(mut g) = state.lock() {
        g.should_exit = true;
    }
    feed.abort();
    let _ = ui.await;
    eprintln!("left {title}");
    Ok(())
}
