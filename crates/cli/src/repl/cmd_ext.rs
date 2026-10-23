//! Extensions: plugins, MCP, skills, agents, hooks, memory, setup.

use super::*;

impl Repl {
    #[allow(unused_variables, unused_mut)]
    pub(super) async fn cmd_ext(
        &mut self,
        input: &str,
        command: &str,
        rest: &str,
        mut parts: std::str::SplitWhitespace<'_>,
        out: ReplOutput<'_>,
    ) -> anyhow::Result<bool> {
        match command {
            "/reload" => {
                if let Some(tx) = &self.self_cmd_tx {
                    let _ = tx.send(TuiCmd::ReloadExtensions);
                } else {
                    match self.runtime.reload_extensions().await {
                        Ok(m) => out.println(&format!("[reload] {m}")),
                        Err(e) => out.eprintln(&format!("[reload] {e}")),
                    }
                }
            }
            "/add-dir" => {
                let arg = rest.trim();
                if arg.is_empty() {
                    let dirs = dcode_ai_common::workspace_scope::dirs();
                    if dirs.is_empty() {
                        out.println("[add-dir] no extra folders. Usage: /add-dir <path>");
                    } else {
                        out.println("[add-dir] extra working folders:");
                        for d in dirs {
                            out.println(&format!("  {}", d.display()));
                        }
                    }
                    return Ok(true);
                }
                let path = match arg.strip_prefix("~/") {
                    Some(r) => dcode_ai_common::config::home_dir()
                        .map(|h| h.join(r))
                        .unwrap_or_else(|| std::path::PathBuf::from(arg)),
                    None => {
                        let p = std::path::PathBuf::from(arg);
                        if p.is_absolute() {
                            p
                        } else {
                            self.runtime.workspace_root().join(p)
                        }
                    }
                };
                match dcode_ai_common::workspace_scope::add_dir(&path) {
                    Ok(dir) => {
                        self.runtime.refresh_system_prompt();
                        out.println(&format!(
                            "[add-dir] dcode-ai can now read and edit {} (this session; add it to [permissions] additional_directories to keep it)",
                            dir.display()
                        ));
                    }
                    Err(e) => out.eprintln(&format!("[add-dir] {e}")),
                }
            }
            "/skills" => {
                let skills = SkillCatalog::discover(
                    self.runtime.workspace_root(),
                    &self.runtime.config().harness.skill_directories,
                )
                .map_err(anyhow::Error::msg)?;
                if skills.is_empty() {
                    let lines = vec!["No skills discovered.".into()];
                    if let ReplOutput::Tui(st) = &out {
                        if let Ok(mut g) = st.lock() {
                            g.open_info_modal("skills", lines);
                        }
                    } else {
                        out.println("no skills discovered");
                    }
                } else {
                    let lines: Vec<String> = skills.iter().map(|s| s.summary_line()).collect();
                    if let ReplOutput::Tui(st) = &out {
                        if let Ok(mut g) = st.lock() {
                            g.open_info_modal("skills", lines);
                        }
                    } else {
                        for l in &lines {
                            out.println(l);
                        }
                    }
                }
            }
            "/memory" => {
                let user_path = dcode_ai_runtime::memory_tool::user_memory_path();
                let project_path = self.runtime.memory_store_path();
                let arg = rest.trim();
                if arg.is_empty() {
                    let mut lines = vec![format!(
                        "Auto memory: {}  (/memory auto on|off)",
                        if self.runtime.config().memory.auto {
                            "on"
                        } else {
                            "off"
                        }
                    )];
                    for (title, path) in [
                        ("About you (every project)", user_path.clone()),
                        ("This project", Some(project_path.clone())),
                    ] {
                        lines.push(String::new());
                        let notes = match path {
                            Some(p) => MemoryStore::new(&p)
                                .load()
                                .await
                                .map(|m| m.notes)
                                .unwrap_or_default(),
                            None => Vec::new(),
                        };
                        lines.push(format!("{title} — {} note(s)", notes.len()));
                        for note in notes.iter().rev().take(20) {
                            let text = note
                                .content
                                .split_whitespace()
                                .collect::<Vec<_>>()
                                .join(" ");
                            lines.push(format!(
                                "  {}  [{}] {}",
                                note.id,
                                note.kind,
                                crate::tui::render_helpers::truncate_chars(&text, 140)
                            ));
                        }
                    }
                    lines.push(String::new());
                    lines.push(
                        "/memory <text> saves a note · /memory forget <id> removes one".into(),
                    );
                    if let ReplOutput::Tui(st) = &out {
                        if let Ok(mut g) = st.lock() {
                            g.open_info_modal("Memory", lines);
                        }
                    } else {
                        for l in &lines {
                            out.println(l);
                        }
                    }
                } else if let Some(v) = arg.strip_prefix("auto") {
                    let on = match v.trim() {
                        "on" => true,
                        "off" => false,
                        _ => !self.runtime.config().memory.auto,
                    };
                    self.runtime.config_mut().memory.auto = on;
                    let saved = dcode_ai_common::config::DcodeAiConfig::load_global_file()
                        .and_then(|mut c| {
                            c.memory.auto = on;
                            c.save_global()
                        });
                    if let Err(e) = saved {
                        out.eprintln(&format!("warn: failed to save: {e}"));
                    }
                    out.println(&format!(
                        "[memory] auto memory {} — applies from the next session (/new)",
                        if on { "on" } else { "off" }
                    ));
                } else if let Some(id) = arg.strip_prefix("forget ").map(str::trim) {
                    let mut removed = false;
                    for path in [user_path, Some(project_path)].into_iter().flatten() {
                        let store = MemoryStore::new(&path);
                        if let Ok(mut m) = store.load().await {
                            let before = m.notes.len();
                            m.notes.retain(|n| n.id != id);
                            if m.notes.len() != before {
                                removed = true;
                                store.save(&m).await.map_err(anyhow::Error::msg)?;
                            }
                        }
                    }
                    out.println(&if removed {
                        format!("[memory] forgot {id}")
                    } else {
                        format!("[memory] no note with id {id}")
                    });
                } else {
                    self.runtime
                        .append_memory_note("note", Some(arg.to_string()))
                        .await
                        .map_err(anyhow::Error::msg)?;
                    out.println("memory note saved");
                }
            }
            // In the TUI, bare `/setup` opens the checkbox picker.
            "/setup" if rest.trim().is_empty() && matches!(out, ReplOutput::Tui(_)) => {
                let _ = crate::setup_cli::refresh_remote(false).await;
                if let ReplOutput::Tui(st) = &out {
                    let picker =
                        crate::tui::setup_picker::SetupPicker::load(self.runtime.workspace_root());
                    if let Ok(mut g) = st.lock() {
                        g.setup_picker = Some(Box::new(picker));
                    }
                }
            }
            "/setup" => {
                let _ = crate::setup_cli::refresh_remote(false).await;
                let ws = self.runtime.workspace_root().to_path_buf();
                let args = rest.trim().to_string();
                let installs = !args.is_empty();
                if installs {
                    out.println("[setup] installing packs…");
                }
                let lines =
                    tokio::task::spawn_blocking(move || crate::setup_cli::slash(&ws, &args))
                        .await
                        .unwrap_or_else(|e| vec![format!("[setup] {e}")]);
                if installs {
                    if let Some(tx) = &self.self_cmd_tx {
                        let _ = tx.send(TuiCmd::ReloadExtensions);
                    } else if let Err(e) = self.runtime.reload_extensions().await {
                        out.eprintln(&format!("[reload] {e}"));
                    }
                }
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.reload_slash_entries = true;
                        g.open_info_modal("Starter packs", lines);
                    }
                } else {
                    for l in &lines {
                        out.println(l);
                    }
                }
            }
            "/plugins" | "/plugin"
                if rest.trim().is_empty() && matches!(out, ReplOutput::Tui(_)) =>
            {
                // The interactive browser (Discover · Installed · Marketplaces · Errors).
                if let ReplOutput::Tui(st) = &out {
                    let browser = crate::tui::plugin_browser::PluginBrowser::load();
                    if let Ok(mut g) = st.lock() {
                        g.plugin_browser = Some(Box::new(browser));
                    }
                    // Like Claude Code, start with Anthropic's marketplaces
                    // (each added once; a removed one stays removed).
                    let missing = dcode_ai_core::plugin_installer::marketplaces_dir()
                        .map(|d| dcode_ai_core::plugin_installer::missing_default_markets(&d))
                        .unwrap_or_default();
                    if !missing.is_empty() {
                        spawn_plugin_action(
                            Arc::clone(st),
                            crate::tui::plugin_browser::BrowserAction::AddDefaultMarkets(missing),
                            self.self_cmd_tx.clone(),
                        );
                    }
                }
            }
            "/plugins" | "/plugin" => {
                let mut parts = rest.split_whitespace();
                let action = parts.next().unwrap_or("");
                let name = parts.next().unwrap_or("");
                let lines = match (action, name) {
                    ("enable" | "disable", n) if !n.is_empty() => {
                        let enable = action == "enable";
                        match dcode_ai_common::plugins::plugins_dir()
                            .ok_or_else(|| "no home directory".to_string())
                            .and_then(|dir| {
                                dcode_ai_core::plugin_installer::set_enabled(&dir, n, enable)
                            }) {
                            Ok(()) => vec![format!(
                                "Plugin `{n}` {}. Start a new session (/new) to apply.",
                                if enable { "enabled" } else { "disabled" }
                            )],
                            Err(e) => vec![format!("[plugins] {e}")],
                        }
                    }
                    ("", _) => crate::plugin_cli::overview_lines(),
                    _ => vec!["usage: /plugins [enable|disable <name>]".into()],
                };
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_info_modal("Plugins", lines);
                    }
                } else {
                    for l in &lines {
                        out.println(l);
                    }
                }
            }
            "/mcp" => {
                let subcommand = rest.trim();
                let servers = &self.runtime.config().mcp.servers;
                if subcommand == "test" || subcommand.starts_with("test ") {
                    // /mcp test [name] — verify a server connection.
                    let name = subcommand.strip_prefix("test").unwrap_or("").trim();
                    let target = if name.is_empty() {
                        servers.first()
                    } else {
                        servers.iter().find(|s| s.name == name)
                    };
                    match target {
                        Some(server) => {
                            out.println(&format!("[mcp] Testing {} (30s timeout)…", server.name));
                            let manager = Arc::clone(self.runtime.mcp_manager());
                            let srv = server.clone();
                            let test_result = tokio::time::timeout(
                                std::time::Duration::from_secs(30),
                                async move {
                                    dcode_ai_core::tools::mcp::load_mcp_tools(&manager, &[srv])
                                        .await
                                },
                            )
                            .await;
                            match test_result {
                                Ok(Ok(tools)) => {
                                    out.println(&format!(
                                        "[mcp] ✓ {} connected — {} tool(s) discovered",
                                        server.name,
                                        tools.len()
                                    ));
                                    for t in &tools {
                                        out.println(&format!("  • {}", t.definition().name));
                                    }
                                }
                                Ok(Err(e)) => {
                                    out.eprintln(&format!("[mcp] ✗ {} failed: {e}", server.name))
                                }
                                Err(_) => out.eprintln(&format!(
                                    "[mcp] ✗ {} timed out after 30s — try: npm install -g @modelcontextprotocol/server-filesystem",
                                    server.name
                                )),
                            }
                        }
                        None => {
                            if name.is_empty() {
                                out.eprintln("[mcp] No servers configured");
                            } else {
                                out.eprintln(&format!("[mcp] Server '{name}' not found"));
                            }
                        }
                    }
                } else if let ReplOutput::Tui(st) = &out
                    && subcommand.is_empty()
                {
                    // The interactive panel: status, sign-in, reconnect.
                    let panel = crate::tui::mcp_panel::McpPanel::new(self.mcp_rows());
                    if let Ok(mut g) = st.lock() {
                        g.mcp_panel = Some(Box::new(panel));
                    }
                } else {
                    // /mcp — show server status overview.
                    let mut lines: Vec<String> =
                        vec![format!("MCP Servers ({} configured)", servers.len())];
                    lines.push(String::new());
                    if servers.is_empty() {
                        lines.push("No servers configured.".into());
                        lines.push(String::new());
                        lines.push("Add one from a shell:".into());
                        lines.push(
                            "  dcode-ai mcp add files -- npx -y @modelcontextprotocol/server-filesystem ."
                                .into(),
                        );
                        lines.push(
                            "  dcode-ai mcp add docs --transport http https://example.com/mcp"
                                .into(),
                        );
                        lines.push(
                            "or put an `mcpServers` map in .mcp.json (Claude Code format).".into(),
                        );
                    } else {
                        for server in servers {
                            let status = if server.enabled { "●" } else { "○" };
                            let transport = if server.url.is_some() {
                                "http"
                            } else {
                                "stdio"
                            };
                            let origin = if server.origin.is_empty() {
                                "config.toml"
                            } else {
                                server.origin.as_str()
                            };
                            lines.push(format!(
                                "{status} {:<20} [{transport}] {} · {origin}",
                                server.name,
                                if server.enabled {
                                    "enabled"
                                } else {
                                    "disabled"
                                }
                            ));
                            if !server.command.is_empty() {
                                lines.push(format!(
                                    "  cmd: {} {}",
                                    server.command,
                                    server.args.join(" ")
                                ));
                            }
                            if let Some(url) = &server.url {
                                lines.push(format!("  url: {url}"));
                            }
                        }
                        lines.push(String::new());
                        lines.push("Use /mcp test [name] to verify connection".into());
                    }
                    if let ReplOutput::Tui(st) = &out {
                        if let Ok(mut g) = st.lock() {
                            g.open_info_modal("MCP Servers", lines);
                        }
                    } else {
                        for l in &lines {
                            out.println(l);
                        }
                    }
                }
            }
            "/agents" if rest.trim_start().starts_with("new") => {
                // Claude Code's "Generate with Claude": describe the agent,
                // the model writes `agents/<name>.md`.
                let mut desc = rest
                    .trim_start()
                    .trim_start_matches("new")
                    .trim()
                    .to_string();
                let user_scope = desc.starts_with("--user");
                if user_scope {
                    desc = desc.trim_start_matches("--user").trim().to_string();
                }
                if desc.is_empty() {
                    out.println("usage: /agents new [--user] <what the agent should do>");
                    return Ok(true);
                }
                let dir = if user_scope {
                    match dcode_ai_common::config::dcode_ai_home_dir() {
                        Some(h) => h.join("agents"),
                        None => {
                            out.eprintln("[agents] no home directory");
                            return Ok(true);
                        }
                    }
                } else {
                    self.runtime.workspace_root().join(".dcode-ai/agents")
                };
                out.println("[agents] writing the agent definition…");
                let reply = match self
                    .runtime
                    .complete_text(&agent_generation_prompt(&desc))
                    .await
                {
                    Ok(r) => r,
                    Err(e) => {
                        out.eprintln(&format!("[agents] {e}"));
                        return Ok(true);
                    }
                };
                match save_generated_agent(&dir, &reply) {
                    Ok((path, def)) => out.println(&format!(
                        "[agents] created `{}` — {}\n  {}\n  tools: {} · model: {}\nEdit the file to adjust it; the agent is available to spawn_subagent now.",
                        def.name,
                        def.description,
                        path.display(),
                        def.tools
                            .as_ref()
                            .map(|t| t.join(", "))
                            .unwrap_or_else(|| "all".into()),
                        def.model.as_deref().unwrap_or("inherit"),
                    )),
                    Err(e) => out.eprintln(&format!("[agents] {e}")),
                }
            }
            "/agents" => {
                let snapshot = self.runtime.snapshot();
                let defs = dcode_ai_core::agent_defs::discover(
                    self.runtime.workspace_root(),
                    &dcode_ai_common::plugins::enabled_plugins(),
                );
                let mut lines: Vec<String> = vec!["Agents dcode-ai can delegate to:".into()];
                if defs.is_empty() {
                    lines.push("  none defined — add .dcode-ai/agents/<name>.md".into());
                    lines.push(
                        "  (frontmatter: name, description, tools, model; body = instructions)"
                            .into(),
                    );
                } else {
                    for d in &defs {
                        let tools = d
                            .tools
                            .as_ref()
                            .map(|t| format!(" · tools: {}", t.join(", ")))
                            .unwrap_or_default();
                        let model = d
                            .model
                            .as_deref()
                            .map(|m| format!(" · model: {m}"))
                            .unwrap_or_default();
                        lines.push(format!("  {}{model}{tools}", d.name));
                        if !d.description.is_empty() {
                            lines.push(format!("    {}", d.description));
                        }
                    }
                }
                lines.push(String::new());
                lines.push("Sub-agent sessions:".into());
                if snapshot.child_session_ids.is_empty() {
                    lines.push("  none yet".into());
                } else {
                    lines.extend(
                        snapshot
                            .child_session_ids
                            .iter()
                            .map(|id| format!("  {id}")),
                    );
                }
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_info_modal("agents", lines);
                    }
                } else {
                    for l in &lines {
                        out.println(l);
                    }
                }
            }
            "/hooks" => {
                use dcode_ai_common::config::{HOOK_EVENTS, HookCommand};
                let ws = self.runtime.workspace_root().to_path_buf();
                let arg = rest.trim();
                let mut words = arg.split_whitespace();
                let sub = words.next().unwrap_or("");
                // Read from disk so hooks added from here (or in an editor) show.
                let fresh = dcode_ai_common::config::DcodeAiConfig::load_for_workspace(&ws)
                    .map(|c| c.hooks)
                    .unwrap_or_else(|_| self.runtime.config().hooks.clone());
                match sub {
                    "" => {
                        let mut lines = vec!["Hooks (run on lifecycle events):".to_string()];
                        let mut any = false;
                        for event in HOOK_EVENTS {
                            let hooks = fresh.for_event(event);
                            if hooks.is_empty() {
                                continue;
                            }
                            any = true;
                            lines.push(String::new());
                            lines.push(event.to_string());
                            for (n, c) in hooks.iter().enumerate() {
                                let m = c
                                    .matcher
                                    .as_deref()
                                    .map(|m| format!(" [{m}]"))
                                    .unwrap_or_default();
                                let block = if c.blocking { " (blocking)" } else { "" };
                                let origin = if c.origin.is_empty() {
                                    String::new()
                                } else {
                                    format!("  · {}", c.origin)
                                };
                                lines.push(format!("  {}. {}{m}{block}{origin}", n + 1, c.command));
                            }
                        }
                        if !any {
                            lines.push("  (none)".into());
                        }
                        lines.push(String::new());
                        lines.push(
                            "/hooks add <event> [--matcher <tool>] [--blocking] [--user] <command>"
                                .into(),
                        );
                        lines.push("/hooks remove <event> <n> [--user]".into());
                        lines.push(format!("Events: {}", HOOK_EVENTS.join(", ")));
                        if let ReplOutput::Tui(st) = &out {
                            if let Ok(mut g) = st.lock() {
                                g.open_info_modal("Hooks", lines);
                            }
                        } else {
                            for l in &lines {
                                out.println(l);
                            }
                        }
                    }
                    "add" | "remove" => {
                        let event = words.next().unwrap_or("").to_string();
                        let mut matcher = None;
                        let mut blocking = false;
                        let mut user = false;
                        let mut rest_words = Vec::new();
                        while let Some(w) = words.next() {
                            match w {
                                "--matcher" => matcher = words.next().map(String::from),
                                "--blocking" => blocking = true,
                                "--user" => user = true,
                                _ => rest_words.push(w),
                            }
                        }
                        let path = if user {
                            dcode_ai_common::config::global_config_file()
                        } else {
                            Some(dcode_ai_common::config::workspace_config_path(&ws))
                        };
                        let Some(path) = path else {
                            out.eprintln("no home directory");
                            return Ok(true);
                        };
                        let command_text = if sub == "add" {
                            rest_words.join(" ")
                        } else {
                            // `remove <event> <n>`: the n-th hook listed for the event.
                            let n: usize =
                                rest_words.first().and_then(|w| w.parse().ok()).unwrap_or(0);
                            fresh
                                .for_event(&event)
                                .get(n.wrapping_sub(1))
                                .map(|h| h.command.clone())
                                .unwrap_or_default()
                        };
                        if event.is_empty() || command_text.is_empty() {
                            out.eprintln(
                                "usage: /hooks add <event> [--matcher <tool>] [--blocking] [--user] <command> · /hooks remove <event> <n> [--user]",
                            );
                            return Ok(true);
                        }
                        let hook = HookCommand {
                            command: command_text.clone(),
                            matcher,
                            blocking,
                            origin: String::new(),
                        };
                        match dcode_ai_common::config::edit_hook(&path, &event, &hook, sub == "add") {
                            Ok(true) => {
                                out.println(&format!(
                                    "[hooks] {} {event}: {command_text} ({})",
                                    if sub == "add" { "added" } else { "removed" },
                                    path.display()
                                ));
                                if let Some(tx) = &self.self_cmd_tx {
                                    let _ = tx.send(TuiCmd::ReloadExtensions);
                                } else if let Err(e) = self.runtime.reload_extensions().await {
                                    out.eprintln(&format!("[reload] {e}"));
                                }
                            }
                            Ok(false) => out.println(&format!(
                                "[hooks] `{command_text}` is not in {} (plugin hooks are removed with the plugin)",
                                path.display()
                            )),
                            Err(e) => out.eprintln(&format!("[hooks] {e}")),
                        }
                    }
                    other => out.eprintln(&format!(
                        "[hooks] unknown `{other}`; use /hooks, /hooks add …, /hooks remove …"
                    )),
                }
            }
            "/init" => {
                let workspace = self.runtime.workspace_root();
                let dcode_toml = workspace.join(".dcode.toml");
                let instructions_dir = workspace.join(".dcode-ai");
                let instructions_path = instructions_dir.join("instructions.md");

                // Write .dcode.toml only if it doesn't exist yet.
                if dcode_toml.exists() {
                    out.println("[init] .dcode.toml already exists — skipped");
                } else {
                    let toml_content = r#"# dcode-ai project configuration — commit this file to share with teammates.
# All fields are optional; unset fields fall back to the user's global config.

# [model]
# default_model = "claude-opus-4-5"   # override the default model for this project

# [permissions]
# mode = "AcceptEdits"                 # Default | AcceptEdits | Plan | DontAsk | Bypass

# [session]
# agent_profile = "@build"             # default agent profile

# [memory]
# file_path = ".dcode-ai/memory.json"  # project memory store path
"#;
                    match std::fs::write(&dcode_toml, toml_content) {
                        Ok(_) => out.println("[init] Created .dcode.toml"),
                        Err(e) => out.eprintln(&format!("[init] Failed to write .dcode.toml: {e}")),
                    }
                }

                // Write instructions.md scaffold.
                if instructions_path.exists() {
                    out.println("[init] .dcode-ai/instructions.md already exists — skipped");
                } else {
                    let instructions_content = "# Project Instructions for dcode-ai\n\n\
                         Add context here that dcode-ai should always know about this project.\n\n\
                         ## Project Overview\n\n\
                         <!-- Describe what this project does -->\n\n\
                         ## Key Conventions\n\n\
                         - <!-- e.g. \"Always run `cargo test` before committing\" -->\n\n\
                         ## Architecture Notes\n\n\
                         <!-- Important design decisions, patterns, or constraints -->\n"
                        .to_string();
                    if let Err(e) = std::fs::create_dir_all(&instructions_dir) {
                        out.eprintln(&format!("[init] Failed to create .dcode-ai/: {e}"));
                    } else {
                        match std::fs::write(&instructions_path, instructions_content) {
                            Ok(_) => out.println("[init] Created .dcode-ai/instructions.md"),
                            Err(e) => out
                                .eprintln(&format!("[init] Failed to write instructions.md: {e}")),
                        }
                    }
                }

                // Remind about .gitignore.
                let gitignore = workspace.join(".gitignore");
                let gitignore_hint = if gitignore.exists() {
                    let content = std::fs::read_to_string(&gitignore).unwrap_or_default();
                    if content.contains(".dcode-ai/") || content.contains(".dcode-ai") {
                        None
                    } else {
                        Some(true)
                    }
                } else {
                    Some(true)
                };
                if gitignore_hint.is_some() {
                    out.println(
                        "[init] Tip: add `.dcode-ai/` to your .gitignore (sessions + local config)",
                    );
                    out.println(
                        "[init] Tip: commit `.dcode.toml` to share project config with teammates",
                    );
                }

                // Generate AGENTS.md by scanning the repo (Codex/Claude Code
                // `/init` parity). Skipped when one already exists.
                let agents_md = workspace.join("AGENTS.md");
                let workspace = workspace.to_path_buf();
                if agents_md.exists() {
                    out.println("[init] AGENTS.md already exists — skipped");
                } else {
                    out.println("[init] Generating AGENTS.md from the repository…");
                    let prompt = "Explore this repository (key configs, build files, source \
                        layout, tests, CI) and write an AGENTS.md file in the workspace root \
                        for AI coding agents working here. Keep it under ~80 lines and only \
                        include what you can verify from the repo:\n\
                        - one-paragraph project overview\n\
                        - build / test / lint commands (exact invocations)\n\
                        - source layout: what lives where\n\
                        - code conventions actually used in this codebase\n\
                        - anything surprising an agent must know before editing\n\
                        Write the file with write_file, then reply with a one-line summary.";
                    match self.runtime.run_turn(prompt).await {
                        Ok(output) => {
                            if matches!(out, ReplOutput::Stdio) {
                                out.println(&output);
                            }
                            if workspace.join("AGENTS.md").exists() {
                                out.println("[init] Created AGENTS.md");
                            } else {
                                out.println(
                                    "[init] Agent finished without writing AGENTS.md — \
                                     re-run /init or create it manually",
                                );
                            }
                        }
                        Err(e) => out.eprintln(&format!("[init] AGENTS.md generation failed: {e}")),
                    }
                }
            }
            // ── /ide ──────────────────────────────────────────────────────────
            // Pull editor context (selection / open files / active file) into
            // the conversation. dcode reads `.dcode/ide-context.json`, which an
            // editor keybinding writes; if it's absent, fall back to the git
            // working set so /ide is still useful without any editor setup.
            "/ide" => {
                let ws = self.runtime.workspace_root().to_path_buf();
                let ctx_path = ws.join(".dcode").join("ide-context.json");
                let block = match std::fs::read_to_string(&ctx_path)
                    .ok()
                    .and_then(|j| format_ide_context(&j))
                {
                    Some(b) => {
                        out.println("[ide] staged live editor context (.dcode/ide-context.json)");
                        b
                    }
                    None => {
                        let changed = git_changed_files(&ws);
                        if changed.is_empty() {
                            out.println("[ide] no editor bridge and no local changes to attach.");
                            out.println(
                                "  To enable live context, have your editor write .dcode/ide-context.json",
                            );
                            out.println(
                                "  (fields: active_file, selection, open_files, cursor.line).",
                            );
                            return Ok(true);
                        }
                        out.println(&format!(
                            "[ide] no editor bridge — staged your git working set ({} file(s)).",
                            changed.len()
                        ));
                        out.println(
                            "  (For live selection/open files, write .dcode/ide-context.json from your editor.)",
                        );
                        format!(
                            "Files you are currently working on (git working set):\n{}",
                            changed
                                .iter()
                                .map(|f| format!("- {f}"))
                                .collect::<Vec<_>>()
                                .join("\n")
                        )
                    }
                };
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.pending_context.push(block);
                        g.touch_transcript();
                    }
                    out.println("[ide] staged — send a message to use this context");
                } else {
                    let prompt = format!("{block}\n\nAcknowledge the current IDE context briefly.");
                    let _ = self.runtime.run_turn(&prompt).await;
                }
            }
            _ => {}
        }
        Ok(true)
    }
}
