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
                if dcode_ai_common::config::per_token_billing() {
                    lines.push(format!("Cost:        ${cost:.4} (API list prices)"));
                } else {
                    lines.push("Cost:        on your plan (tokens count toward its limits)".into());
                }
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
            "/context" if rest.trim_start().starts_with("window") => {
                let arg = rest.trim_start().trim_start_matches("window").trim();
                let setting = match arg {
                    "" => None,
                    "default" | "reset" | "config" => Some(None),
                    a => dcode_ai_common::config::WindowSetting::parse(a).map(Some),
                };
                match setting {
                    None => out.eprintln(
                        "usage: /context window full|200k|<tokens>|default — full: the model's whole window (no early compaction); a number: work within it; default: back to [memory.context]",
                    ),
                    Some(w) => {
                        let label = match &w {
                            None => "default ([memory.context])".to_string(),
                            Some(s) if s.cap().is_none() => "the model's whole window, no early compaction".to_string(),
                            Some(s) => format!("{} tokens", s.cap().unwrap_or(0)),
                        };
                        self.runtime.set_window_override(w);
                        let b = self.runtime.context_breakdown();
                        out.println(&format!(
                            "[context] window for this session: {label} — window {} · compacts at {}",
                            b.window, b.compact_at
                        ));
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
            // In the TUI, bare `/diff` (or `/diff open`) opens the full-screen diff view.
            "/diff" if matches!(rest.trim(), "" | "open") && matches!(out, ReplOutput::Tui(_)) => {
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
            "/copy" if rest.trim().starts_with("code") => {
                // The newest code block in a reply (`/copy code 2`: the one
                // before it).
                let back: usize = rest.trim()["code".len()..]
                    .trim()
                    .parse()
                    .unwrap_or(1)
                    .max(1);
                if let ReplOutput::Tui(st) = &out {
                    let block = st.lock().ok().and_then(|g| {
                        g.blocks
                            .iter()
                            .rev()
                            .filter_map(|b| match b {
                                DisplayBlock::Assistant(t) => Some(t.as_str()),
                                _ => None,
                            })
                            .flat_map(|t| crate::tui::markdown::code_blocks(t).into_iter().rev())
                            .nth(back - 1)
                    });
                    match block {
                        Some(code) => match crate::tui::clipboard::copy_to_clipboard(&code) {
                            Ok(_) => out.println(&format!(
                                "[copy] Code block copied ({} lines).",
                                code.lines().count()
                            )),
                            Err(e) => out.eprintln(&format!("[copy] {e}")),
                        },
                        None => out.println("[copy] No code block in the replies yet."),
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
            "/split" => {
                if let ReplOutput::Tui(st) = &out {
                    match crate::tui::split::SplitAction::parse(rest) {
                        Some(a) => {
                            if let Ok(mut g) = st.lock() {
                                let msg = g.split_action(a);
                                g.show_toast(msg, crate::tui::state::ToastKind::Info);
                            }
                        }
                        None => out.println("usage: /split [right|below|next|prev|show|close] — panes beside the conversation (keys: Alt+\\ right, Alt+- below, Alt+O next, Alt+A change, Alt+W close)"),
                    }
                } else {
                    out.println("[split] the split workspace is in the full-screen terminal UI");
                }
            }
            "/dashboard" | "/dash" => {
                if let ReplOutput::Tui(st) = &out {
                    let words: Vec<&str> = rest.split_whitespace().collect();
                    match words.as_slice() {
                        // `/dashboard left off`, `/dashboard right on`: which
                        // side panels show (remembered).
                        [side @ ("left" | "right"), state @ ("on" | "off")] => {
                            let show = *state == "on";
                            let mut p = crate::tui::dash_prefs::load();
                            if *side == "left" {
                                p.hide_left = !show;
                            } else {
                                p.hide_right = !show;
                            }
                            crate::tui::dash_prefs::save(&p);
                            if let Ok(mut g) = st.lock() {
                                g.dash_hits.hide_left = p.hide_left;
                                g.dash_hits.hide_right = p.hide_right;
                                g.touch_transcript();
                            }
                            out.println(&format!(
                                "[dashboard] {side} panel {} — Alt+←/→ resizes, Alt+Z zooms the focused panel",
                                if show { "shown" } else { "hidden" }
                            ));
                        }
                        ["reset"] => {
                            crate::tui::dash_prefs::save(
                                &crate::tui::dash_prefs::DashPrefs::default(),
                            );
                            if let Ok(mut g) = st.lock() {
                                g.split = Default::default();
                                g.dash_hits.left_w = 0;
                                g.dash_hits.right_w = 0;
                                g.dash_hits.hide_left = false;
                                g.dash_hits.hide_right = false;
                                g.dash_hits.zoom = false;
                                g.touch_transcript();
                            }
                            out.println("[dashboard] layout reset");
                        }
                        [] => {
                            if let Ok(mut g) = st.lock() {
                                g.toggle_view_mode();
                            }
                        }
                        _ => out.println("usage: /dashboard [left|right on|off · reset]"),
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
            "/cost" | "/usage"
                if matches!(rest.trim(), "today" | "day" | "week" | "month" | "7" | "30") =>
            {
                let days = match rest.trim() {
                    "today" | "day" => 1,
                    "week" | "7" => 7,
                    _ => 30,
                };
                let history = dcode_ai_runtime::usage_log::log_path()
                    .map(|p| dcode_ai_runtime::usage_log::load(&p))
                    .unwrap_or_default();
                let mut lines = dcode_ai_runtime::usage_log::cost_lines(
                    &history,
                    chrono::Local::now().date_naive(),
                    days,
                );
                let h = &self.runtime.config().harness;
                if h.code_mode
                    && h.code_mode_auto
                    && let Some(why) =
                        dcode_ai_runtime::usage_log::code_mode_verdict(&history, chrono::Utc::now())
                {
                    lines.push(String::new());
                    lines.push(why);
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
            "/cost" | "/usage" => {
                let model = self.runtime.model().to_string();
                let window =
                    dcode_ai_runtime::model_limits::effective_context_window(&model) as u64;
                let ctx = match &out {
                    ReplOutput::Tui(st) => st.lock().map(|g| g.context_tokens).unwrap_or(0),
                    _ => 0,
                };
                let (fresh, output, cache_read, cache_write, est) = self.runtime.usage_breakdown();
                let mut lines = vec![
                    format!("Model:        {model}"),
                    String::new(),
                    "Where this session's tokens went:".into(),
                    format!("  fresh input   {:>9}  full price", fmt_count(fresh)),
                    format!(
                        "  cache reads   {:>9}  ~10% of input price",
                        fmt_count(cache_read)
                    ),
                    format!(
                        "  cache writes  {:>9}  ~125–200% of input price",
                        fmt_count(cache_write)
                    ),
                    format!("  output        {:>9}  ~5× input price", fmt_count(output)),
                ];
                if dcode_ai_common::config::per_token_billing() {
                    lines.push(format!("  estimated     ${est:.4} at API list prices"));
                } else {
                    lines.push(
                        "  billing       your plan — no per-token bill; these tokens count toward its limits"
                            .into(),
                    );
                }
                if let Some(hit) = (cache_read * 100).checked_div(fresh + cache_read + cache_write)
                {
                    lines.push(format!(
                        "  cache hit     {hit:>8}%  of input came from the cache"
                    ));
                }
                let starts = dcode_ai_core::provider::claude_code::SESSION_STARTS
                    .load(std::sync::atomic::Ordering::Relaxed);
                let one_shots = dcode_ai_core::provider::claude_code::ONE_SHOTS
                    .load(std::sync::atomic::Ordering::Relaxed);
                if starts + one_shots > 0 {
                    lines.push(String::new());
                    lines.push(format!(
                        "Claude Code: {starts} conversation process{} started ({} restart{}), {one_shots} one-off call{}",
                        if starts == 1 { "" } else { "es" },
                        starts.saturating_sub(1),
                        if starts == 2 { "" } else { "s" },
                        if one_shots == 1 { "" } else { "s" },
                    ));
                    lines.push("  every restart re-sends the conversation as cache writes".into());
                    use dcode_ai_core::provider::claude_code as cc;
                    let load = |c: &std::sync::atomic::AtomicU64| {
                        c.load(std::sync::atomic::Ordering::Relaxed)
                    };
                    let reasons = [
                        (
                            load(&cc::RESTART_HISTORY),
                            "history rewritten (compaction, rewind)",
                        ),
                        (
                            load(&cc::RESTART_PROMPT),
                            "system prompt, model or effort changed",
                        ),
                        (load(&cc::RESTART_TOOLS), "a tool's schema changed"),
                        (load(&cc::RESTART_ENDED), "the process ended"),
                    ];
                    for (n, why) in reasons.iter().filter(|(n, _)| *n > 0) {
                        lines.push(format!("    {n:>3} × {why}"));
                    }
                }
                let (cold, cold_tokens) = self.runtime.cold_requests();
                if cold > 0 {
                    lines.push(format!(
                        "Cache misses: {cold} request{} re-sent the prompt without the cache ({} tokens written)",
                        if cold == 1 { "" } else { "s" },
                        fmt_count(cold_tokens)
                    ));
                }
                lines.push(String::new());
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
                let mut lines = vec![
                    session_line,
                    format!("Model:       {}", self.runtime.model()),
                    format!("Agent:       @{}", self.agent_profile.label()),
                    format!("Permission:  {:?}", self.runtime.permission_mode()),
                    format!("Children:    {}", snapshot.child_session_ids.len()),
                    format!(
                        "Memory:      {}",
                        self.runtime.memory_store_path().display()
                    ),
                    String::new(),
                ];
                // Across sessions and projects, from the local usage log.
                let history = dcode_ai_runtime::usage_log::log_path()
                    .map(|p| dcode_ai_runtime::usage_log::load(&p))
                    .unwrap_or_default();
                lines.extend(dcode_ai_runtime::usage_log::summary_lines(
                    &history,
                    chrono::Local::now().date_naive(),
                    14,
                ));
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
            "/recap" => {
                let ws = self.runtime.workspace_root().to_path_buf();
                let r = tokio::task::spawn_blocking(move || {
                    let (d, t) = (
                        dcode_ai_cli::recap::today(&ws),
                        dcode_ai_cli::recap::totals(&ws),
                    );
                    let files = dcode_ai_cli::recap::write_card(&ws, &d, &t);
                    (dcode_ai_cli::recap::lines(&d, &t), files)
                })
                .await;
                match r {
                    Ok((lines, files)) => {
                        for l in lines {
                            out.println(&format!("[recap] {l}"));
                        }
                        match files {
                            Ok(files) => {
                                for f in &files {
                                    out.println(&format!("[recap] {}", f.display()));
                                }
                                // The card opens sharp in the picture viewer;
                                // the terminal's half-block preview only when
                                // it can't (no desktop, over SSH).
                                let opened = !rest.split_whitespace().any(|w| w == "--inline")
                                    && files.first().is_some_and(|png| {
                                        std::env::var_os("SSH_CONNECTION").is_none()
                                            && dcode_ai_cli::ffmpeg_setup::open(png).is_ok()
                                    });
                                if opened {
                                    out.println("[recap] opened the card in your picture viewer (/recap --inline shows it here)");
                                } else if let Some(png) = files.first()
                                    && let ReplOutput::Tui(st) = out
                                    && let Some(rows) = super::cmd_work::picture_rows(png, 100)
                                    && let Ok(mut g) = st.lock()
                                {
                                    g.push_block(crate::tui::state::DisplayBlock::Picture {
                                        title: "today".into(),
                                        rows,
                                    });
                                }
                            }
                            Err(e) => out.eprintln(&format!("[recap] {e}")),
                        }
                    }
                    Err(e) => out.eprintln(&format!("[recap] {e}")),
                }
            }
            "/pin" | "/pins" | "/unpin" => {
                let mut pins = self.runtime.pins();
                match command {
                    "/pin" if !rest.trim().is_empty() => {
                        pins.push(rest.trim().to_string());
                        self.runtime.set_pins(&pins);
                        out.println(&format!(
                            "[pin] {} — kept word for word through compaction ({} pinned)",
                            rest.trim(),
                            pins.len()
                        ));
                    }
                    "/unpin" => match rest.trim().parse::<usize>() {
                        Ok(n) if (1..=pins.len()).contains(&n) => {
                            let gone = pins.remove(n - 1);
                            self.runtime.set_pins(&pins);
                            out.println(&format!("[pin] unpinned: {gone}"));
                        }
                        _ => out.println("usage: /unpin <n> (/pins lists them)"),
                    },
                    _ => {
                        if pins.is_empty() {
                            out.println("[pin] nothing pinned — /pin <fact> keeps it through every compaction");
                        }
                        for (i, p) in pins.iter().enumerate() {
                            out.println(&format!("[pin] {}. {p}", i + 1));
                        }
                    }
                }
            }
            "/budget" => {
                let (.., spent) = self.runtime.usage_breakdown();
                let arg = rest.trim().trim_start_matches('$');
                let fmt = |v: f64| {
                    if v < 1.0 {
                        format!("${v:.4}")
                    } else {
                        format!("${v:.2}")
                    }
                };
                match arg {
                    "" => {
                        let b = self.runtime.cost_budget();
                        if b > 0.0 {
                            out.println(&format!(
                                "[budget] {} of {} spent ({:.0}%) — /budget <usd> changes it, /budget off removes it",
                                fmt(spent), fmt(b), spent / b * 100.0
                            ));
                        } else {
                            out.println(&format!("[budget] none — {} spent so far; /budget <usd> sets one (it asks before going over)", fmt(spent)));
                        }
                    }
                    "off" | "none" | "0" => {
                        self.runtime.set_cost_budget(0.0);
                        out.println("[budget] off for this session");
                    }
                    n => match n.parse::<f64>() {
                        Ok(v) if v > 0.0 => {
                            self.runtime.set_cost_budget(v);
                            out.println(&format!(
                                "[budget] {} for this session ({} spent) — a heads-up at 80%, a question at 100%",
                                fmt(v), fmt(spent)
                            ));
                        }
                        _ => out.println("usage: /budget [<usd>|off] — e.g. /budget 5"),
                    },
                }
            }
            "/bugreport" => {
                let log = self.runtime.event_log_path();
                let model = self.runtime.model().to_string();
                match dcode_ai_cli::bugreport::write_report(&dcode_ai_cli::bugreport::Inputs {
                    workspace: self.runtime.workspace_root(),
                    session_id: self.runtime.session_id(),
                    event_log: &log,
                    model: &model,
                }) {
                    Ok(p) => {
                        out.println(&format!("[bugreport] written: {}", p.display()));
                        out.println("[bugreport] secrets are masked, but read it before sharing — then attach it to https://github.com/Dhanuzh/dcode-ai/issues/new");
                    }
                    Err(e) => out.eprintln(&format!("[bugreport] {e}")),
                }
            }
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
