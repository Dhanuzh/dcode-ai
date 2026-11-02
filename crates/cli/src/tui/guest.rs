//! A guest on someone's session (`dcode-ai join <host:port/key>`) gets the
//! same terminal UI as the host: the same state, rendering, dashboard and
//! office. Only the ends differ: events come from the invite connection
//! instead of a local agent, and what the guest types goes to the host's
//! session (`say`, `approve`, `stop`) instead of to a model.

use crate::invite::{Guest, GuestSender, GuestUpdate};
use crate::tui::app::{ApprovalAnswer, TuiCmd, run_blocking};
use crate::tui::office::PersonRole;
use crate::tui::state::{DisplayBlock, TuiSessionState};
use std::sync::{Arc, Mutex};

/// What the guest's own slash commands do.
fn guest_command(text: &str, g: &mut TuiSessionState, send: &GuestSender) -> GuestAction {
    let (word, rest) = text.split_once(' ').unwrap_or((text, ""));
    let rest = rest.trim();
    match word {
        "/quit" | "/exit" | "/leave" => return GuestAction::Leave,
        "/stop" => {
            send.stop_turn();
        }
        "/chat" if !rest.is_empty() => {
            send.chat(rest);
        }
        "/chat" => {
            let lines: Vec<String> = g
                .team
                .chat
                .iter()
                .rev()
                .take(20)
                .rev()
                .map(|c| format!("[chat] {} {}: {}", c.at.format("%H:%M"), c.name, c.text))
                .collect();
            if lines.is_empty() {
                g.push_block(DisplayBlock::System(
                    "[chat] no chat yet — /chat <text> talks to the people here (the agent doesn't see it)".into(),
                ));
            }
            for l in lines {
                g.push_block(DisplayBlock::System(l));
            }
        }
        "/todo" if rest.is_empty() => {
            for l in g.team.lines() {
                g.push_block(DisplayBlock::System(format!("[todo] {l}")));
            }
        }
        "/todo" => {
            if let Some(which) = rest.strip_prefix("done ").or_else(|| rest.strip_prefix("tick ")) {
                match g.team.find(which).map(|t| t.id.clone()) {
                    Some(id) => {
                        send.todo_done(&id);
                    }
                    None => g.push_block(DisplayBlock::System(format!("[todo] no item {which} — /todo lists them"))),
                }
            } else {
                send.todo(rest);
            }
        }
        "/people" => {
            let names: Vec<String> = g
                .office
                .people_ordered()
                .into_iter()
                .filter(|p| p.leaving.is_none())
                .map(|p| {
                    let me = g.office.me.as_deref() == Some(p.id.as_str());
                    let role = match p.role {
                        PersonRole::Owner => "host",
                        PersonRole::Joined if me => "you",
                        PersonRole::Joined => "joined",
                        PersonRole::Watching => "watching",
                    };
                    format!("[people]   {} — {role}", p.name)
                })
                .collect();
            for l in names {
                g.push_block(DisplayBlock::System(l));
            }
        }
        "/call" => {
            if g.office.voice.is_some() {
                g.push_block(DisplayBlock::System(
                    "[call] you're in the call — /mute · /hangup".into(),
                ));
            } else {
                let me = g.office.me.clone().unwrap_or_default();
                match crate::voice::Call::start(&me) {
                    Ok((call, mut mic)) => {
                        g.office.voice = Some(call);
                        send.call_join();
                        let s = send.clone();
                        tokio::spawn(async move {
                            while let Some(packet) = mic.recv().await {
                                if !s.audio(&packet) {
                                    break;
                                }
                            }
                        });
                        g.push_block(DisplayBlock::System(
                            "[call] 📞 you joined the call — /mute · /hangup · headphones avoid echo".into(),
                        ));
                    }
                    Err(e) => g.push_block(DisplayBlock::System(format!("[call] can't start: {e}"))),
                }
            }
        }
        "/mute" => match &g.office.voice {
            Some(v) => {
                let on = !v.muted();
                v.set_muted(on);
                g.push_block(DisplayBlock::System(
                    if on { "[call] 🔇 muted — /mute again to talk" } else { "[call] 🎙 unmuted" }.into(),
                ));
            }
            None => g.push_block(DisplayBlock::System("[call] you're not in a call — /call".into())),
        },
        "/hangup" => match g.office.voice.take() {
            Some(v) => {
                v.hang_up();
                send.call_leave();
                g.push_block(DisplayBlock::System("[call] you left the call".into()));
            }
            None => g.push_block(DisplayBlock::System("[call] you're not in a call".into())),
        },
        "/office" => g.office_open = true,
        "/timeline" => g.timeline_open = true,
        "/help" => g.push_block(DisplayBlock::System(
            "[join] you're a guest on this session: type to send a message to the agent · /chat <text> to the people · /todo <text> adds to the shared to-do list · /todo done <n> · /people · /call · /mute · /hangup · /office · /timeline · /stop (if the host allows it) · /quit to leave".into(),
        )),
        _ => g.push_block(DisplayBlock::System(format!(
            "[join] {word} isn't available while joined (it would change the host's session) — /help"
        ))),
    }
    GuestAction::None
}

enum GuestAction {
    None,
    Leave,
}

/// Run the TUI for a guest until they leave or the host ends it.
pub async fn run(guest: Guest, theme: Option<&str>) -> anyhow::Result<()> {
    crate::tui::theme::set_by_name(theme);
    let title = guest.title.clone();
    let host = guest.host.clone();
    let project = if guest.project.is_empty() {
        title.clone()
    } else {
        guest.project.clone()
    };
    // The host's project, not this folder (whose files aren't the session's).
    let workspace = std::path::PathBuf::from(&project);
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
        g.workspace_display = if host.is_empty() {
            project.clone()
        } else {
            format!("{project} · hosted by {host}")
        };
        if !host.is_empty() {
            g.office.set_owner(&host);
        }
        g.office.me = Some(guest.me.clone());
        let whose = if host.is_empty() {
            project.clone()
        } else {
            format!("{host}'s {project}")
        };
        let topic = if title.is_empty() || title == project {
            String::new()
        } else {
            format!(" (“{title}”)")
        };
        g.push_block(DisplayBlock::System(format!(
            "[join] you joined {whose}{topic} — type to send a message to the agent · /chat <text> to the people · /todo · /help · /quit{}",
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
                GuestUpdate::Audio { from, packet } => {
                    if let Some(v) = &g.office.voice {
                        v.play(&from, &packet);
                    }
                }
                GuestUpdate::Event(env) => g.apply_event(&env.event),
                // Connection blips are passing notices; anything else
                // (removed, invite ended) stays in the transcript.
                GuestUpdate::Note(text)
                    if text.starts_with("connection lost") || text == "reconnected" =>
                {
                    g.show_toast(text, crate::tui::state::ToastKind::Info)
                }
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
                            Ok(mut g) => guest_command(text.trim(), &mut g, &send),
                            Err(_) => GuestAction::None,
                        };
                        if matches!(action, GuestAction::Leave) {
                            leave = true;
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
                                Ok(mut g) => guest_command(&text, &mut g, &send),
                                Err(_) => GuestAction::None,
                            };
                            if matches!(action, GuestAction::Leave) {
                                break;
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
    if let Ok(mut g) = state.lock()
        && let Some(v) = g.office.voice.take()
    {
        v.hang_up();
        send.call_leave();
    }
    send.bye();
    if let Ok(mut g) = state.lock() {
        g.should_exit = true;
    }
    // Let the bye go out.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    feed.abort();
    let _ = ui.await;
    eprintln!("left {title}");
    Ok(())
}
