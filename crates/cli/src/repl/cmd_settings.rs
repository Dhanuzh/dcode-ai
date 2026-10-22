//! Model, provider, permission and UI settings.

use super::*;

impl Repl {
    #[allow(unused_variables, unused_mut)]
    pub(super) async fn cmd_settings(
        &mut self,
        input: &str,
        command: &str,
        rest: &str,
        mut parts: std::str::SplitWhitespace<'_>,
        out: ReplOutput<'_>,
    ) -> anyhow::Result<bool> {
        match command {
            "/agent" => {
                if let Some(target) = parts.next() {
                    let target_clean = target.trim_start_matches('@').to_lowercase();
                    let matched = AgentProfile::ALL.iter().find(|p| p.label() == target_clean);
                    if let Some(profile) = matched {
                        self.agent_profile = *profile;
                        self.current_agent_label = format!("@{}", profile.label());
                        self.prompt.set_agent(&self.current_agent_label);
                        if *profile == AgentProfile::Plan {
                            self.runtime.set_permission_mode(PermissionMode::Plan);
                        } else {
                            self.runtime.set_permission_mode(PermissionMode::Default);
                        }
                        if let ReplOutput::Tui(st) = &out
                            && let Ok(mut g) = st.lock()
                        {
                            g.set_agent_profile(&self.current_agent_label);
                            g.set_permission_mode(&format!("{:?}", self.runtime.permission_mode()));
                        }
                        out.println(&format!("Switched to @{} mode", profile.label()));
                    } else {
                        out.println(&format!("Unknown agent profile: {}", target));
                        out.println(&format!(
                            "Available: {}",
                            AgentProfile::ALL
                                .iter()
                                .map(|p| p.label())
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                    }
                } else if let ReplOutput::Tui(st) = &out {
                    let current_idx = AgentProfile::ALL
                        .iter()
                        .position(|p| *p == self.agent_profile)
                        .unwrap_or(0);
                    if let Ok(mut g) = st.lock() {
                        g.open_agent_picker(current_idx);
                    }
                } else {
                    out.println(&format!("Current agent: @{}", self.agent_profile.label()));
                    out.println("Available profiles:");
                    for profile in AgentProfile::ALL {
                        let marker = if profile == self.agent_profile {
                            " *"
                        } else {
                            ""
                        };
                        out.println(&format!("  @{}{}", profile.label(), marker));
                    }
                }
            }
            "/vim" => {
                let on = !self.runtime.config().ui.vim_mode;
                self.runtime.config_mut().ui.vim_mode = on;
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.vim_enabled = on;
                    g.vim_normal = false;
                    g.vim_pending = None;
                }
                let saved =
                    dcode_ai_common::config::DcodeAiConfig::load_global_file().and_then(|mut c| {
                        c.ui.vim_mode = on;
                        c.save_global()
                    });
                if let Err(e) = saved {
                    out.eprintln(&format!("warn: failed to save vim mode: {e}"));
                }
                out.println(if on {
                    "[vim] vim mode on — Esc for NORMAL, i for INSERT (/vim to turn off)"
                } else {
                    "[vim] vim mode off"
                });
            }
            "/permissions" => {
                let arg = rest.trim();
                let mut words = arg.split_whitespace();
                let first = words.next().unwrap_or("");
                match first {
                    "" => {
                        let mut lines = vec![
                            format!("Mode: {:?}", self.runtime.permission_mode()),
                            String::new(),
                        ];
                        let rules = self.runtime.permission_rules();
                        for kind in ["allow", "ask", "deny"] {
                            let of: Vec<_> = rules.iter().filter(|r| r.0 == kind).collect();
                            lines.push(format!("{} ({})", kind.to_uppercase(), of.len()));
                            for (_, rule, origin) in of {
                                lines.push(format!("  {rule}    · {origin}"));
                            }
                            lines.push(String::new());
                        }
                        lines.push(
                            "/permissions allow|ask|deny <rule> [--user] · remove <rule> · mode · folders · forget"
                                .into(),
                        );
                        lines.push(
                            "Rules look like `execute_bash:cargo *`, `edit_file`, `Bash(git:*)`; saved to .dcode-ai/config.local.toml (or ~/.dcode-ai/config.toml with --user)."
                                .into(),
                        );
                        if let ReplOutput::Tui(st) = &out {
                            if let Ok(mut g) = st.lock() {
                                g.open_info_modal("Permissions", lines);
                            }
                        } else {
                            for l in &lines {
                                out.println(l);
                            }
                        }
                    }
                    "mode" => {
                        if let ReplOutput::Tui(st) = &out {
                            let current_idx = permission_mode_index(self.runtime.permission_mode());
                            if let Ok(mut g) = st.lock() {
                                g.open_permission_picker(current_idx);
                            }
                        } else {
                            out.println(&format!(
                                "permission_mode: {:?}",
                                self.runtime.permission_mode()
                            ));
                        }
                    }
                    "allow" | "ask" | "deny" | "remove" => {
                        let user = arg.split_whitespace().any(|w| w == "--user");
                        let rule = words
                            .filter(|w| *w != "--user")
                            .collect::<Vec<_>>()
                            .join(" ");
                        if rule.is_empty() {
                            out.eprintln(
                                "usage: /permissions allow|ask|deny|remove <rule> [--user]",
                            );
                            return Ok(true);
                        }
                        // Claude Code names (`Bash(git:*)`, `Read`) map to dcode-ai rules.
                        let rules = crate::cli_tool_rules(std::slice::from_ref(&rule));
                        let path = if user {
                            match dcode_ai_common::config::global_config_file() {
                                Some(p) => p,
                                None => {
                                    out.eprintln("no home directory");
                                    return Ok(true);
                                }
                            }
                        } else {
                            dcode_ai_common::config::workspace_config_path(
                                self.runtime.workspace_root(),
                            )
                        };
                        let kinds: &[&str] = if first == "remove" {
                            &["allow", "ask", "deny"]
                        } else {
                            &[first]
                        };
                        let mut msg = Vec::new();
                        for kind in kinds.iter().flat_map(|k| rules.iter().map(move |r| (k, r))) {
                            let (kind, rule) = kind;
                            let add = first != "remove";
                            self.runtime.set_permission_rule(kind, rule, add);
                            match dcode_ai_common::config::edit_permission_rule(
                                &path, kind, rule, add,
                            ) {
                                Ok(true) => msg.push(format!(
                                    "{} {kind} `{rule}` ({})",
                                    if add { "added" } else { "removed" },
                                    path.display()
                                )),
                                Ok(false) => {}
                                Err(e) => msg.push(format!("error: {e}")),
                            }
                        }
                        if msg.is_empty() {
                            msg.push(if first == "remove" {
                                format!("`{rule}` was not saved in {}", path.display())
                            } else {
                                format!("`{rule}` already in {first}")
                            });
                        }
                        for m in msg {
                            out.println(&format!("[permissions] {m}"));
                        }
                    }
                    "forget" => {
                        match dcode_ai_common::folders::forget_permission(
                            self.runtime.workspace_root(),
                        ) {
                            Ok(true) => out.println(
                                "[permissions] forgot this folder's choice — you'll be asked at the next start",
                            ),
                            Ok(false) => out.println(
                                "[permissions] no choice was remembered for this folder",
                            ),
                            Err(e) => out.eprintln(&format!("[permissions] {e}")),
                        }
                    }
                    "folders" => {
                        let all = dcode_ai_common::folders::remembered_permissions();
                        let mut lines = vec![format!(
                            "Folders with a remembered permission choice ({}):",
                            all.len()
                        )];
                        for (folder, choice) in all {
                            lines.push(format!("  {choice:<13} {folder}"));
                        }
                        lines.push(String::new());
                        lines
                            .push("/permissions forget clears the current folder's choice.".into());
                        if let ReplOutput::Tui(st) = &out {
                            if let Ok(mut g) = st.lock() {
                                g.open_info_modal("Remembered folders", lines);
                            }
                        } else {
                            for l in &lines {
                                out.println(l);
                            }
                        }
                    }
                    mode => {
                        if let Some(parsed_mode) = parse_permission_mode(mode) {
                            self.runtime.set_permission_mode(parsed_mode);
                            if let ReplOutput::Tui(st) = out
                                && let Ok(mut g) = st.lock()
                            {
                                g.set_permission_mode(&format!("{parsed_mode:?}"));
                            }
                            out.println(&format!("permission mode set to {parsed_mode:?}"));
                        } else {
                            out.println(
                                "usage: /permissions [mode | <mode> | allow|ask|deny|remove <rule>]; modes: default, plan, accept-edits, auto, dont-ask, bypass-permissions",
                            );
                        }
                    }
                }
            }
            "/permission-bypass" => {
                let sub = parts.next().unwrap_or("").trim();
                let target = match sub.to_ascii_lowercase().as_str() {
                    "" | "toggle" => {
                        if self.runtime.permission_mode() == PermissionMode::BypassPermissions {
                            PermissionMode::Default
                        } else {
                            PermissionMode::BypassPermissions
                        }
                    }
                    "on" | "enable" | "yes" | "1" => PermissionMode::BypassPermissions,
                    "off" | "disable" | "no" | "0" => PermissionMode::Default,
                    _ => {
                        out.println(
                            "usage: /permission-bypass [on|off|toggle] — bypass auto-allows file tools; first bash asks once",
                        );
                        return Ok(true);
                    }
                };
                self.runtime.set_permission_mode(target);
                if let ReplOutput::Tui(st) = out
                    && let Ok(mut g) = st.lock()
                {
                    g.set_permission_mode(&format!("{target:?}"));
                }
                out.println(&format!("permission mode set to {target:?}"));
            }
            "/models" => {
                let catalog = dcode_ai_runtime::model_limits_api::fetch_provider_model_ids(
                    self.runtime.config(),
                )
                .await;
                let (provider_models, catalog_error) = match catalog {
                    Ok(models) => (models, None),
                    Err(error) => (Vec::new(), Some(error.to_string())),
                };
                let auth = dcode_ai_common::auth::AuthStore::load().unwrap_or_default();
                let connected = active_provider_connected(self.runtime.config(), &auth);
                if let ReplOutput::Tui(st) = &out {
                    let entries =
                        build_model_picker_entries(self.runtime.config(), &provider_models);
                    if let Ok(mut g) = st.lock() {
                        // An unavailable live catalog is non-fatal in the picker.
                        // Keep it empty; never substitute a built-in model list.
                        g.open_model_picker(entries);
                    }
                } else {
                    let provider = self.runtime.config().provider.default;
                    out.println(&format!(
                        "default_provider={} default_model={} thinking={} budget={}",
                        provider_label(self.runtime.config(), provider),
                        self.runtime.config().model.default_model,
                        self.runtime.config().model.enable_thinking,
                        self.runtime.config().model.thinking_budget
                    ));
                    for provider in dcode_ai_common::config::ProviderKind::ALL {
                        out.println(&format!(
                            "  {} -> {} ({})",
                            provider_label(self.runtime.config(), provider),
                            self.runtime.config().provider.model_for(provider),
                            self.runtime.config().provider.base_url_for(provider)
                        ));
                    }
                    if !connected {
                        out.println(
                            "no provider connected for current selection; run /connect or /login",
                        );
                    }
                    if let Some(error) = catalog_error {
                        out.eprintln(&format!("[models] {error}"));
                    } else {
                        out.println(&format!(
                            "active provider models ({}) :",
                            active_surface_label(self.runtime.config())
                        ));
                        for model in &provider_models {
                            out.println(&format!("  - {model}"));
                        }
                    }
                    for (alias, target) in &self.runtime.config().model.aliases {
                        out.println(&format!("  {alias} -> {target}"));
                    }
                }
            }
            "/config" => {
                let config = self.runtime.config();
                let lines = vec![
                    format!("Provider:    {}", config.provider.default.display_name()),
                    format!("Model:       {}", self.runtime.model()),
                    format!("Permission:  {:?}", self.runtime.permission_mode()),
                    format!(
                        "Memory:      {}",
                        self.runtime.memory_store_path().display()
                    ),
                    format!("Editor:      {}", config.effective_editor_command()),
                    format!(
                        "Thinking:    {} (budget: {})",
                        config.model.enable_thinking, config.model.thinking_budget
                    ),
                    format!("Max tokens:  {}", config.model.max_tokens),
                    String::new(),
                    "Provider endpoints:".into(),
                    format!(
                        "  Claude:      {} ({:?} backend)",
                        config.provider.base_url_for(ProviderKind::Anthropic),
                        config.provider.anthropic.backend
                    ),
                    format!(
                        "  Copilot:     {}",
                        config.provider.base_url_for(ProviderKind::Copilot)
                    ),
                ];
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_info_modal("config", lines);
                    }
                } else {
                    for l in &lines {
                        out.println(l);
                    }
                }
            }
            "/login" | "/connect" => {
                let provider_hint = rest.trim();
                if !provider_hint.is_empty() {
                    if let Some(oauth_provider) = parse_oauth_provider(provider_hint) {
                        if matches!(oauth_provider, OAuthProvider::Anthropic)
                            && self.runtime.config().provider.anthropic.backend
                                == dcode_ai_common::config::AnthropicBackend::ClaudeCode
                        {
                            // The Claude account login belongs to the `claude`
                            // binary itself; dcode-ai just switches to it.
                            if dcode_ai_common::provider_runtime::claude_code_signed_in() {
                                self.apply_provider_after_oauth_login(
                                    OAuthProvider::Anthropic,
                                    &out,
                                )
                                .await?;
                                out.println("[login] using your Claude account via Claude Code ✓");
                            } else if dcode_ai_common::provider_runtime::has_claude_cli() {
                                out.println("[login] sign in to Claude Code first: run `! claude auth login`, then `/login anthropic` again");
                            } else {
                                out.println("[login] Claude account sign-in needs Claude Code (`claude`): https://claude.com/claude-code");
                            }
                            return Ok(true);
                        }
                        if let ReplOutput::Tui(st) = &out
                            && matches!(oauth_provider, OAuthProvider::Anthropic)
                        {
                            let prompt =
                                match crate::oauth_login::begin_anthropic_login_prompt().await {
                                    Ok(prompt) => prompt,
                                    Err(e) => {
                                        out.println(&format!("[login] {e}"));
                                        return Ok(true);
                                    }
                                };
                            let opened =
                                crate::oauth_login::try_open_browser(&prompt.authorization_url);
                            if let Ok(mut g) = st.lock() {
                                g.open_anthropic_oauth_modal(
                                    prompt.authorization_url.clone(),
                                    prompt.code_verifier.clone(),
                                );
                            }
                            if opened {
                                out.println(
                                    "[login] starting anthropic OAuth flow (browser opened)",
                                );
                            } else {
                                out.println(
                                    "[login] starting anthropic OAuth flow (browser open failed; use popup URL)",
                                );
                            }
                            return Ok(true);
                        }
                        out.println(&format!("[login] starting {} OAuth flow…", provider_hint));
                        match crate::oauth_login::login_with_output(oauth_provider, |line| {
                            out.println(line)
                        })
                        .await
                        {
                            Ok(()) => {
                                self.apply_provider_after_oauth_login(oauth_provider, &out)
                                    .await?;
                            }
                            Err(e) => {
                                out.eprintln(&format!("[login] {e}"));
                            }
                        }
                    } else {
                        out.eprintln(
                            "unknown provider; sign in with: /login anthropic | /login copilot",
                        );
                    }
                    return Ok(true);
                }
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_connect_modal();
                    }
                    out.println("[connect] Choose a provider (↑↓ · Enter · type to search · Esc).");
                } else {
                    out.println("Connect an LLM provider (non-TUI):");
                    out.println("  dcode-ai login <anthropic|openai|codex|copilot|antigravity>");
                    out.println("  /apikey <openrouter|opencodezen>");
                    out.println(
                        "  /provider <openai|copilot|anthropic|openrouter|antigravity|opencodezen>",
                    );
                    out.println(
                        "  /model <name>                 — set model after switching provider",
                    );
                    out.println(&format!(
                        "  current: {} → {}",
                        self.runtime.config().provider.default.display_name(),
                        self.runtime.model()
                    ));
                }
            }
            "/logout" => {
                dcode_ai_common::provider_runtime::forget_claude_signin();
                let target = rest.trim();
                let parsed = if target.is_empty() {
                    Some(LogoutTarget::All)
                } else {
                    parse_logout_target(target)
                };
                if let Some(target) = parsed {
                    match crate::oauth_login::logout_with_output(target, |line| out.println(line)) {
                        Ok(()) => out.println("[logout] done"),
                        Err(e) => out.eprintln(&format!("[logout] {e}")),
                    }
                } else {
                    out.eprintln(
                        "usage: /logout [anthropic|openai|copilot|antigravity|opencodezen|all]",
                    );
                }
            }
            "/auth" => {
                let store = dcode_ai_common::auth::AuthStore::load().unwrap_or_default();
                out.println("Auth status:");
                out.println(&format!(
                    "  anthropic:   {}",
                    if dcode_ai_common::provider_runtime::anthropic_signed_in(
                        &self.runtime.config().provider.anthropic,
                    ) {
                        match self.runtime.config().provider.anthropic.backend {
                            dcode_ai_common::config::AnthropicBackend::ClaudeCode => {
                                "signed in (Claude account via Claude Code)"
                            }
                            dcode_ai_common::config::AnthropicBackend::Api => {
                                "signed in (API, OAuth via ant)"
                            }
                        }
                    } else if store.anthropic.is_some() {
                        "legacy API key"
                    } else {
                        "not logged in"
                    }
                ));
                out.println(&format!(
                    "  copilot:     {}",
                    if store.copilot.is_some() {
                        "logged in"
                    } else {
                        "not logged in"
                    }
                ));
            }
            "/sidebar" => {
                let _ = rest;
                out.println("[sidebar] removed in fullscreen TUI. Use /status, /config, /sessions, and /mcp for context.");
            }
            "/quiet" => {
                let _ = rest;
                let mut cfg = self.runtime.config().clone();
                let new_val = !cfg.ui.quiet_startup;
                cfg.ui.quiet_startup = new_val;
                match self.runtime.apply_dcode_ai_config(cfg) {
                    Ok(()) => {
                        if let Err(e) = self.runtime.config().save_global() {
                            out.eprintln(&format!(
                                "[quiet] toggled for this session, but global save failed: {e}"
                            ));
                        }
                        if let ReplOutput::Tui(st) = &out
                            && let Ok(mut g) = st.lock()
                        {
                            g.quiet_startup = new_val;
                        }
                        out.println(if new_val {
                            "[quiet] Startup notices hidden (MCP status + permissions). Applies from the next launch."
                        } else {
                            "[quiet] Startup notices will be shown on the next launch."
                        });
                    }
                    Err(e) => out.eprintln(&format!("[quiet] failed to apply: {e}")),
                }
            }
            "/settings" => {
                let lines = vec![
                    "Workspace settings (.dcode-ai/config.local.toml):".into(),
                    String::new(),
                    format!(
                        "  Provider:    {}",
                        self.runtime.config().provider.default.display_name()
                    ),
                    format!("  Model:       {}", self.runtime.model()),
                    format!(
                        "  Editor:      {}",
                        self.runtime.config().effective_editor_command()
                    ),
                    format!("  Permission:  {:?}", self.runtime.permission_mode()),
                    String::new(),
                    "Commands:".into(),
                    "  /connect           OpenCode-style provider picker".into(),
                    "  /login             Alias of /connect".into(),
                    "  /logout [target]   Logout provider auth".into(),
                    "  /auth              Show auth/login status".into(),
                    "  /context           Session context with recent turns".into(),
                    "  /models            Browse and select models".into(),
                    "  /provider [name]   Provider connect/switch".into(),
                    "  /session-name      Show/set manual session name".into(),
                    "  /editor [seed]     Open external editor".into(),
                    "  /set-editor <cmd>  Persist editor command".into(),
                ];
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_info_modal("settings", lines);
                    }
                } else {
                    for l in &lines {
                        out.println(l);
                    }
                }
            }
            "/theme" => {
                use crate::tui::theme;
                let target = rest.trim();
                if target.is_empty() {
                    let active = theme::current();
                    // The picker offers Claude's six; others stay reachable by
                    // name (and the active one is always listed).
                    let mut names: Vec<String> = theme::CLAUDE_THEMES
                        .iter()
                        .map(|t| t.name.to_string())
                        .collect();
                    if !names.iter().any(|n| n == active.name) {
                        names.push(active.name.to_string());
                    }
                    if let ReplOutput::Tui(st) = out {
                        let cur_idx = names.iter().position(|n| n == active.name).unwrap_or(0);
                        if let Ok(mut g) = st.lock() {
                            g.open_theme_picker(names, cur_idx);
                        }
                    } else {
                        let names: Vec<String> = theme::ALL_THEMES
                            .iter()
                            .map(|t| t.name.to_string())
                            .collect();
                        out.println(&format!("current theme: {}", active.name));
                        out.println(&format!("available: {}", names.join(", ")));
                        out.println("usage: /theme <name>    (persists to workspace config)");
                    }
                } else {
                    let applied = theme::set_by_name(Some(target));
                    let matched = applied.name.eq_ignore_ascii_case(
                        target.trim().trim_start_matches('-').trim_end_matches('-'),
                    ) || theme::ALL_THEMES
                        .iter()
                        .any(|t| t.name == applied.name && target.eq_ignore_ascii_case(t.name));
                    if !matched
                        && !target.eq_ignore_ascii_case("default")
                        && !target.eq_ignore_ascii_case("tokyo")
                        && !target.eq_ignore_ascii_case("tokyo-night")
                        && !target.eq_ignore_ascii_case("mocha")
                        && !target.eq_ignore_ascii_case("gruv")
                        && !target.eq_ignore_ascii_case("dcode")
                        && !target.eq_ignore_ascii_case("dark")
                    {
                        out.eprintln(&format!(
                            "unknown theme '{target}'; falling back to '{}'",
                            applied.name
                        ));
                    }
                    self.runtime.config_mut().ui.theme = Some(applied.name.to_string());
                    if let Err(e) = persist_theme_globally(applied.name) {
                        out.eprintln(&format!("warn: failed to persist theme: {e}"));
                    }
                    out.println(&format!("[theme] set to {}", applied.name));
                }
            }
            "/title" => {
                let t = rest.trim();
                if t.is_empty() {
                    out.println("usage: /title <text>   (sets the terminal window title)");
                } else {
                    let _ =
                        crossterm::execute!(std::io::stdout(), crossterm::terminal::SetTitle(t));
                    out.println(&format!("[title] terminal title set to: {t}"));
                }
            }
            "/raw" => {
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.transcript_overlay_open = true;
                        g.transcript_overlay_raw = true;
                        g.transcript_overlay_scroll = usize::MAX;
                    }
                    out.println("[raw] raw scrollback view — q to close, r to toggle styling");
                } else {
                    out.println("[raw] available in TUI mode");
                }
            }
            "/mention" => {
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.set_input_text("@");
                    }
                    out.println("[mention] type a path after @ to attach a file");
                } else {
                    out.println("[mention] in TUI, type @ then a path to attach a file");
                }
            }
            "/output-style" | "/output-styles" => {
                let ws = self.runtime.workspace_root().to_path_buf();
                let styles = dcode_ai_common::output_styles::discover(&ws);
                let current = self
                    .runtime
                    .config()
                    .ui
                    .output_style
                    .clone()
                    .unwrap_or_else(|| "default".into())
                    .to_ascii_lowercase();
                let target = rest.trim().to_ascii_lowercase();
                if target.is_empty() {
                    let mut lines = vec![
                        "Choose how dcode-ai talks to you:".to_string(),
                        String::new(),
                    ];
                    for st in &styles {
                        let mark = if st.key == current { "✔" } else { " " };
                        let origin = if st.source.is_some() { " (custom)" } else { "" };
                        lines.push(format!("{mark} {:<12} {}{origin}", st.key, st.description));
                    }
                    lines.push(String::new());
                    lines.push("/output-style <name> to switch · custom styles: .claude/output-styles/*.md".into());
                    if let ReplOutput::Tui(st) = &out {
                        if let Ok(mut g) = st.lock() {
                            g.open_info_modal("Output style", lines);
                        }
                    } else {
                        for l in lines {
                            out.println(&l);
                        }
                    }
                } else if let Some(style) = styles.iter().find(|st| st.key == target) {
                    let value = (style.key != "default").then(|| style.key.clone());
                    self.runtime.config_mut().ui.output_style = value.clone();
                    self.runtime.refresh_system_prompt();
                    // Like the theme, the style follows you across projects.
                    let saved = dcode_ai_common::config::DcodeAiConfig::load_global_file()
                        .and_then(|mut g| {
                            g.ui.output_style = value;
                            g.save_global()
                        });
                    if let Err(e) = saved {
                        out.eprintln(&format!("warn: failed to save output style: {e}"));
                    }
                    out.println(&format!(
                        "[output-style] {} — {}",
                        style.name, style.description
                    ));
                } else {
                    out.eprintln(&format!(
                        "[output-style] unknown '{target}'; choose {}",
                        styles
                            .iter()
                            .map(|s| s.key.as_str())
                            .collect::<Vec<_>>()
                            .join("|")
                    ));
                }
            }
            "/personality" => {
                let target = rest.trim().to_ascii_lowercase();
                let valid = ["concise", "friendly", "technical", "default"];
                if target.is_empty() {
                    let cur = self
                        .runtime
                        .config()
                        .ui
                        .personality
                        .clone()
                        .unwrap_or_else(|| "default".into());
                    out.println(&format!("[personality] current: {cur}"));
                    out.println("usage: /personality <concise|friendly|technical|default>");
                } else if valid.contains(&target.as_str()) {
                    self.runtime.config_mut().ui.personality = if target == "default" {
                        None
                    } else {
                        Some(target.clone())
                    };
                    self.runtime.refresh_system_prompt();
                    if let Err(e) = self
                        .runtime
                        .config()
                        .save_workspace_file(self.runtime.workspace_root())
                    {
                        out.eprintln(&format!("warn: failed to persist personality: {e}"));
                    }
                    out.println(&format!("[personality] set to {target}"));
                } else {
                    out.eprintln(&format!(
                        "[personality] unknown '{target}'; choose concise|friendly|technical|default"
                    ));
                }
            }
            "/keymap" => {
                // /keymap                → list global actions + custom bindings
                // /keymap <action> <key> → bind a key (e.g. /keymap palette ctrl+b)
                let args: Vec<&str> = rest.split_whitespace().collect();
                let actions = [
                    "palette",
                    "search",
                    "history",
                    "pin",
                    "expand",
                    "subagents",
                    "thinking",
                    "clear",
                ];
                if args.is_empty() {
                    out.println(&format!(
                        "[keymap] remappable actions: {}",
                        actions.join(", ")
                    ));
                    let custom = &self.runtime.config().ui.keymap;
                    if custom.is_empty() {
                        out.println(
                            "usage: /keymap <action> <key>  (e.g. /keymap palette ctrl+b) — rebinds the action to that key",
                        );
                    } else {
                        out.println("[keymap] custom bindings:");
                        for (a, k) in custom {
                            out.println(&format!("  {a} = {k}"));
                        }
                    }
                } else if args.len() == 2 {
                    let action = args[0].to_ascii_lowercase();
                    let key = args[1].to_ascii_lowercase();
                    if !actions.contains(&action.as_str()) {
                        out.eprintln(&format!(
                            "[keymap] unknown action '{action}'; choose one of: {}",
                            actions.join(", ")
                        ));
                    } else if crate::tui::keymap::parse_key_combo(&key).is_none() {
                        out.eprintln(&format!(
                            "[keymap] could not parse key '{key}' (try ctrl+b, alt+enter, f5)"
                        ));
                    } else {
                        self.runtime
                            .config_mut()
                            .ui
                            .keymap
                            .insert(action.clone(), key.clone());
                        let bindings = crate::tui::keymap::build_key_bindings(
                            &self.runtime.config().ui.keymap,
                        );
                        if let ReplOutput::Tui(st) = &out
                            && let Ok(mut g) = st.lock()
                        {
                            g.key_bindings = bindings;
                        }
                        if let Err(e) = self
                            .runtime
                            .config()
                            .save_workspace_file(self.runtime.workspace_root())
                        {
                            out.eprintln(&format!("warn: failed to persist keymap: {e}"));
                        }
                        out.println(&format!(
                            "[keymap] rebound {action} → {key} (its default key is now inactive)"
                        ));
                    }
                } else {
                    out.eprintln("[keymap] usage: /keymap [<action> <key>]");
                }
            }
            "/statusline" => {
                let item = rest.trim().to_ascii_lowercase();
                let valid = ["agent", "effort", "time", "context", "model"];
                if item.is_empty() {
                    let hidden = &self.runtime.config().ui.statusline_hidden;
                    let status: Vec<String> = valid
                        .iter()
                        .map(|&k| {
                            let on = !hidden.iter().any(|h| h == k);
                            format!("{k}={}", if on { "on" } else { "off" })
                        })
                        .collect();
                    out.println(&format!("[statusline] {}", status.join("  ")));
                    out.println(
                        "usage: /statusline <agent|effort|time|context|model> to toggle an item",
                    );
                } else if valid.contains(&item.as_str()) {
                    let now_hidden = {
                        let hidden = &mut self.runtime.config_mut().ui.statusline_hidden;
                        if let Some(pos) = hidden.iter().position(|h| h == &item) {
                            hidden.remove(pos);
                            false
                        } else {
                            hidden.push(item.clone());
                            true
                        }
                    };
                    let new_hidden = self.runtime.config().ui.statusline_hidden.clone();
                    if let ReplOutput::Tui(st) = &out
                        && let Ok(mut g) = st.lock()
                    {
                        g.statusline_hidden = new_hidden;
                        g.touch_transcript();
                    }
                    if let Err(e) = self
                        .runtime
                        .config()
                        .save_workspace_file(self.runtime.workspace_root())
                    {
                        out.eprintln(&format!("warn: failed to persist statusline: {e}"));
                    }
                    out.println(&format!(
                        "[statusline] {item} {}",
                        if now_hidden { "hidden" } else { "shown" }
                    ));
                } else {
                    out.eprintln(&format!(
                        "[statusline] unknown '{item}'; choose agent|effort|time|context|model"
                    ));
                }
            }
            "/provider" => {
                let rest = rest.trim();
                if rest.is_empty() {
                    if let ReplOutput::Tui(st) = &out {
                        if let Ok(mut g) = st.lock() {
                            g.open_connect_modal();
                        }
                        out.println("[provider] connect/login popup opened (↑↓ · Enter · Esc)");
                    } else {
                        out.println(&format!(
                            "current default provider: {} (model {})",
                            self.runtime.config().provider.default.display_name(),
                            self.runtime.model()
                        ));
                        out.println("usage: /provider <anthropic|copilot>");
                    }
                } else if rest.eq_ignore_ascii_case("copilot")
                    || rest.eq_ignore_ascii_case("github")
                {
                    self.apply_copilot_provider_in_session(out).await?;
                } else if let Some(p) = ProviderKind::from_cli_name(rest)
                    .or_else(|| ProviderKind::parse_display_name(rest))
                {
                    self.apply_provider_in_session(p, out).await?;
                } else {
                    out.eprintln("unknown provider; try: anthropic, copilot");
                }
            }
            "/apikey" => {
                let _ = rest;
                out.eprintln(
                    "API keys are not used. Sign in instead: /login anthropic (your Claude account) or /login copilot",
                );
            }
            "/editor" => {
                let seed = if rest.is_empty() { None } else { Some(rest) };
                match self.open_external_editor(seed).await {
                    Some(text) if !text.is_empty() => {
                        if let ReplOutput::Tui(st) = out {
                            if let Ok(mut g) = st.lock() {
                                g.set_input_text(text);
                            }
                            out.println("[editor] loaded into composer — press Enter to send");
                        } else {
                            let expanded = match expand_at_file_mentions_default(
                                &text,
                                self.runtime.workspace_root(),
                            ) {
                                Ok(s) => s,
                                Err(e) => {
                                    out.eprintln(&format!("file mention expansion: {e}"));
                                    text
                                }
                            };
                            match self.runtime.run_turn(&expanded).await {
                                Ok(o) => println!("{o}"),
                                Err(e) => eprintln!("error: {e}"),
                            }
                        }
                    }
                    Some(_) => out.println("[editor] empty buffer — nothing sent"),
                    None => {}
                }
            }
            "/set-editor" => {
                let cmd = rest.trim();
                if cmd.is_empty() {
                    out.println(&format!(
                        "usage: /set-editor <command>  (effective: {})",
                        self.runtime.config().effective_editor_command()
                    ));
                } else {
                    self.runtime.config_mut().ui.editor = Some(cmd.to_string());
                    match self
                        .runtime
                        .config()
                        .save_workspace_file(self.runtime.workspace_root())
                    {
                        Ok(()) => out.println(&format!(
                            "[set-editor] saved `{cmd}` to .dcode-ai/config.local.toml"
                        )),
                        Err(e) => out.eprintln(&format!("[set-editor] save failed: {e}")),
                    }
                }
            }
            "/auto-answer" => {
                let from_tui = if let ReplOutput::Tui(st) = &out {
                    st.lock()
                        .ok()
                        .and_then(|g| g.active_question.as_ref().map(|q| q.question_id.clone()))
                } else {
                    None
                };
                let ok = if let Some(qid) = from_tui {
                    self.runtime
                        .submit_question_answer(&qid, QuestionSelection::Suggested)
                } else {
                    self.runtime.submit_suggested_answer()
                };
                if ok {
                    out.println("accepted suggested answer for pending question");
                } else {
                    out.eprintln(
                        "no pending interactive question to auto-answer (use when ask_question is waiting)",
                    );
                }
            }
            "/thinking" => {
                let mut cfg = self.runtime.config().clone();
                cfg.model.enable_thinking = !cfg.model.enable_thinking;
                let new_state = cfg.model.enable_thinking;
                match self.runtime.apply_dcode_ai_config(cfg) {
                    Ok(()) => {
                        if let Err(e) = self
                            .runtime
                            .config()
                            .save_workspace_file(self.runtime.workspace_root())
                        {
                            out.eprintln(&format!("[thinking] toggled but save failed: {e}"));
                        } else {
                            out.println(&format!(
                                "thinking {} (budget: {})",
                                if new_state { "enabled" } else { "disabled" },
                                self.runtime.config().model.thinking_budget
                            ));
                        }
                    }
                    Err(e) => out.eprintln(&format!("[thinking] {e}")),
                }
            }
            // ── /effort <level> ───────────────────────────────────────────────
            "/effort" => {
                let level = rest.trim().to_ascii_lowercase();
                const LEVELS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
                if level.is_empty() {
                    let cfg = self.runtime.config();
                    let claude = cfg
                        .provider
                        .anthropic
                        .effort
                        .clone()
                        .unwrap_or_else(|| "high".into());
                    let copilot = cfg
                        .provider
                        .copilot
                        .reasoning_effort
                        .clone()
                        .unwrap_or_else(|| "model default".into());
                    out.println(&format!(
                        "[effort] Claude: {claude} · Copilot: {copilot}  (usage: /effort {})",
                        LEVELS.join("|")
                    ));
                    return Ok(true);
                }
                let level = match level.as_str() {
                    "l" => "low".to_string(),
                    "m" | "med" => "medium".to_string(),
                    "h" => "high".to_string(),
                    "x" => "xhigh".to_string(),
                    other => other.to_string(),
                };
                if !LEVELS.contains(&level.as_str()) {
                    out.eprintln(&format!("[effort] usage: /effort {}", LEVELS.join("|")));
                    return Ok(true);
                }
                let mut cfg = self.runtime.config().clone();
                cfg.provider.anthropic.effort = Some(level.clone());
                cfg.provider.copilot.reasoning_effort = Some(level.clone());
                cfg.model.enable_thinking = level != "low";
                match self.runtime.apply_dcode_ai_config(cfg) {
                    Ok(()) => {
                        if let ReplOutput::Tui(st) = &out
                            && let Ok(mut g) = st.lock()
                        {
                            g.thinking_enabled = level != "low";
                        }
                        out.println(&format!(
                            "[effort] {level} for this session (Claude effort; Copilot reasoning where supported)"
                        ));
                    }
                    Err(e) => out.eprintln(&format!("[effort] {e}")),
                }
            }
            _ => {}
        }
        Ok(true)
    }
}
