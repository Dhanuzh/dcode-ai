use super::*;

pub(super) async fn spawn_run(
    workspace_root: &Path,
    prompt: &str,
    model: Option<String>,
    safe: bool,
    permission_mode: CliPermissionMode,
    json: bool,
) -> anyhow::Result<()> {
    let session_id = format!("session-{}", chrono::Utc::now().timestamp_millis());
    let sessions_dir = workspace_root.join(".dcode-ai/sessions");
    std::fs::create_dir_all(&sessions_dir)?;
    let spawn_log = sessions_dir.join(format!("{session_id}.spawn.log"));
    let stdout = std::fs::File::create(&spawn_log)?;
    let stderr = stdout.try_clone()?;
    let exe = std::env::current_exe()?;

    let mut command = std::process::Command::new(exe);
    command
        .arg("run")
        .arg("--prompt")
        .arg(prompt)
        .arg("--stream")
        .arg("ndjson")
        .arg("--session-id")
        .arg(&session_id)
        .arg("--permission-mode")
        .arg(permission_mode.as_arg())
        .args(if safe { vec!["--safe"] } else { vec![] });

    if let Some(model) = model {
        command.arg("--model").arg(model);
    }

    let child = command.stdout(stdout).stderr(stderr).spawn()?;

    if json {
        print_json(
            &SpawnCommandOutput {
                session_id: session_id.clone(),
                pid: child.id(),
                status_path: sessions_dir.join(format!("{session_id}.json")),
                event_log_path: sessions_dir.join(format!("{session_id}.events.jsonl")),
                spawn_log_path: spawn_log,
                socket_path: dcode_ai_runtime::ipc::IpcServer::new(&session_id).socket_path(),
                permission_mode: permission_mode.as_arg().to_string(),
                safe_mode: safe,
            },
            false,
        )?;
    } else {
        println!("{session_id}");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn list_sessions(
    config: &DcodeAiConfig,
    workspace_root: &Path,
    json: bool,
    status_filter: Option<SessionStatusFilter>,
    since_hours: Option<u32>,
    search: Option<String>,
    limit: usize,
    include_children: bool,
) -> anyhow::Result<()> {
    use dcode_ai_common::session::SessionStatus;

    let store = dcode_ai_runtime::session_store::SessionStore::new(
        workspace_root.join(&config.session.history_dir),
    );
    let ids = store.list().await.map_err(anyhow::Error::msg)?;
    let (mut sessions, mut unreadable) = store.load_snapshots(ids).await;

    // Sub-agent sessions appear as `(+N sub-agents)` on their parent.
    if !include_children {
        sessions.retain(|s| s.parent_session_id.is_none());
    }

    // Apply status filter
    if let Some(status) = status_filter {
        sessions.retain(|s| match status {
            SessionStatusFilter::Running => matches!(s.status, SessionStatus::Running),
            SessionStatusFilter::Completed => matches!(s.status, SessionStatus::Completed),
            SessionStatusFilter::Cancelled => matches!(s.status, SessionStatus::Cancelled),
            SessionStatusFilter::Failed => matches!(s.status, SessionStatus::Error),
        });
    }

    // Apply time filter
    if let Some(hours) = since_hours {
        let cutoff = chrono::Utc::now() - chrono::Duration::hours(hours as i64);
        sessions.retain(|s| s.updated_at > cutoff);
    }

    // Apply search filter
    if let Some(pattern) = search {
        let pattern_lower = pattern.to_lowercase();
        sessions.retain(|s| {
            s.id.to_lowercase().contains(&pattern_lower)
                || s.session_name
                    .as_ref()
                    .map(|name| name.to_lowercase().contains(&pattern_lower))
                    .unwrap_or(false)
                || s.session_summary
                    .as_ref()
                    .map(|sum| sum.to_lowercase().contains(&pattern_lower))
                    .unwrap_or(false)
                || s.model.to_lowercase().contains(&pattern_lower)
        });
    }

    sessions.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.id.cmp(&right.id))
    });

    // Apply limit
    sessions.truncate(limit);
    unreadable.sort();

    if json {
        print_json(
            &SessionListOutput {
                sessions,
                unreadable,
            },
            false,
        )?;
    } else {
        for session in sessions {
            print_human_session(&session);
        }
        for id in unreadable {
            println!("{id}\tUnreadable");
        }
    }
    Ok(())
}

/// Delete sessions untouched for `days` days; returns how many.
pub(super) async fn prune_sessions(
    config: &DcodeAiConfig,
    workspace_root: &Path,
    days: u32,
) -> anyhow::Result<usize> {
    let store = dcode_ai_runtime::session_store::SessionStore::new(
        workspace_root.join(&config.session.history_dir),
    );
    let cutoff = chrono::Utc::now() - chrono::Duration::days(i64::from(days));
    Ok(store
        .prune_older_than(cutoff)
        .await
        .map_err(anyhow::Error::msg)?
        .len())
}

pub(super) async fn latest_session_id(
    config: &DcodeAiConfig,
    workspace_root: &Path,
) -> anyhow::Result<String> {
    let store = dcode_ai_runtime::session_store::SessionStore::new(
        workspace_root.join(&config.session.history_dir),
    );
    let ids = store.list().await.map_err(anyhow::Error::msg)?;
    let mut latest = None;

    // Same view as `dcode-ai sessions`: snapshots (cheap, and readable
    // while a live session is writing), top-level sessions only.
    for session in store.load_snapshots(ids).await.0 {
        if session.parent_session_id.is_some() {
            continue;
        }
        let should_replace = latest
            .as_ref()
            .map(|(_, updated_at)| session.updated_at > *updated_at)
            .unwrap_or(true);
        if should_replace {
            latest = Some((session.id, session.updated_at));
        }
    }

    latest
        .map(|(id, _)| id)
        .ok_or_else(|| anyhow::anyhow!("no saved sessions found to resume"))
}

/// Full session id for `query`: an exact id, or a unique suffix of one
/// (`56044093` for `session-1791887456044093-0`).
pub(super) async fn resolve_session_id(
    config: &DcodeAiConfig,
    workspace_root: &Path,
    query: &str,
) -> anyhow::Result<String> {
    let store = dcode_ai_runtime::session_store::SessionStore::new(
        workspace_root.join(&config.session.history_dir),
    );
    let ids = store.list().await.map_err(anyhow::Error::msg)?;
    pick_session_id(&ids, query)
}

pub(super) fn pick_session_id(ids: &[String], query: &str) -> anyhow::Result<String> {
    let q = query.trim();
    if let Some(id) = ids.iter().find(|id| *id == q) {
        return Ok(id.clone());
    }
    let matches: Vec<&String> = ids.iter().filter(|id| id.contains(q)).collect();
    match matches.as_slice() {
        [one] => Ok((*one).clone()),
        [] => anyhow::bail!("no session matches `{q}` (see `dcode-ai sessions`)"),
        many => anyhow::bail!(
            "`{q}` matches {} sessions; use more digits: {}",
            many.len(),
            many.iter()
                .take(5)
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

pub(super) async fn resume_session(
    config: DcodeAiConfig,
    workspace_root: &Path,
    session_id: &str,
    prompt: Option<String>,
    safe: bool,
    stream: StreamMode,
    no_tui: bool,
) -> anyhow::Result<()> {
    let use_tui = !no_tui
        && stdout().is_terminal()
        && stdin().is_terminal()
        && matches!(stream, StreamMode::Human);
    let approval_handler: Option<std::sync::Arc<dyn dcode_ai_core::approval::ApprovalHandler>> =
        if use_tui {
            None
        } else {
            Some(InteractiveIpcApprovalHandler::new())
        };
    let mut runtime = build_resumed_session_runtime(
        config,
        workspace_root,
        safe,
        true,
        session_id,
        approval_handler,
    )
    .await
    .map_err(anyhow::Error::msg)?;
    if let Some(prompt) = prompt {
        if let Some(rx) = runtime.take_event_rx() {
            let ipc_handle = runtime.take_ipc_handle();
            let approval_pending = runtime.take_ipc_approval_pending();
            let _stream_task = spawn_stream_task(
                rx,
                stream,
                runtime.event_log_path(),
                ipc_handle,
                approval_pending,
                runtime.question_pending(),
                None,
            );
        }
        let output = runtime
            .run_turn(&prompt)
            .await
            .map_err(anyhow::Error::msg)?;
        println!("{output}");
        return Ok(());
    }

    if !use_tui && let Some(rx) = runtime.take_event_rx() {
        let ipc_handle = runtime.take_ipc_handle();
        let approval_pending = runtime.take_ipc_approval_pending();
        let _stream_task = spawn_stream_task(
            rx,
            stream,
            runtime.event_log_path(),
            ipc_handle,
            approval_pending,
            runtime.question_pending(),
            None,
        );
    }
    let mut repl = Repl::new(runtime, safe, true);
    if use_tui {
        repl.run_with_tui().await?;
    } else {
        repl.run().await?;
    }
    Ok(())
}

pub(super) async fn show_logs(
    config: &DcodeAiConfig,
    workspace_root: &Path,
    session_id: &str,
    follow: bool,
    json: bool,
) -> anyhow::Result<()> {
    if follow {
        return attach_session(config, workspace_root, session_id, json).await;
    }
    print_log_file(config, workspace_root, session_id, json).await
}

/// `dcode-ai join <id>`: follow a running session live and talk to it.
/// Lines you type are sent as messages (queued until the agent is free);
/// `/approve` and `/deny` answer the approval it is waiting on; `/stop`
/// interrupts the turn; `/quit` leaves (the session keeps running).
pub(super) async fn join_session(
    config: &DcodeAiConfig,
    workspace_root: &Path,
    session_id: &str,
    name: &str,
) -> anyhow::Result<()> {
    use dcode_ai_common::event::{AgentCommand, AgentEvent};
    use tokio::io::AsyncBufReadExt;
    let store = dcode_ai_runtime::session_store::SessionStore::new(
        workspace_root.join(&config.session.history_dir),
    );
    let session = load_session_for_ipc_command(&store, session_id, false).await?;
    let Some(socket) = session.meta.socket_path.clone() else {
        anyhow::bail!("session {session_id} is not running (nothing to join)");
    };
    let client = dcode_ai_runtime::ipc::IpcClient::new(socket.clone());
    let mut events = client
        .connect()
        .await
        .map_err(|e| anyhow::anyhow!("session {session_id} is not running: {e}"))?;
    eprintln!(
        "joined {session_id} as {name} — type to send a message · /approve · /deny · /stop · /quit"
    );
    // Say hello, then keep showing up in the office until we leave.
    let person = dcode_ai_cli::people::new_person_id("join");
    let _ = client
        .send_command(&AgentCommand::Hello {
            person: person.clone(),
            name: name.to_string(),
        })
        .await;
    let beat = {
        let client = dcode_ai_runtime::ipc::IpcClient::new(socket.clone());
        let person = person.clone();
        tokio::spawn(async move {
            let mut every = tokio::time::interval(std::time::Duration::from_secs(10));
            every.tick().await;
            loop {
                every.tick().await;
                let cmd = AgentCommand::Heartbeat {
                    person: person.clone(),
                };
                if client.send_command(&cmd).await.is_err() {
                    break;
                }
            }
        })
    };
    let pending: std::sync::Arc<std::sync::Mutex<Option<String>>> = Default::default();
    let seen = pending.clone();
    let printer = tokio::spawn(async move {
        while let Some(envelope) = events.recv().await {
            match &envelope.event {
                AgentEvent::ApprovalRequested { call_id, .. } => {
                    if let Ok(mut p) = seen.lock() {
                        *p = Some(call_id.clone());
                    }
                }
                AgentEvent::ApprovalResolved { .. } => {
                    if let Ok(mut p) = seen.lock() {
                        *p = None;
                    }
                }
                _ => {}
            }
            let _ = print_event_envelope(&envelope, false);
        }
        eprintln!("[join] the session ended");
    });
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        let call = || pending.lock().ok().and_then(|mut p| p.take());
        let cmd = match text {
            "/quit" | "/exit" => break,
            "/stop" => AgentCommand::Cancel,
            "/approve" | "/deny" => match call() {
                Some(call_id) if text == "/approve" => AgentCommand::ApproveToolCall { call_id },
                Some(call_id) => AgentCommand::DenyToolCall { call_id },
                None => {
                    eprintln!("[join] nothing is waiting for approval");
                    continue;
                }
            },
            _ => AgentCommand::Say {
                person: person.clone(),
                content: text.to_string(),
                name: Some(name.to_string()),
            },
        };
        if let Err(e) = client.send_command(&cmd).await {
            eprintln!("[join] could not reach the session: {e}");
            break;
        }
    }
    beat.abort();
    let _ = client.send_command(&AgentCommand::Bye { person }).await;
    printer.abort();
    Ok(())
}

/// `dcode-ai join <host:port/key>`: join someone's session over the
/// network with their `/invite` line. Same keys as a local join.
pub(super) async fn join_invite(
    invite: &dcode_ai_cli::invite::Invite,
    name: &str,
    tui: Option<Option<&str>>,
) -> anyhow::Result<()> {
    use dcode_ai_cli::invite::{Guest, GuestUpdate};
    use dcode_ai_common::event::AgentEvent;
    use tokio::io::AsyncBufReadExt;
    let mut guest = Guest::connect(invite, name, || {
        eprintln!(
            "[join] knocking on {}:{} as {name} — waiting for the host to let you in…",
            invite.host, invite.port
        )
    })
    .await?;
    // The same terminal UI as the host's, unless asked for plain text.
    if let Some(theme) = tui {
        return dcode_ai_cli::tui::guest::run(guest, theme).await;
    }
    let can_drive = guest.can_drive;
    let keys = if !can_drive {
        "you're watching — what you type goes to the people here (team chat), not the agent · /quit"
    } else if guest.let_approve {
        "type to send a message · /chat · /todo · /approve · /deny · /stop · /quit"
    } else {
        "type to send a message · /chat <text> · /todo <text> · /quit"
    };
    let send = guest.sender();
    eprintln!("[join] you're in: {} — {keys}", guest.title);
    let mut pending: Option<String> = None;
    let mut lines = tokio::io::BufReader::new(tokio::io::stdin()).lines();
    loop {
        tokio::select! {
            update = guest.updates.recv() => match update {
                Some(GuestUpdate::Event(env)) => {
                    match &env.event {
                        AgentEvent::ApprovalRequested { call_id, .. } => pending = Some(call_id.clone()),
                        AgentEvent::ApprovalResolved { .. } => pending = None,
                        _ => {}
                    }
                    let _ = print_event_envelope(&env, false);
                }
                Some(GuestUpdate::Note(text)) => eprintln!("[join] {text}"),
                Some(GuestUpdate::History(_)) | Some(GuestUpdate::Audio { .. }) => {}
                None => {
                    eprintln!("[join] disconnected");
                    break;
                }
            },
            line = lines.next_line() => {
                let Ok(Some(line)) = line else {
                    // The terminal closed: say bye before going.
                    send.bye();
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    break;
                };
                let text = line.trim();
                let sent = match text {
                    "" => continue,
                    "/quit" | "/exit" => {
                        send.bye();
                        // Let the bye go out before closing.
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        break;
                    }
                    "/stop" => send.stop_turn(),
                    "/approve" | "/deny" => match pending.take() {
                        Some(call_id) => send.approve(&call_id, text == "/approve"),
                        None => {
                            eprintln!("[join] nothing is waiting for approval");
                            continue;
                        }
                    },
                    _ => match text.split_once(' ') {
                        Some(("/chat", rest)) => send.chat(rest),
                        Some(("/todo", rest)) => send.todo(rest),
                        _ if !can_drive => send.chat(text),
                        _ => send.say(text),
                    },
                };
                if !sent {
                    eprintln!("[join] lost the connection");
                    break;
                }
            }
        }
    }
    Ok(())
}

pub(super) async fn load_session_for_ipc_command(
    store: &dcode_ai_runtime::session_store::SessionStore,
    session_id: &str,
    json: bool,
) -> anyhow::Result<dcode_ai_common::session::SessionState> {
    match store.load(session_id).await {
        Ok(session) => Ok(session),
        Err(err) => {
            if json {
                let message = format!("session not found: {session_id}");
                print_command_error_json("session_not_found", &message, Some(session_id))?;
            }
            Err(anyhow::Error::msg(err))
        }
    }
}

pub(super) async fn load_session_snapshot_for_ipc_command(
    store: &dcode_ai_runtime::session_store::SessionStore,
    session_id: &str,
    json: bool,
) -> anyhow::Result<SessionSnapshot> {
    match store.load_snapshot(session_id).await {
        Ok(snapshot) => Ok(snapshot),
        Err(err) => {
            if json {
                let message = format!("session not found: {session_id}");
                print_command_error_json("session_not_found", &message, Some(session_id))?;
            }
            Err(anyhow::Error::msg(err))
        }
    }
}

pub(super) async fn attach_session(
    config: &DcodeAiConfig,
    workspace_root: &Path,
    session_id: &str,
    json: bool,
) -> anyhow::Result<()> {
    let store = dcode_ai_runtime::session_store::SessionStore::new(
        workspace_root.join(&config.session.history_dir),
    );
    let session = load_session_for_ipc_command(&store, session_id, json).await?;

    if let Some(socket_path) = session.meta.socket_path.clone() {
        let client = dcode_ai_runtime::ipc::IpcClient::new(socket_path);
        if let Ok(mut rx) = client.connect().await {
            while let Some(envelope) = rx.recv().await {
                print_event_envelope(&envelope, json)?;
            }
            return Ok(());
        }
    }

    print_log_file(config, workspace_root, session_id, json).await
}

pub(super) async fn show_status(
    config: &DcodeAiConfig,
    workspace_root: &Path,
    session_id: &str,
    json: bool,
) -> anyhow::Result<()> {
    let store = dcode_ai_runtime::session_store::SessionStore::new(
        workspace_root.join(&config.session.history_dir),
    );
    let snapshot = load_session_snapshot_for_ipc_command(&store, session_id, json).await?;
    if json {
        print_json(&snapshot, false)?;
    } else {
        print_human_session(&snapshot);
    }
    Ok(())
}

pub(super) async fn cancel_session(
    config: &DcodeAiConfig,
    workspace_root: &Path,
    session_id: &str,
    json: bool,
) -> anyhow::Result<()> {
    let store = dcode_ai_runtime::session_store::SessionStore::new(
        workspace_root.join(&config.session.history_dir),
    );
    let mut session = load_session_for_ipc_command(&store, session_id, json).await?;

    if let Some(socket_path) = session.meta.socket_path.clone() {
        let client = dcode_ai_runtime::ipc::IpcClient::new(socket_path);
        let _ = client.send_command(&AgentCommand::Shutdown).await;
    }

    if let Some(pid) = session.meta.pid {
        let _ = tokio::process::Command::new("kill")
            .arg(pid.to_string())
            .output()
            .await;
    }

    session.meta.status = SessionStatus::Cancelled;
    session.meta.updated_at = chrono::Utc::now();
    session.meta.pid = None;
    session.meta.socket_path = None;
    store.save(&session).await.map_err(anyhow::Error::msg)?;
    if json {
        print_json(
            &CancelCommandOutput {
                session: session.snapshot(),
                cancelled: true,
            },
            false,
        )?;
    } else {
        println!("Cancelled {session_id}");
    }
    Ok(())
}

pub(super) async fn print_log_file(
    config: &DcodeAiConfig,
    workspace_root: &Path,
    session_id: &str,
    json: bool,
) -> anyhow::Result<()> {
    let log_path = workspace_root
        .join(&config.session.history_dir)
        .join(format!("{session_id}.events.jsonl"));
    let data = match tokio::fs::read_to_string(&log_path).await {
        Ok(data) => data,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            println!("No event log found for {session_id}");
            return Ok(());
        }
        Err(err) => return Err(err.into()),
    };
    for line in data.lines() {
        let envelope: EventEnvelope = serde_json::from_str(line)?;
        print_event_envelope(&envelope, json)?;
    }
    Ok(())
}

/// One line per session: `56044093  3h ago  completed  model  title`.
/// Full details (summary, lineage, paths) are in `--json`.
pub(super) fn print_human_session(session: &SessionSnapshot) {
    let short = session
        .id
        .split('-')
        .filter(|s| s.chars().any(|c| c.is_ascii_digit()))
        .max_by_key(|s| s.len())
        .map(|s| &s[s.len().saturating_sub(8)..])
        .unwrap_or(&session.id);
    let title = session
        .session_name
        .as_deref()
        .map(|t| crate::tui::render_helpers::truncate_chars(t, 70))
        .unwrap_or_default();
    let children = match session.child_session_ids.len() {
        0 => String::new(),
        n => format!("  (+{n} sub-agent{})", if n == 1 { "" } else { "s" }),
    };
    println!(
        "{short:<8}  {:>8}  {:<9}  {:<20}  {title}{children}",
        repl_helpers::time_ago(session.updated_at, chrono::Utc::now()),
        session.status_label(),
        crate::tui::render_helpers::truncate_chars(&session.model, 20),
    );
}

pub(super) fn print_event_envelope(envelope: &EventEnvelope, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string(envelope)?);
    } else {
        stream::render_human_event(&envelope.event);
    }
    Ok(())
}
