//! Seeing and sharing a session: `/timeline`, `/files`, `/sounds`,
//! `/speak`, `/watch`, `/handoff`.

use super::*;

/// Who is handing a session over: git's user name, else the login name.
fn handoff_author(workspace: &std::path::Path) -> String {
    dcode_ai_cli::people::my_name(workspace)
}

/// Working-tree changes (paths), for the handoff note.
fn changed_files(workspace: &std::path::Path) -> Vec<String> {
    std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| l.get(3..).map(str::to_string))
                .filter(|p| !p.starts_with(".dcode-ai/"))
                .collect()
        })
        .unwrap_or_default()
}

impl Repl {
    #[allow(unused_variables, unused_mut)]
    pub(super) async fn cmd_team(
        &mut self,
        input: &str,
        command: &str,
        rest: &str,
        mut parts: std::str::SplitWhitespace<'_>,
        out: ReplOutput<'_>,
    ) -> anyhow::Result<bool> {
        match command {
            "/office" => {
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.office_open = true;
                } else {
                    out.println("[office] the office needs the full-screen terminal UI");
                }
            }
            "/timeline" => {
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.timeline_open = true;
                } else {
                    out.println("[timeline] the timeline needs the full-screen terminal UI");
                }
            }
            "/files" => {
                let lines: Vec<String> = match &out {
                    ReplOutput::Tui(st) => st
                        .lock()
                        .map(|g| {
                            let map = &g.file_map;
                            let mut lines = vec![format!(
                                "[files] {} touched · {} edited (dashboard: Ctrl+\\ then Alt+4)",
                                map.files.len(),
                                map.edited()
                            )];
                            for (depth, name, touch) in map.tree() {
                                let indent = "  ".repeat(depth + 1);
                                lines.push(match touch {
                                    None => format!("{indent}{name}"),
                                    Some(t) if t.edits > 0 => format!(
                                        "{indent}✎ {name}  {} edit{} · {}",
                                        t.edits,
                                        if t.edits == 1 { "" } else { "s" },
                                        crate::tui::activity::ago(t.last_at)
                                    ),
                                    Some(t) => format!(
                                        "{indent}· {name}  read {}× · {}",
                                        t.reads,
                                        crate::tui::activity::ago(t.last_at)
                                    ),
                                });
                            }
                            lines
                        })
                        .unwrap_or_default(),
                    _ => vec!["[files] needs the terminal UI".into()],
                };
                for l in lines {
                    out.println(&l);
                }
            }
            "/sounds" => {
                let on = match rest.trim() {
                    "on" => Some(true),
                    "off" => Some(false),
                    _ => None,
                };
                let Some(on) = on else {
                    let now = self.runtime.config().ui.sounds;
                    out.println(&format!(
                        "[sounds] {} — /sounds on|off (a bell when a long turn finishes or dcode-ai needs you)",
                        if now { "on" } else { "off" }
                    ));
                    return Ok(true);
                };
                self.runtime.config_mut().ui.sounds = on;
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.sounds = on;
                }
                let path = dcode_ai_common::config::global_config_file();
                if let Some(path) = path {
                    let _ = dcode_ai_common::config::set_toml_value(
                        &path,
                        "ui",
                        "sounds",
                        toml::Value::Boolean(on),
                    );
                }
                out.println(&format!(
                    "[sounds] {} (saved to your config)",
                    if on { "on" } else { "off" }
                ));
            }
            "/speak" => {
                let last = self
                    .runtime
                    .messages()
                    .iter()
                    .rev()
                    .find(|m| m.role == dcode_ai_common::message::Role::Assistant)
                    .map(|m| m.content.to_summary_text())
                    .unwrap_or_default();
                match crate::tui::notify::speak(&last) {
                    Ok(()) => out.println("[speak] reading the last reply aloud…"),
                    Err(e) => out.eprintln(&format!("[speak] {e}")),
                }
            }
            "/watch" => {
                let arg = rest.trim();
                if arg == "stop" {
                    if self.watch.take().is_some() {
                        out.println("[watch] stopped — the link no longer works");
                    } else {
                        out.println("[watch] no watch link is running");
                    }
                    return Ok(true);
                }
                // `/watch`, `/watch lan`, `/watch lan 8787`, `/watch 8787`.
                let words: Vec<&str> = arg.split_whitespace().collect();
                let lan = words.contains(&"lan");
                let port: u16 = words.iter().find_map(|w| w.parse().ok()).unwrap_or(0);
                if let Some(w) = &self.watch
                    && w.lan == lan
                    && (port == 0 || w.port == port)
                {
                    out.println(&format!("[watch] already live: {}", w.url));
                    return Ok(true);
                }
                // A new one replaces the old.
                self.watch = None;
                let title = self
                    .runtime
                    .session_name()
                    .map(str::to_string)
                    .unwrap_or_else(|| self.runtime.session_id().to_string());
                match crate::watch::start_on(
                    self.runtime.event_log_path(),
                    title,
                    lan,
                    port,
                    self.runtime.event_tx(),
                )
                .await
                {
                    Ok(w) => {
                        let _ = crate::tui::clipboard::copy_to_clipboard(&w.url);
                        out.println(&format!("[watch] live, read-only: {}", w.url));
                        if lan {
                            out.println(
                                "[watch] reachable by anyone on this network who has the link — /watch stop ends it",
                            );
                            print_qr(&out, "[watch] or scan it with a phone:", &w.url);
                            if let Some(help) = crate::watch::lan_help(w.port) {
                                for line in help.lines() {
                                    out.println(&format!("[watch] {line}"));
                                }
                                if port == 0 {
                                    out.println(
                                        "[watch] tip: /watch lan 8787 keeps the same port, so the rule is made once",
                                    );
                                }
                            }
                        } else {
                            out.println(
                                "[watch] this machine only (copied) · /watch lan to share on your network · /watch stop",
                            );
                        }
                        self.watch = Some(w);
                    }
                    Err(e) => out.eprintln(&format!("[watch] could not start: {e}")),
                }
            }
            "/invite" => {
                let words: Vec<&str> = rest.split_whitespace().collect();
                if words.first() == Some(&"stop") {
                    if let Some(inv) = &self.invite {
                        inv.call_leave();
                    }
                    if let ReplOutput::Tui(st) = &out
                        && let Ok(mut g) = st.lock()
                    {
                        g.office.voice = None;
                    }
                    if self.invite.take().is_some() {
                        out.println("[invite] stopped — the line no longer works and guests were disconnected");
                    } else {
                        out.println("[invite] no invite is open");
                    }
                    return Ok(true);
                }
                let ReplOutput::Tui(st) = &out else {
                    out.eprintln(
                        "[invite] needs the terminal UI (it asks you before letting anyone in)",
                    );
                    return Ok(true);
                };
                let Some(socket) = self.runtime.socket_path() else {
                    out.eprintln(
                        "[invite] this session has no IPC endpoint, so nobody can join it",
                    );
                    return Ok(true);
                };
                let web = words.contains(&"web");
                let let_approve = words.contains(&"--let-approve");
                let port: u16 = words.iter().find_map(|w| w.parse().ok()).unwrap_or(0);
                // A new invite replaces the old one (and its key).
                self.invite = None;
                let title = self
                    .runtime
                    .session_name()
                    .map(str::to_string)
                    .unwrap_or_else(|| self.runtime.session_id().to_string());
                let knocks = spawn_knock_prompts((*st).clone());
                let opts = dcode_ai_cli::invite::HostOptions {
                    port,
                    web,
                    let_approve,
                    socket,
                    log_path: self.runtime.event_log_path(),
                    title,
                    knocks,
                    presence: self.runtime.event_tx(),
                    host_name: dcode_ai_cli::people::my_name(self.runtime.workspace_root()),
                    project: self
                        .runtime
                        .workspace_root()
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    session_id: self.runtime.session_id().to_string(),
                    model: self.runtime.model().to_string(),
                };
                match dcode_ai_cli::invite::host(opts).await {
                    Ok(inv) => {
                        let line = inv.command();
                        let _ = crate::tui::clipboard::copy_to_clipboard(&line);
                        // The browser link (and its QR code) first, so the
                        // line to send ends up at the bottom.
                        if let Some(url) = &inv.web_url {
                            print_qr(&out, "[invite] or scan it with a phone:", url);
                            out.println(&format!(
                                "[invite] in a browser (no install; plain HTTP, so people on this network could read it): {url}"
                            ));
                        }
                        out.println(&format!(
                            "[invite] open on port {} — send this line to whoever should join (copied):",
                            inv.port()
                        ));
                        out.println(&format!("[invite]   {line}"));
                        out.println(if let_approve {
                            "[invite] you're asked before anyone gets in · guests can send messages, approve and stop · /invite stop ends it"
                        } else {
                            "[invite] you're asked before anyone gets in · guests can send messages (approvals stay with you) · /invite stop ends it"
                        });
                        if let Some(help) = crate::watch::lan_help(inv.port()) {
                            for line in help.lines() {
                                out.println(&format!("[invite] {line}"));
                            }
                        }
                        out.println("[invite] outside this network: use a VPN such as Tailscale and its address instead");
                        self.invite = Some(inv);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => out.eprintln(&format!(
                        "[invite] port {} is in use — try /invite <another port>",
                        if port == 0 {
                            dcode_ai_cli::invite::DEFAULT_PORT
                        } else {
                            port
                        }
                    )),
                    Err(e) => out.eprintln(&format!("[invite] could not start: {e}")),
                }
            }
            "/call" => {
                let ReplOutput::Tui(st) = &out else {
                    out.eprintln("[call] calls need the terminal UI");
                    return Ok(true);
                };
                let Some(inv) = &self.invite else {
                    out.println("[call] start /invite first — calls are with the people who join through it");
                    return Ok(true);
                };
                let already = st.lock().map(|g| g.office.voice.is_some()).unwrap_or(false);
                if already {
                    out.println("[call] you're in the call — /mute · /hangup");
                    return Ok(true);
                }
                match dcode_ai_cli::voice::Call::start(dcode_ai_cli::invite::HOST_PERSON) {
                    Ok((call, mic)) => {
                        inv.call_join(call.clone(), mic);
                        if let Ok(mut g) = st.lock() {
                            g.office.voice = Some(call);
                        }
                        out.println("[call] 📞 you started the call — guests type /call to join · /mute · /hangup · headphones avoid echo");
                    }
                    Err(e) => out.eprintln(&format!("[call] can't start: {e}")),
                }
            }
            "/mute" => {
                let ReplOutput::Tui(st) = &out else {
                    return Ok(true);
                };
                let state = st.lock().ok().and_then(|g| g.office.voice.clone());
                match state {
                    Some(v) => {
                        let on = !v.muted();
                        v.set_muted(on);
                        out.println(if on {
                            "[call] 🔇 muted — /mute again to talk"
                        } else {
                            "[call] 🎙 unmuted"
                        });
                    }
                    None => out.println("[call] you're not in a call — /call"),
                }
            }
            "/hangup" => {
                let ReplOutput::Tui(st) = &out else {
                    return Ok(true);
                };
                let had = st
                    .lock()
                    .ok()
                    .and_then(|mut g| g.office.voice.take())
                    .is_some();
                if let Some(inv) = &self.invite {
                    inv.call_leave();
                }
                out.println(if had {
                    "[call] you left the call"
                } else {
                    "[call] you're not in a call"
                });
            }
            "/chat" => {
                let text = rest.trim();
                if text.is_empty() {
                    let lines: Vec<String> = match &out {
                        ReplOutput::Tui(st) => st
                            .lock()
                            .map(|g| {
                                g.team
                                    .chat
                                    .iter()
                                    .rev()
                                    .take(20)
                                    .rev()
                                    .map(|c| {
                                        format!("{} {}: {}", c.at.format("%H:%M"), c.name, c.text)
                                    })
                                    .collect()
                            })
                            .unwrap_or_default(),
                        _ => Vec::new(),
                    };
                    if lines.is_empty() {
                        out.println("[chat] no chat yet — /chat <text> talks to the people here (the agent doesn't see it) · dashboard Alt+6");
                    }
                    for l in lines {
                        out.println(&format!("[chat] {l}"));
                    }
                    return Ok(true);
                }
                let name = dcode_ai_cli::people::my_name(self.runtime.workspace_root());
                if let Some(tx) = self.runtime.event_tx() {
                    let _ = tx
                        .send(dcode_ai_common::event::AgentEvent::PersonChat {
                            person: "owner".into(),
                            name,
                            text: text.chars().take(2000).collect(),
                        })
                        .await;
                }
            }
            "/todo" => {
                let arg = rest.trim();
                let name = dcode_ai_cli::people::my_name(self.runtime.workspace_root());
                let ReplOutput::Tui(st) = &out else {
                    out.eprintln("[todo] the shared to-do list needs the terminal UI");
                    return Ok(true);
                };
                if arg.is_empty() {
                    let lines = st.lock().map(|g| g.team.lines()).unwrap_or_default();
                    for l in lines {
                        out.println(&format!("[todo] {l}"));
                    }
                    return Ok(true);
                }
                let event = if let Some(which) = arg
                    .strip_prefix("done ")
                    .or_else(|| arg.strip_prefix("tick "))
                {
                    let id = st
                        .lock()
                        .ok()
                        .and_then(|g| g.team.find(which).map(|t| t.id.clone()));
                    match id {
                        Some(id) => {
                            dcode_ai_common::event::AgentEvent::TeamTodoDone { id, by: name }
                        }
                        None => {
                            out.eprintln(&format!("[todo] no item {which} — /todo lists them"));
                            return Ok(true);
                        }
                    }
                } else {
                    dcode_ai_common::event::AgentEvent::TeamTodoAdded {
                        id: dcode_ai_runtime::supervisor::new_todo_id(),
                        text: arg.chars().take(500).collect(),
                        by: name,
                    }
                };
                if let Some(tx) = self.runtime.event_tx() {
                    let _ = tx.send(event).await;
                }
            }
            "/people" => {
                use crate::tui::office::PersonRole;
                let people: Vec<(String, PersonRole)> = match &out {
                    ReplOutput::Tui(st) => st
                        .lock()
                        .map(|g| {
                            g.office
                                .people_ordered()
                                .into_iter()
                                .filter(|p| p.leaving.is_none())
                                .map(|p| (p.name.clone(), p.role))
                                .collect()
                        })
                        .unwrap_or_default(),
                    _ => Vec::new(),
                };
                let guests = self.invite.as_ref().map(|i| i.guests()).unwrap_or_default();
                out.println(&format!("[people] {} here", people.len().max(1)));
                for (name, role) in people {
                    let what = match role {
                        PersonRole::Owner => "you · host".to_string(),
                        PersonRole::Watching => "watching (/watch page)".to_string(),
                        PersonRole::Joined => match guests.iter().find(|g| g.name == name) {
                            Some(g) => {
                                format!("joined over /invite from {} · /kick {name}", g.from)
                            }
                            None => "joined (dcode-ai join)".to_string(),
                        },
                    };
                    out.println(&format!("[people]   {name} — {what}"));
                }
            }
            "/kick" => {
                let name = rest.trim();
                if name.is_empty() {
                    out.eprintln("usage: /kick <name> (see /people)");
                    return Ok(true);
                }
                let n = self.invite.as_ref().map(|i| i.kick(name)).unwrap_or(0);
                if n == 0 {
                    out.eprintln(&format!(
                        "[kick] no invited guest named {name} (/people lists who is here; local joins leave with /quit, /watch stop ends the page)"
                    ));
                } else {
                    out.println(&format!(
                        "[kick] removed {name} — their invite no longer works for them"
                    ));
                }
            }
            "/handoff" => {
                let _ = self.runtime.save().await;
                let ws = self.runtime.workspace_root().to_path_buf();
                let todo = match &out {
                    ReplOutput::Tui(st) => st.lock().ok().and_then(|g| g.current_plan.clone()),
                    _ => None,
                };
                let handoff = crate::session_share_cli::Handoff {
                    from: handoff_author(&ws),
                    at: chrono::Utc::now(),
                    note: rest.trim().to_string(),
                    todo,
                    changed_files: changed_files(&ws),
                };
                let id = self.runtime.session_id().to_string();
                let sessions = ws.join(&self.runtime.config().session.history_dir);
                let file = ws.join(format!("{id}.dcode-session"));
                match crate::session_share_cli::export_with(
                    &sessions,
                    &id,
                    Some(file.clone()),
                    Some(handoff),
                )
                .await
                {
                    Ok(path) => {
                        out.println(&format!(
                            "[handoff] wrote {} (secrets masked) — your teammate runs `dcode-ai session import {}` and says \"continue\"",
                            path.display(),
                            path.file_name().and_then(|n| n.to_str()).unwrap_or_default()
                        ));
                        if rest.trim().is_empty() {
                            out.println(
                                "[handoff] tip: /handoff <note> adds what's done and what's next",
                            );
                        }
                    }
                    Err(e) => out.eprintln(&format!("[handoff] {e}")),
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}

/// Asks you in the TUI about each knock on `/invite` ("Priya wants to
/// join — let them in?"), one at a time, when no other prompt is open.
/// Unanswered after a minute: no.
fn spawn_knock_prompts(
    st: Arc<Mutex<TuiSessionState>>,
) -> tokio::sync::mpsc::Sender<dcode_ai_cli::invite::Knock> {
    use dcode_ai_cli::invite::{KNOCK_QUESTION, expire_knock, register_knock};
    let (tx, mut rx) = tokio::sync::mpsc::channel::<dcode_ai_cli::invite::Knock>(8);
    tokio::spawn(async move {
        let mut n = 0u64;
        while let Some(knock) = rx.recv().await {
            n += 1;
            let qid = format!("{KNOCK_QUESTION}{n}");
            let mut shown = false;
            for _ in 0..300 {
                if let Ok(mut g) = st.lock()
                    && g.active_question.is_none()
                    && g.active_approval.is_none()
                {
                    g.active_question = Some(InteractiveQuestionPayload {
                        question_id: qid.clone(),
                        call_id: qid.clone(),
                        prompt: format!(
                            "{} ({}) wants to join this session — let them in?",
                            knock.name, knock.from
                        ),
                        options: vec![
                            QuestionOption {
                                id: "allow".into(),
                                label: format!("Let {} in", knock.name),
                                description: Some(
                                    "they see the session live and can send messages".into(),
                                ),
                            },
                            QuestionOption {
                                id: "deny".into(),
                                label: "No".into(),
                                description: None,
                            },
                        ],
                        allow_custom: false,
                        suggested_answer: "deny".into(),
                        header: Some("Invite".into()),
                        ..Default::default()
                    });
                    g.open_question_modal();
                    shown = true;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            if !shown {
                let _ = knock.reply.send(false);
                continue;
            }
            register_knock(&qid, knock.reply);
            let st = st.clone();
            let name = knock.name.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                if expire_knock(&qid)
                    && let Ok(mut g) = st.lock()
                {
                    if g.active_question.as_ref().map(|q| q.question_id.as_str())
                        == Some(qid.as_str())
                    {
                        g.active_question = None;
                        g.close_question_modal();
                    }
                    g.push_block(crate::tui::state::DisplayBlock::System(format!(
                        "[invite] {name} wasn't let in (no answer)"
                    )));
                }
            });
        }
    });
    tx
}

/// A link's QR code as one block (so its rows stay together and aligned).
fn print_qr(out: &ReplOutput<'_>, label: &str, url: &str) {
    let qr = dcode_ai_cli::people::qr_lines(url);
    if qr.is_empty() {
        return;
    }
    match out {
        ReplOutput::Tui(st) => {
            if let Ok(mut g) = st.lock() {
                g.push_block(crate::tui::state::DisplayBlock::System(format!(
                    "{label}\n{}",
                    qr.join("\n")
                )));
            }
        }
        _ => {
            out.println(label);
            for l in qr {
                out.println(&l);
            }
        }
    }
}
