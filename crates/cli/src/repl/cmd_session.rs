//! Session, history and background-job commands.

use super::*;

impl Repl {
    #[allow(unused_variables, unused_mut)]
    pub(super) async fn cmd_session(
        &mut self,
        input: &str,
        command: &str,
        rest: &str,
        mut parts: std::str::SplitWhitespace<'_>,
        out: ReplOutput<'_>,
    ) -> anyhow::Result<bool> {
        match command {
            "/stop" => {
                let arg = rest.trim();
                if arg.is_empty() {
                    self.runtime.request_cancel();
                    out.println("[stop] cancelling current turn…");
                } else if arg == "all" {
                    let n = self.background_jobs.len();
                    for job in self.background_jobs.drain(..) {
                        job.handle.abort();
                    }
                    out.println(&format!("[stop] stopped {n} background job(s)"));
                } else if let Ok(id) = arg.parse::<u32>() {
                    if let Some(pos) = self.background_jobs.iter().position(|j| j.id == id) {
                        let job = self.background_jobs.remove(pos);
                        job.handle.abort();
                        out.println(&format!("[stop] stopped job {id} ({})", job.cmd));
                    } else {
                        out.eprintln(&format!("[stop] no background job with id {id}"));
                    }
                } else {
                    out.eprintln("[stop] usage: /stop [<id>|all]  (no arg cancels the turn)");
                }
            }
            "/run-bg" => {
                let cmd = rest.trim();
                if cmd.is_empty() {
                    out.println("[run-bg] usage: /run-bg <shell command>");
                } else {
                    let id = self.start_background_job(cmd);
                    out.println(&format!(
                        "[run-bg] started job {id}: {cmd}  — /ps to view, /stop {id} to kill"
                    ));
                }
            }
            "/bashes" => {
                use dcode_ai_common::event::BackgroundJobStatus as S;
                let jobs = self.runtime.background_jobs();
                let arg = rest.trim();
                if let Some(id) = arg
                    .strip_prefix("kill")
                    .map(str::trim)
                    .and_then(|n| n.parse::<u32>().ok())
                {
                    if jobs.stop(id) {
                        if let ReplOutput::Tui(st) = &out
                            && let Ok(mut g) = st.lock()
                        {
                            g.background_shells.remove(&id);
                        }
                        out.println(&format!("[bashes] stopped job {id}"));
                    } else {
                        out.eprintln(&format!("[bashes] no background job {id}"));
                    }
                    return Ok(true);
                }
                let list = jobs.list();
                if list.is_empty() {
                    out.println(
                        "[bashes] no background shells. The agent starts them with run_background.",
                    );
                    return Ok(true);
                }
                let mut lines = Vec::new();
                for job in &list {
                    let status = match (job.status, job.exit_code) {
                        (S::Running, _) => "running".to_string(),
                        (S::Completed, c) | (S::Failed, c) => match c {
                            Some(c) => format!("exited {c}"),
                            None => "failed".to_string(),
                        },
                        (S::Stopped, _) => "stopped".to_string(),
                    };
                    lines.push(format!("[{}] {status}  $ {}", job.id, job.command));
                    for l in dcode_ai_core::tools::background_exec::output_tail(&job.output)
                        .lines()
                        .rev()
                        .take(6)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                    {
                        lines.push(format!("    {l}"));
                    }
                    lines.push(String::new());
                }
                lines.push("/bashes kill <id> stops a job".into());
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_info_modal("Background shells", lines);
                    }
                } else {
                    for l in lines {
                        out.println(&l);
                    }
                }
            }
            "/ps" => {
                // Drop finished+empty handles; report status.
                if self.background_jobs.is_empty() {
                    out.println("[ps] no background jobs. Start one with /run-bg <cmd>");
                } else {
                    let mut lines = vec!["Background jobs:".to_string(), String::new()];
                    for job in &self.background_jobs {
                        let running = !job.done.load(std::sync::atomic::Ordering::SeqCst);
                        let status = if running { "running" } else { "done" };
                        lines.push(format!("[{}] {status}  $ {}", job.id, job.cmd));
                        // Show the latest output (tail) for both running and
                        // finished jobs, so re-running /ps tracks live progress.
                        if let Ok(buf) = job.output.lock() {
                            let all: Vec<&str> = buf.lines().collect();
                            let start = all.len().saturating_sub(8);
                            for l in &all[start..] {
                                lines.push(format!("    {l}"));
                            }
                        }
                    }
                    lines.push(String::new());
                    lines.push("/stop <id> to kill · /stop all to clear".into());
                    if let ReplOutput::Tui(st) = &out {
                        if let Ok(mut g) = st.lock() {
                            g.open_info_modal("background jobs", lines.clone());
                        }
                    } else {
                        for l in &lines {
                            out.println(l);
                        }
                    }
                }
            }
            "/session-name" | "/rename" => {
                let raw = rest.trim();
                if raw.is_empty() {
                    if let Some(name) = self.runtime.session_name() {
                        out.println(&format!(
                            "[session-name] {name} ({})",
                            self.runtime.session_id()
                        ));
                    } else {
                        out.println(&format!(
                            "[session-name] unset ({})",
                            self.runtime.session_id()
                        ));
                    }
                    out.println("usage: /session-name <name>  (or `/session-name clear`)");
                    return Ok(true);
                }
                if raw.eq_ignore_ascii_case("clear") {
                    self.runtime.set_session_name(None);
                    if let Err(e) = self.runtime.save().await {
                        out.eprintln(&format!("[session-name] cleared, but save failed: {e}"));
                    } else {
                        out.println("[session-name] cleared");
                    }
                    return Ok(true);
                }
                self.runtime.set_session_name(Some(raw.to_string()));
                let applied = self
                    .runtime
                    .session_name()
                    .unwrap_or(self.runtime.session_id());
                if let Err(e) = self.runtime.save().await {
                    out.eprintln(&format!(
                        "[session-name] set to `{applied}`, but save failed: {e}"
                    ));
                } else {
                    out.println(&format!("[session-name] set to `{applied}`"));
                }
            }
            "/rewind" => {
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                    && !g.open_backtrack()
                {
                    g.push_block(DisplayBlock::System("Nothing to rewind to yet.".into()));
                    g.touch_transcript();
                }
            }
            "/clear" => {
                let keep_context = rest == "--keep-context";
                out.clear_screen();
                if keep_context {
                    // In TUI mode we only wipe display blocks; conversation messages are preserved.
                    out.println("[screen cleared — context preserved]");
                } else {
                    out.println("[screen cleared]");
                }
            }
            "/undo" => match self.runtime.undo_last_turn() {
                Ok(Some(msg)) => out.println(&format!("[undo] {msg}")),
                Ok(None) => out.println("[undo] nothing to undo"),
                Err(e) => out.eprintln(&format!("[undo] {e}")),
            },
            "/redo" => match self.runtime.redo_last_turn() {
                Ok(Some(msg)) => out.println(&format!("[redo] {msg}")),
                Ok(None) => out.println("[redo] nothing to redo"),
                Err(e) => out.eprintln(&format!("[redo] {e}")),
            },
            "/fork" => match self.runtime.fork_session().await {
                Ok(new_id) => {
                    if let ReplOutput::Tui(st) = &out
                        && let Ok(mut g) = st.lock()
                    {
                        g.session_id = new_id.clone();
                        g.push_block(DisplayBlock::System(format!(
                            "Forked into new session {new_id} — original is preserved"
                        )));
                        g.touch_transcript();
                    }
                    out.println(&format!("[fork] now on {new_id}"));
                }
                Err(e) => out.eprintln(&format!("[fork] {e}")),
            },
            "/approve" => {
                // dcode has no auto-review pipeline, so this is the feasible
                // analog of Codex's `/approve`: re-allow the most recently
                // requested tool for the rest of the session (handy when you
                // just declined something and want to let it through next time).
                let pattern = if let ReplOutput::Tui(st) = &out {
                    st.lock().ok().and_then(|g| g.last_approval_pattern.clone())
                } else {
                    None
                };
                match pattern {
                    Some(p) => {
                        self.runtime.add_session_allow_pattern(p.clone());
                        if let ReplOutput::Tui(st) = &out
                            && let Ok(mut g) = st.lock()
                        {
                            g.push_block(DisplayBlock::System(format!(
                                "[approve] allowing '{p}' for the rest of this session"
                            )));
                            g.touch_transcript();
                        }
                        out.println(&format!("[approve] allowing '{p}' for this session"));
                    }
                    None => out
                        .eprintln("[approve] nothing to approve (no recent tool approval request)"),
                }
            }
            "/input" => {
                // Send a line straight to an interactive_exec session's stdin,
                // e.g. a sudo password. Handled entirely locally — the text is
                // never sent to the model and never echoed to the transcript.
                let rest = rest.trim();
                if rest.is_empty() {
                    let sessions = self.runtime.interactive_sessions();
                    if sessions.is_empty() {
                        out.println(
                            "[input] no interactive sessions (the agent starts one via interactive_exec)",
                        );
                    } else {
                        out.println("[input] active interactive sessions:");
                        for (id, running, cmd) in sessions {
                            out.println(&format!(
                                "  [{id}] {}  $ {cmd}",
                                if running { "running" } else { "exited" }
                            ));
                        }
                        out.println(
                            "usage: /input <id> <text>  — sent straight to the command, NOT to the AI",
                        );
                    }
                } else {
                    let mut parts = rest.splitn(2, char::is_whitespace);
                    let id_str = parts.next().unwrap_or("");
                    let text = parts.next().unwrap_or("");
                    match id_str.parse::<u32>() {
                        Ok(id) => match self.runtime.interactive_write(id, text) {
                            Ok(()) => out.println(&format!(
                                "[input] sent to session {id} (kept local — not shared with the AI)"
                            )),
                            Err(e) => out.eprintln(&format!("[input] {e}")),
                        },
                        Err(_) => out.eprintln("[input] usage: /input <id> <text>"),
                    }
                }
            }
            "/side" => {
                // Ephemeral aside: snapshot the conversation context, let the
                // user have a throwaway exchange, then `/side end` restores the
                // main thread's context (the aside is discarded from the model's
                // memory). The aside stays visible in the transcript for
                // reference. Supports nesting.
                let sub = rest.trim().to_ascii_lowercase();
                if sub == "end" || sub == "return" || sub == "back" {
                    match self.side_snapshots.pop() {
                        Some(saved) => {
                            self.runtime.restore_messages(saved);
                            let depth = self.side_snapshots.len();
                            if let ReplOutput::Tui(st) = &out
                                && let Ok(mut g) = st.lock()
                            {
                                g.push_block(DisplayBlock::System(
                                    "↩ returned from side conversation — main context restored."
                                        .into(),
                                ));
                                g.touch_transcript();
                            }
                            out.println(&format!(
                                "[side] returned to main thread{}",
                                if depth > 0 {
                                    format!(" ({depth} aside(s) still open)")
                                } else {
                                    String::new()
                                }
                            ));
                        }
                        None => {
                            out.eprintln("[side] not in a side conversation (start one with /side)")
                        }
                    }
                } else {
                    self.side_snapshots.push(self.runtime.snapshot_messages());
                    if let ReplOutput::Tui(st) = &out
                        && let Ok(mut g) = st.lock()
                    {
                        g.push_block(DisplayBlock::System(
                            "⤷ side conversation started (ephemeral) — use /side end to return; the aside won't affect the main thread."
                                .into(),
                        ));
                        g.touch_transcript();
                    }
                    out.println("[side] side conversation started — /side end to return");
                }
            }
            "/delete" => {
                let current = self.runtime.session_id().to_string();
                let store = dcode_ai_runtime::session_store::SessionStore::new(
                    self.runtime
                        .workspace_root()
                        .join(&self.runtime.config().session.history_dir),
                );
                match store.delete(&current).await {
                    Ok(_) => {
                        out.println(&format!("[delete] removed session {current}"));
                        if let Err(e) = self.runtime.new_session().await {
                            out.eprintln(&format!("[delete] new session failed: {e}"));
                        } else {
                            if let ReplOutput::Tui(st) = &out
                                && let Ok(mut g) = st.lock()
                            {
                                g.blocks.clear();
                                g.flushed_block_count = 0;
                                g.request_clear = true;
                                g.streaming_assistant = None;
                                g.session_id = self.runtime.session_id().to_string();
                                g.transcript_follow_tail = true;
                                g.touch_transcript();
                            }
                            out.println("[delete] started a fresh session");
                        }
                    }
                    Err(e) => out.eprintln(&format!("[delete] {e}")),
                }
            }
            "/compact" => {
                if rest == "--preview" {
                    let preview = self.runtime.compaction_preview();
                    out.println(&format!("compaction preview:\n{}", preview));
                    return Ok(true);
                }
                // Claude Code's /compact: summarize the conversation (with
                // optional focus instructions) and continue from the summary.
                let focus = (!rest.trim().is_empty()).then_some(rest.trim());
                out.println("[compact] summarizing the conversation…");
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.set_busy(true);
                }
                let result = self.runtime.compact_now(focus).await;
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.set_busy(false);
                }
                match result {
                    Ok((before, after, summary)) => {
                        let _ = self
                            .runtime
                            .append_memory_note("session-summary", Some(summary.clone()))
                            .await;
                        out.println(&format!(
                            "[compact] conversation compacted: ~{before} → ~{after} tokens. Summary:\n{summary}"
                        ));
                    }
                    Err(e) => out.eprintln(&format!("[compact] {e}")),
                }
            }
            "/logs" => match tokio::fs::read_to_string(self.runtime.event_log_path()).await {
                Ok(data) => {
                    if let ReplOutput::Tui(st) = &out {
                        let lines: Vec<String> =
                            data.lines().rev().take(100).map(String::from).collect();
                        let lines: Vec<String> = lines.into_iter().rev().collect();
                        if let Ok(mut g) = st.lock() {
                            g.open_info_modal("logs (last 100)", lines);
                        }
                    } else {
                        out.print(&data);
                    }
                }
                Err(err) => out.eprintln(&format!("failed to read log: {err}")),
            },
            "/attach" => {
                let snapshot = self.runtime.snapshot();
                let session_line = snapshot
                    .session_name
                    .as_ref()
                    .map(|name| format!("Session:  {} ({})", name, snapshot.id))
                    .unwrap_or_else(|| format!("Session:  {}", snapshot.id));
                let lines = vec![
                    session_line,
                    format!(
                        "Socket:   {}",
                        snapshot
                            .socket_path
                            .as_ref()
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|| "<none>".into())
                    ),
                ];
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_info_modal("attach", lines);
                    }
                } else {
                    for l in &lines {
                        out.println(l);
                    }
                }
            }
            "/sessions" => match self.runtime.list_session_ids().await {
                Ok(mut ids) => {
                    ids.sort();
                    if ids.is_empty() {
                        let lines = vec!["No saved sessions.".into()];
                        if let ReplOutput::Tui(st) = &out {
                            if let Ok(mut g) = st.lock() {
                                g.open_info_modal("sessions", lines);
                            }
                        } else {
                            out.println("no saved sessions");
                        }
                    } else if let ReplOutput::Tui(st) = &out {
                        let current = self.runtime.session_id().to_string();
                        let store = dcode_ai_runtime::session_store::SessionStore::new(
                            self.runtime
                                .workspace_root()
                                .join(&self.runtime.config().session.history_dir),
                        );
                        let sessions_dir = self
                            .runtime
                            .workspace_root()
                            .join(&self.runtime.config().session.history_dir);
                        let now = chrono::Utc::now();
                        let mut rows = Vec::new();
                        for id in &ids {
                            let snapshot = store.load_snapshot(id).await.ok();
                            // Sub-agent sessions live under their parent.
                            if snapshot
                                .as_ref()
                                .is_some_and(|s| s.parent_session_id.is_some())
                            {
                                continue;
                            }
                            let updated = snapshot.as_ref().map(|s| s.updated_at);
                            let name = snapshot.as_ref().and_then(|s| s.session_name.clone());
                            let model = snapshot
                                .as_ref()
                                .map(|s| s.model.clone())
                                .unwrap_or_default();
                            let first = session_first_prompt(&sessions_dir, id);
                            let when = updated
                                .map(|t| time_ago(t, now))
                                .unwrap_or_else(|| "?".into());
                            let label = session_picker_label(
                                &when,
                                name.as_deref(),
                                first.as_deref(),
                                &model,
                                id,
                            );
                            let search_text = format!(
                                "{} {} {id}",
                                name.unwrap_or_default(),
                                first.unwrap_or_default()
                            );
                            let preview =
                                session_event_preview(&sessions_dir, id, 3).unwrap_or_default();
                            rows.push((
                                updated,
                                SessionPickerEntry {
                                    id: id.clone(),
                                    label,
                                    search_text,
                                    preview,
                                },
                            ));
                        }
                        // Newest first.
                        rows.sort_by_key(|r| std::cmp::Reverse(r.0));
                        let entries: Vec<SessionPickerEntry> =
                            rows.into_iter().map(|(_, e)| e).collect();
                        if let Ok(mut g) = st.lock() {
                            g.open_session_picker(entries, &current);
                        }
                    } else {
                        let store = dcode_ai_runtime::session_store::SessionStore::new(
                            self.runtime
                                .workspace_root()
                                .join(&self.runtime.config().session.history_dir),
                        );
                        for id in ids {
                            match store.load_snapshot(&id).await {
                                Ok(snapshot) => {
                                    if let Some(name) = snapshot.session_name {
                                        out.println(&format!("{name}\t{id}"));
                                    } else {
                                        out.println(&id);
                                    }
                                }
                                Err(_) => out.println(&id),
                            }
                        }
                    }
                }
                Err(error) => {
                    out.eprintln(&format!("failed to list sessions: {error}"));
                }
            },
            "/sessions-clean" => match self.runtime.cleanup_empty_sessions().await {
                Ok(deleted) => {
                    if deleted.is_empty() {
                        out.println("no empty sessions found");
                    } else {
                        out.println(&format!("removed {} empty sessions", deleted.len()));
                        if !matches!(out, ReplOutput::Tui(_)) {
                            for id in deleted {
                                out.println(&id);
                            }
                        }
                    }
                }
                Err(error) => {
                    out.eprintln(&format!("failed to clean empty sessions: {error}"));
                }
            },
            "/new" => {
                let summary = self.runtime.compact_summary();
                self.runtime.set_session_summary(Some(summary.clone()));
                self.runtime
                    .append_memory_note("session-summary", Some(summary))
                    .await
                    .map_err(anyhow::Error::msg)?;
                self.runtime.save().await.map_err(anyhow::Error::msg)?;
                self.runtime
                    .new_session()
                    .await
                    .map_err(anyhow::Error::msg)?;
                let new_id = self.runtime.session_id().to_string();
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.blocks.clear();
                    g.flushed_block_count = 0;
                    let version = env!("CARGO_PKG_VERSION");
                    g.blocks.push(DisplayBlock::System(format!(
                        "dcode-ai v{version} · new session"
                    )));
                    g.streaming_assistant = None;
                    g.scroll_lines = 0;
                    g.transcript_follow_tail = true;
                    g.session_id = new_id.clone();
                    g.model = self.runtime.model().to_string();
                    g.input_tokens = 0;
                    g.output_tokens = 0;
                    g.cost_usd = 0.0;
                    g.started = std::time::Instant::now();
                    g.touch_transcript();
                }
                out.println(&format!("new session started: {new_id}"));
            }
            "/export" => {
                let snapshot = self.runtime.snapshot();
                let events = self.runtime.event_log_path();
                let md = match tokio::fs::read_to_string(&events).await {
                    Ok(raw) => {
                        let mut md_lines =
                            vec![format!("# Session {}", snapshot.id), String::new()];
                        for line in raw.lines() {
                            if let Ok(val) = serde_json::from_str::<serde_json::Value>(line)
                                && let Some(kind) = val.get("kind").and_then(|v| v.as_str())
                            {
                                match kind {
                                    "MessageReceived" => {
                                        let role =
                                            val.get("role").and_then(|v| v.as_str()).unwrap_or("?");
                                        let content = val
                                            .get("content")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("");
                                        md_lines.push(format!("## {role}"));
                                        md_lines.push(String::new());
                                        md_lines.push(content.to_string());
                                        md_lines.push(String::new());
                                    }
                                    "ToolCallStarted" => {
                                        let tool =
                                            val.get("tool").and_then(|v| v.as_str()).unwrap_or("?");
                                        md_lines.push(format!("### tool: {tool}"));
                                        md_lines.push(String::new());
                                    }
                                    _ => {}
                                }
                            }
                        }
                        md_lines.join("\n")
                    }
                    Err(e) => {
                        out.eprintln(&format!("[export] failed to read event log: {e}"));
                        return Ok(true);
                    }
                };
                let export_path = self
                    .runtime
                    .workspace_root()
                    .join(format!(".dcode-ai/export-{}.md", snapshot.id));
                if let Some(parent) = export_path.parent() {
                    let _ = tokio::fs::create_dir_all(parent).await;
                }
                match tokio::fs::write(&export_path, &md).await {
                    Ok(()) => out.println(&format!("exported to {}", export_path.display())),
                    Err(e) => out.eprintln(&format!("[export] {e}")),
                }
            }
            // ── /retry ────────────────────────────────────────────────────────
            "/retry" => {
                let last_user = self
                    .runtime
                    .messages()
                    .iter()
                    .rev()
                    .find(|m| m.role == dcode_ai_common::message::Role::User)
                    .map(|m| m.content.event_preview());
                match last_user {
                    Some(text) if !text.trim().is_empty() => {
                        out.println("[retry] Re-sending last message…");
                        match self.runtime.run_turn(text.trim()).await {
                            Ok(response) => {
                                if matches!(out, ReplOutput::Stdio) {
                                    out.println(&response);
                                }
                            }
                            Err(e) => out.eprintln(&format!("[retry] {e}")),
                        }
                    }
                    _ => out.eprintln("[retry] No previous user message to retry"),
                }
            }

            // ── /cd <path> ─────────────────────────────────────────────────
            "/cd" => {
                let target = rest.trim();
                if target.is_empty() {
                    out.println(&format!(
                        "[cd] current: {}",
                        self.runtime.workspace_root().display()
                    ));
                    return Ok(true);
                }
                let new_root = if std::path::Path::new(target).is_absolute() {
                    std::path::PathBuf::from(target)
                } else {
                    self.runtime.workspace_root().join(target)
                };
                match dcode_ai_common::config::canonicalize_simplified(&new_root) {
                    Ok(canonical) if canonical.is_dir() => {
                        std::env::set_current_dir(&canonical)
                            .map_err(|e| anyhow::anyhow!("chdir: {e}"))?;
                        out.println(&format!("[cd] shell cwd set to {}", canonical.display()));
                        out.println(
                            "[cd] note: the agent's file tools still operate on the session \
workspace. To fully switch, run dcode-ai in the new directory (or `/project add <path>` \
then switch).",
                        );
                    }
                    Ok(_) => out.eprintln(&format!("[cd] not a directory: {target}")),
                    Err(e) => out.eprintln(&format!("[cd] {target}: {e}")),
                }
            }

            // ── /project ──────────────────────────────────────────────────────
            "/project" => {
                let sub = rest.trim();
                if sub.is_empty() || sub == "list" || sub.starts_with("switch") {
                    // Open the picker (or report empty). Never hold the state lock
                    // across out.println — ReplOutput::Tui re-locks it (deadlock).
                    let mut opened = false;
                    let mut count = 0usize;
                    if let ReplOutput::Tui(st) = &out
                        && let Ok(mut g) = st.lock()
                    {
                        count = g.connected_projects.len();
                        if count > 0 {
                            g.open_project_picker();
                            opened = true;
                        }
                    }
                    if matches!(out, ReplOutput::Tui(_)) {
                        if opened {
                            out.println(&format!("[project] {count} project(s) — ↑↓ Enter to switch, Del to remove, Esc to close"));
                        } else {
                            out.println(
                                "[project] no projects yet. Add one with: /project add <path>",
                            );
                        }
                    } else {
                        out.println("[project] use in TUI mode for the project picker");
                    }
                } else if let Some(path) = sub.strip_prefix("add ").map(|s| s.trim()) {
                    let abs = if std::path::Path::new(path).is_absolute() {
                        std::path::PathBuf::from(path)
                    } else {
                        self.runtime.workspace_root().join(path)
                    };
                    match dcode_ai_common::config::canonicalize_simplified(&abs) {
                        Ok(canonical) if canonical.is_dir() => {
                            let name = canonical
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("project")
                                .to_string();
                            if let ReplOutput::Tui(st) = &out
                                && let Ok(mut g) = st.lock()
                            {
                                g.add_project(name.clone(), canonical.clone());
                                g.touch_transcript();
                            }
                            out.println(&format!(
                                "[project] added '{name}' — open the picker with /project or Ctrl+X P"
                            ));
                        }
                        Ok(_) => out.eprintln(&format!("[project] not a directory: {path}")),
                        Err(e) => out.eprintln(&format!("[project] {path}: {e}")),
                    }
                } else {
                    out.println("[project] usage: /project [list | add <path> | switch]");
                }
            }

            // ── /import ───────────────────────────────────────────────────────
            // Import Claude Code project instructions (CLAUDE.md) into the
            // dcode-ai instructions file (AGENTS.md).
            "/import" if rest.trim().eq_ignore_ascii_case("chats") => {
                // Import the most recent Claude Code chat for this workspace as
                // context (Claude Code stores sessions as JSONL under
                // ~/.claude/projects/<encoded-cwd>/).
                let ws = self.runtime.workspace_root().to_path_buf();
                match import_latest_claude_chat(&ws) {
                    Ok((title, text)) => {
                        out.println(&format!("[import] imported chat: {title}"));
                        out.println("[import] staged as context — send a message to use it");
                        let block =
                            format!("Imported Claude Code chat \"{title}\":\n```text\n{text}\n```");
                        if let ReplOutput::Tui(st) = &out {
                            if let Ok(mut g) = st.lock() {
                                g.pending_context.push(block);
                            }
                        } else {
                            let prompt = format!(
                                "Context from a prior Claude Code chat \"{title}\":\n\n```\n{text}\n```\n\nAcknowledge briefly."
                            );
                            let _ = self.runtime.run_turn(&prompt).await;
                        }
                    }
                    Err(e) => out.eprintln(&format!("[import] {e}")),
                }
            }
            "/import" => {
                let ws = self.runtime.workspace_root().to_path_buf();
                let claude_md = ws.join("CLAUDE.md");
                let agents_md = ws.join("AGENTS.md");
                const MARKER: &str = "# Imported from CLAUDE.md (Claude Code)";
                match std::fs::read_to_string(&claude_md) {
                    Ok(content) => {
                        let block = format!("{MARKER}\n\n{}", content.trim());
                        let existing = std::fs::read_to_string(&agents_md).unwrap_or_default();
                        if existing.contains(MARKER) {
                            out.println(
                                "[import] AGENTS.md already has an imported CLAUDE.md section — skipping.",
                            );
                        } else {
                            let merged = if existing.trim().is_empty() {
                                block
                            } else {
                                format!("{}\n\n{block}", existing.trim_end())
                            };
                            match std::fs::write(&agents_md, merged) {
                                Ok(()) => {
                                    self.runtime.refresh_system_prompt();
                                    out.println(
                                        "[import] Imported CLAUDE.md into AGENTS.md and reloaded instructions.",
                                    );
                                }
                                Err(e) => out
                                    .eprintln(&format!("[import] failed to write AGENTS.md: {e}")),
                            }
                        }
                    }
                    Err(_) => {
                        out.println(
                            "[import] No CLAUDE.md found in this workspace — nothing to import.",
                        );
                        out.println("  (Imports CLAUDE.md → AGENTS.md. For prior chats, use /import chats.)");
                    }
                }
            }

            "/history" => {
                let query = rest.trim();
                if query.is_empty() {
                    out.eprintln("[history] usage: /history <search term>");
                    return Ok(true);
                }
                let sessions_dir = self
                    .runtime
                    .workspace_root()
                    .join(&self.runtime.config().session.history_dir);
                let results = search_session_history(&sessions_dir, query, 20);
                if results.is_empty() {
                    out.println(&format!("[history] No matches for \"{query}\""));
                } else {
                    out.println(&format!(
                        "[history] {} match(es) for \"{query}\":",
                        results.len()
                    ));
                    for r in &results {
                        out.println(r);
                    }
                }
            }

            _ => {}
        }
        Ok(true)
    }
}
