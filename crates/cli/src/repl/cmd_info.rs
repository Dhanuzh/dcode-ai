//! Information views: help, status, context, cost, diff.

use super::*;

impl Repl {
    #[allow(unused_variables, unused_mut)]
    pub(super) async fn cmd_info(
        &mut self,
        input: &str,
        command: &str,
        rest: &str,
        mut parts: std::str::SplitWhitespace<'_>,
        out: ReplOutput<'_>,
    ) -> anyhow::Result<bool> {
        match command {
            "/help" => {
                let help_lines = vec![
                    "dcode-ai Interactive Mode".into(),
                    String::new(),
                    "INPUT MODES:".into(),
                    "  ! <cmd>     Run shell command (output feeds into context)".into(),
                    "  @path       Inline file mentions".into(),
                    "  / <cmd>     Slash commands".into(),
                    "  \\           Multiline input (end line with \\ to continue)".into(),
                    String::new(),
                    "SLASH COMMANDS:".into(),
                    "  /help              Show this help".into(),
                    "  /keymaps           Show keyboard shortcuts".into(),
                    "  /status            Session status".into(),
                    "  /context           Session context snapshot".into(),
                    "  /session-name      Show/set manual session name".into(),
                    "  /agent [profile]   Show or switch agent profile".into(),
                    "  /plan <task>       Planning-oriented turn".into(),
                    "  /review <task>     Code review turn".into(),
                    "  /fix <task>        Bug-fix turn".into(),
                    "  /test <task>       Validation turn".into(),
                    "  /clear             Clear the screen".into(),
                    "  /compact           Compact session summary".into(),
                    "  /compact --preview Preview preserved compaction context".into(),
                    "  /new               Start a new session".into(),
                    "  /export            Export session to markdown".into(),
                    "  /thinking          Toggle thinking/reasoning visibility".into(),
                    "  /skills            List discovered skills".into(),
                    "  /memory [text]     Show or store memory notes".into(),
                    "  /models            Browse and select models".into(),
                    "  /connect           Connect LLM provider".into(),
                    "  /login             Alias of /connect".into(),
                    "  /logout [target]   Logout provider auth".into(),
                    "  /auth              Show auth/login status".into(),
                    "  /provider [name]   Provider connect/switch".into(),
                    "  /editor [seed]     Open external editor".into(),
                    "  /set-editor <cmd>  Persist editor command".into(),
                    "  /mcp               List MCP servers".into(),
                    "  /sessions          List/switch sessions".into(),
                    "  /sessions-clean    Remove old empty sessions".into(),
                    "  /permissions [m]   Show or set permission mode".into(),
                    "  /config            Show runtime config".into(),
                    "  /doctor            Run config checks".into(),
                    "  /diff              Show recent file changes".into(),
                    "  /cost              Show token usage".into(),
                    "  /stats             Session statistics".into(),
                    "  /exit              Exit repl".into(),
                    String::new(),
                    "KEYBOARD SHORTCUTS:".into(),
                    "  Use /keymaps for the complete keymap list.".into(),
                ];
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_info_modal("help", help_lines);
                    }
                } else {
                    for l in &help_lines {
                        out.println(l);
                    }
                }
            }
            "/keymaps" => {
                let lines = keymaps_help_lines();
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_info_modal("keymaps", lines);
                    }
                } else {
                    for l in &lines {
                        out.println(l);
                    }
                }
            }
            "/status" => {
                let snapshot = self.runtime.snapshot();
                let model = self.runtime.model().to_string();
                let provider = self.runtime.config().provider.default.display_name();
                let mcp_count = self
                    .runtime
                    .config()
                    .mcp
                    .servers
                    .iter()
                    .filter(|s| s.enabled)
                    .count();
                let window =
                    dcode_ai_runtime::model_limits::effective_context_window(&model) as u64;

                // Live token/cost/context come from the TUI state (CostUpdated).
                let (tin, tout, cost, ctx, projects) = if let ReplOutput::Tui(st) = &out {
                    st.lock()
                        .ok()
                        .map(|g| {
                            (
                                g.input_tokens,
                                g.output_tokens,
                                g.cost_usd,
                                g.context_tokens,
                                g.connected_projects.len(),
                            )
                        })
                        .unwrap_or((0, 0, 0.0, 0, 0))
                } else {
                    (0, 0, 0.0, 0, 0)
                };

                let session_line = snapshot
                    .session_name
                    .as_ref()
                    .map(|name| format!("Session:     {} ({})", name, snapshot.id))
                    .unwrap_or_else(|| format!("Session:     {}", snapshot.id));
                let mut lines = vec![
                    session_line,
                    format!("Model:       {model}"),
                    format!("Provider:    {provider}"),
                    format!("Agent:       @{}", self.agent_profile.label()),
                    format!("Permission:  {:?}", self.runtime.permission_mode()),
                    format!("Workspace:   {}", self.runtime.workspace_root().display()),
                ];
                if window > 0 {
                    let pct = ((ctx.min(window) as f64 / window as f64) * 100.0).round() as u64;
                    let filled = ((pct as usize * 16) / 100).min(16);
                    let bar: String = "█".repeat(filled) + &"░".repeat(16 - filled);
                    lines.push(format!(
                        "Context:     {bar} {pct}%  ({} / {} tokens)",
                        fmt_count(ctx),
                        fmt_count(window)
                    ));
                }
                lines.push(format!(
                    "Tokens:      {} in · {} out",
                    fmt_count(tin),
                    fmt_count(tout)
                ));
                lines.push(format!("Cost:        ${cost:.4}"));
                if mcp_count > 0 {
                    lines.push(format!("MCP:         {mcp_count} server(s)"));
                    if let ReplOutput::Tui(st) = &out
                        && let Ok(g) = st.lock()
                    {
                        for (name, status) in &g.mcp_server_statuses {
                            let status_str = match status {
                                dcode_ai_common::event::McpStartupStatus::Starting => "starting",
                                dcode_ai_common::event::McpStartupStatus::Initializing => {
                                    "initializing"
                                }
                                dcode_ai_common::event::McpStartupStatus::Ready => "ready",
                                dcode_ai_common::event::McpStartupStatus::Failed { .. } => "failed",
                            };
                            lines.push(format!("  • {:<10} {}", name, status_str));
                        }
                    }
                }
                if projects > 1 {
                    lines.push(format!("Projects:    {projects} connected"));
                }
                lines.push(format!("Children:    {}", snapshot.child_session_ids.len()));
                lines.push(format!(
                    "Memory:      {}",
                    self.runtime.memory_store_path().display()
                ));
                if let Some(summary) = &snapshot.session_summary {
                    lines.push(String::new());
                    lines.push(format!("Summary: {}", summary.replace('\n', " ")));
                }
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_info_modal("status", lines);
                    }
                } else {
                    for l in &lines {
                        out.println(l);
                    }
                }
            }
            "/context" => {
                let lines = context_usage_lines(
                    self.runtime.model(),
                    self.runtime.context_breakdown(),
                    dcode_ai_runtime::model_limits::effective_context_window(self.runtime.model())
                        as u64,
                    self.runtime.messages().len(),
                );
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_info_modal("Context usage", lines);
                    }
                } else {
                    for l in &lines {
                        out.println(l);
                    }
                }
            }
            // In the TUI, bare `/diff` opens the full-screen diff view.
            "/diff" if rest.trim().is_empty() && matches!(out, ReplOutput::Tui(_)) => {
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.diff_overlay_open = true;
                }
            }
            "/diff" => {
                // Working-tree diff incl. untracked files (Codex-style), rendered
                // as a colored ```diff block. Pure git + Rust so it works the
                // same on Windows (the previous POSIX one-liner did not).
                let ws = self.runtime.workspace_root().to_path_buf();
                let tracked = tokio::process::Command::new("git")
                    .args(["--no-pager", "diff", "--no-color", "HEAD"])
                    .current_dir(&ws)
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .output()
                    .await;
                match tracked {
                    Ok(cmd_out) => {
                        let mut diff_buf = String::from_utf8_lossy(&cmd_out.stdout).into_owned();
                        // Untracked files render as synthetic new-file hunks.
                        if let Ok(untracked) = tokio::process::Command::new("git")
                            .args(["ls-files", "--others", "--exclude-standard"])
                            .current_dir(&ws)
                            .stdout(Stdio::piped())
                            .stderr(Stdio::piped())
                            .output()
                            .await
                        {
                            for f in String::from_utf8_lossy(&untracked.stdout).lines() {
                                let f = f.trim();
                                if f.is_empty() {
                                    continue;
                                }
                                let Ok(content) = std::fs::read_to_string(ws.join(f)) else {
                                    continue;
                                };
                                diff_buf.push_str(&format!(
                                    "diff --git a/{f} b/{f}\nnew file\n--- /dev/null\n+++ b/{f}\n"
                                ));
                                for line in content.lines() {
                                    diff_buf.push('+');
                                    diff_buf.push_str(line);
                                    diff_buf.push('\n');
                                }
                            }
                        }
                        let diff = diff_buf.trim();
                        if diff.is_empty() {
                            out.println("[diff] No changes in the working tree.");
                        } else {
                            // Cap very large diffs so the transcript stays usable.
                            let capped: String = if diff.lines().count() > 600 {
                                let head: String =
                                    diff.lines().take(600).collect::<Vec<_>>().join("\n");
                                format!("{head}\n… (diff truncated)")
                            } else {
                                diff.to_string()
                            };
                            out.print_markdown(&format!("```diff\n{capped}\n```"));
                        }
                    }
                    Err(e) => out.eprintln(&format!("[diff] Failed: {e}")),
                }
            }
            "/copy" if matches!(rest.trim(), "link" | "url") => {
                // The newest link in the transcript, whole (a long URL that
                // wraps can't be copied cleanly with the mouse).
                if let ReplOutput::Tui(st) = &out {
                    let link = st.lock().ok().and_then(|g| {
                        g.blocks.iter().rev().find_map(|b| {
                            let text = match b {
                                DisplayBlock::Assistant(t)
                                | DisplayBlock::System(t)
                                | DisplayBlock::User(t)
                                | DisplayBlock::ErrorLine(t)
                                | DisplayBlock::Notice { text: t, .. } => t.as_str(),
                                DisplayBlock::ToolDone { detail, .. } => detail.as_str(),
                                _ => return None,
                            };
                            crate::repl_helpers::last_link(text)
                        })
                    });
                    match link {
                        Some(url) => match crate::tui::clipboard::copy_to_clipboard(&url) {
                            Ok(_) => out.println(&format!("[copy] Copied {url}")),
                            Err(e) => out.eprintln(&format!("[copy] {e}")),
                        },
                        None => out.println("[copy] No link in the conversation yet."),
                    }
                } else {
                    out.println("[copy] available in TUI mode");
                }
            }
            "/copy" => {
                // Copy the last assistant response to the clipboard.
                if let ReplOutput::Tui(st) = &out {
                    let text = st.lock().ok().and_then(|g| {
                        g.blocks.iter().rev().find_map(|b| match b {
                            DisplayBlock::Assistant(t) if !t.trim().is_empty() => Some(t.clone()),
                            _ => None,
                        })
                    });
                    match text {
                        Some(t) => match crate::tui::clipboard::copy_to_clipboard(&t) {
                            Ok(_) => out.println("[copy] Last response copied to clipboard."),
                            Err(e) => out.eprintln(&format!("[copy] {e}")),
                        },
                        None => out.println("[copy] No assistant response to copy yet."),
                    }
                } else {
                    out.println("[copy] available in TUI mode");
                }
            }
            "/dashboard" | "/dash" => {
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.toggle_view_mode();
                    }
                } else {
                    out.println("[dashboard] available in TUI mode");
                }
            }
            "/transcript" => {
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.transcript_overlay_open = true;
                        g.transcript_overlay_scroll = usize::MAX; // open at bottom
                    }
                    out.println("[transcript] full-screen view — q to close");
                } else {
                    out.println("[transcript] available in TUI mode");
                }
            }
            "/cost" | "/usage" => {
                let model = self.runtime.model().to_string();
                let window =
                    dcode_ai_runtime::model_limits::effective_context_window(&model) as u64;
                let (tin, tout, cost, ctx) = if let ReplOutput::Tui(st) = &out {
                    st.lock()
                        .ok()
                        .map(|g| {
                            (
                                g.input_tokens,
                                g.output_tokens,
                                g.cost_usd,
                                g.context_tokens,
                            )
                        })
                        .unwrap_or((0, 0, 0.0, 0))
                } else {
                    (0, 0, 0.0, 0)
                };
                let mut lines = vec![
                    format!("Model:    {model}"),
                    format!("Input:    {} tokens", fmt_count(tin)),
                    format!("Output:   {} tokens", fmt_count(tout)),
                    format!("Total:    {} tokens", fmt_count(tin + tout)),
                    format!("Cost:     ${cost:.4}"),
                ];
                if window > 0 {
                    let pct = ((ctx.min(window) as f64 / window as f64) * 100.0).round() as u64;
                    let filled = ((pct as usize * 16) / 100).min(16);
                    let bar: String = "█".repeat(filled) + &"░".repeat(16 - filled);
                    lines.push(format!(
                        "Context:  {bar} {pct}%  ({} / {})",
                        fmt_count(ctx),
                        fmt_count(window)
                    ));
                }
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_info_modal("usage", lines);
                    }
                } else {
                    for l in &lines {
                        out.println(l);
                    }
                }
            }
            "/release-notes" => {
                let lines = release_notes(CHANGELOG, 3);
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_info_modal("Release notes", lines);
                    }
                } else {
                    for l in lines {
                        out.println(&l);
                    }
                }
            }
            "/goals" | "/todos" => {
                // Goal tracking: recall the agent's current task plan (set via
                // the update_plan tool) even after it scrolls out of view.
                let plan = if let ReplOutput::Tui(st) = &out {
                    st.lock().ok().and_then(|g| g.current_plan.clone())
                } else {
                    None
                };
                match plan {
                    Some(p) => {
                        out.println("[goals] current task plan:");
                        for line in p.lines() {
                            out.println(&format!("  {line}"));
                        }
                    }
                    None => out.println(
                        "[goals] no active plan yet — the agent sets one via update_plan on multi-step tasks",
                    ),
                }
            }
            "/stats" => {
                let snapshot = self.runtime.snapshot();
                let session_line = snapshot
                    .session_name
                    .as_ref()
                    .map(|name| format!("Session:     {} ({})", name, snapshot.id))
                    .unwrap_or_else(|| format!("Session:     {}", snapshot.id));
                let lines = vec![
                    session_line,
                    format!("Model:       {}", self.runtime.model()),
                    format!("Agent:       @{}", self.agent_profile.label()),
                    format!("Permission:  {:?}", self.runtime.permission_mode()),
                    format!("Children:    {}", snapshot.child_session_ids.len()),
                    format!(
                        "Memory:      {}",
                        self.runtime.memory_store_path().display()
                    ),
                ];
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_info_modal("stats", lines);
                    }
                } else {
                    for l in &lines {
                        out.println(l);
                    }
                }
            }
            "/doctor" => {
                self.doctor(&out).await;
            }
            // ── /map ──────────────────────────────────────────────────────────
            "/map" => {
                let workspace = self.runtime.workspace_root().to_path_buf();
                let files = discover_workspace_files(&workspace);
                let tree = build_file_tree(&files);
                let n = files.len();
                out.println(&format!("[map] {n} files"));
                out.println(&tree);
            }

            // ── /feedback ─────────────────────────────────────────────────────
            "/feedback" => {
                let log = self.runtime.event_log_path();
                out.println("[feedback] Found a bug or have a request? Open an issue:");
                out.println("  https://github.com/Dhanuzh/dcode-ai/issues/new");
                out.println(&format!(
                    "  Attach this session's log if it helps: {}",
                    log.display()
                ));
            }

            _ => {}
        }
        Ok(true)
    }
}
