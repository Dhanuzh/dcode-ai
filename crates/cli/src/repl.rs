use crate::file_mentions::{
    at_token_before_cursor, discover_workspace_files, expand_at_file_mentions_default,
    filter_paths_prefix,
};
use crate::oauth_login::{LogoutTarget, OAuthProvider};
use crate::prompt::DcodeAiPrompt;
use crate::runner::{SessionRuntime, dispatch_question_answer, dispatch_tool_approval};
use crate::slash_commands::SLASH_COMMANDS;
use crate::tui::app::ApprovalAnswer;
use crate::tui::{
    DisplayBlock, SessionPickerEntry, TuiCmd, TuiSessionState, git_create_branch,
    git_current_branch, git_list_branches, git_switch_branch, replay_event_log_into_state,
    run_blocking, spawn_tui_bridge,
};
use dcode_ai_common::config::{PermissionMode, ProviderKind};
use dcode_ai_common::event::{
    EndReason, InteractiveQuestionPayload, QuestionOption, QuestionSelection,
};
use dcode_ai_common::message::{Message, Role};
use dcode_ai_common::provider_runtime::has_claude_cli;
use dcode_ai_core::skills::SkillCatalog;
use dcode_ai_runtime::memory_store::MemoryStore;
use reedline::{Completer, Emacs, FileBackedHistory, Reedline, Signal, Suggestion, Vi};
use std::collections::VecDeque;
use std::io::Write;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::process::Command;

const STARTUP_APPROVE_ALL_QUESTION_ID: &str = "startup-approve-all";

/// Where slash-command and preset output goes (TTY transcript vs full-screen TUI).
pub(crate) enum ReplOutput<'a> {
    Stdio,
    Tui(&'a Arc<Mutex<TuiSessionState>>),
}

impl ReplOutput<'_> {
    fn print(&self, s: &str) {
        match self {
            ReplOutput::Stdio => {
                print!("{s}");
                let _ = std::io::stdout().flush();
            }
            ReplOutput::Tui(st) => {
                if let Ok(mut g) = st.lock() {
                    for line in s.split('\n') {
                        g.blocks.push(DisplayBlock::System(line.to_string()));
                    }
                    g.touch_transcript();
                }
            }
        }
    }

    fn println(&self, s: &str) {
        self.print(&format!("{s}\n"));
    }

    /// Push content that should be rendered as markdown (code fences, diffs,
    /// tables get syntax highlighting / colored lanes). In stdio mode prints raw.
    fn print_markdown(&self, s: &str) {
        match self {
            ReplOutput::Stdio => {
                print!("{s}");
                let _ = std::io::stdout().flush();
            }
            ReplOutput::Tui(st) => {
                if let Ok(mut g) = st.lock() {
                    g.blocks.push(DisplayBlock::Assistant(s.to_string()));
                    g.touch_transcript();
                    g.transcript_follow_tail = true;
                }
            }
        }
    }

    fn eprintln(&self, s: &str) {
        match self {
            ReplOutput::Stdio => eprintln!("{s}"),
            ReplOutput::Tui(st) => {
                if let Ok(mut g) = st.lock() {
                    g.blocks.push(DisplayBlock::System(format!("[!] {s}")));
                    g.touch_transcript();
                }
            }
        }
    }

    fn clear_screen(&self) {
        match self {
            ReplOutput::Stdio => {
                print!("\x1B[2J\x1B[H");
                std::io::stdout().flush().ok();
            }
            ReplOutput::Tui(st) => {
                if let Ok(mut g) = st.lock() {
                    g.blocks.clear();
                    g.flushed_block_count = 0;
                    g.streaming_assistant = None;
                    g.streaming_thinking = None;
                    g.scroll_lines = 0;
                    g.request_clear = true;
                    g.touch_transcript();
                }
            }
        }
    }
}

/// Compact token/number formatting: 1234 → "1.2k", 1_200_000 → "1.2M".
fn fmt_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn oauth_only_provider(provider: ProviderKind) -> bool {
    matches!(
        provider,
        ProviderKind::OpenAi | ProviderKind::Anthropic | ProviderKind::Antigravity
    )
}

fn oauth_login_slug_for_provider(provider: ProviderKind) -> Option<&'static str> {
    match provider {
        ProviderKind::OpenAi => Some("openai"),
        ProviderKind::Anthropic => Some("anthropic"),
        ProviderKind::Antigravity => Some("antigravity"),
        ProviderKind::OpenRouter | ProviderKind::OpenCodeZen => None,
    }
}

/// Local OpenAI-compatible server presets for `/connect <name>`.
fn local_preset_for(value: &str) -> Option<(&'static str, &'static str)> {
    match value.trim().to_ascii_lowercase().as_str() {
        "ollama" => Some(("Ollama", "http://localhost:11434/v1")),
        "lmstudio" | "lm-studio" | "lm_studio" => Some(("LM Studio", "http://localhost:1234/v1")),
        "vllm" => Some(("vLLM", "http://localhost:8000/v1")),
        _ => None,
    }
}

fn parse_oauth_provider(value: &str) -> Option<OAuthProvider> {
    match value.trim().to_ascii_lowercase().as_str() {
        "openai" | "open-ai" | "gpt" | "codex" => Some(OAuthProvider::Openai),
        "anthropic" | "claude" => Some(OAuthProvider::Anthropic),
        "copilot" | "github" => Some(OAuthProvider::Copilot),
        "antigravity" | "ag" => Some(OAuthProvider::Antigravity),
        "opencodezen" | "opencode" | "zen" => Some(OAuthProvider::Opencodezen),
        _ => None,
    }
}

fn parse_logout_target(value: &str) -> Option<LogoutTarget> {
    match value.trim().to_ascii_lowercase().as_str() {
        "anthropic" | "claude" => Some(LogoutTarget::Anthropic),
        "openai" | "open-ai" | "gpt" | "codex" => Some(LogoutTarget::Openai),
        "copilot" | "github" => Some(LogoutTarget::Copilot),
        "antigravity" | "ag" => Some(LogoutTarget::Antigravity),
        "vertex" | "gcp" | "cloudproject" | "cloud-project" => Some(LogoutTarget::Vertex),
        "opencodezen" | "opencode" | "zen" => Some(LogoutTarget::Opencodezen),
        "all" | "*" => Some(LogoutTarget::All),
        _ => None,
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

fn compact_ws_single_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn role_label(role: &Role) -> &'static str {
    match role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::System => "system",
        Role::Tool => "tool",
    }
}

fn recent_context_lines(messages: &[Message], max_items: usize) -> Vec<String> {
    if messages.is_empty() {
        return vec!["Recent turns: none yet.".into()];
    }

    let take = max_items.max(1);
    let start = messages.len().saturating_sub(take);
    let mut lines = Vec::new();
    if start > 0 {
        lines.push(format!(
            "Recent turns: last {} of {} message(s)",
            messages.len() - start,
            messages.len()
        ));
    } else {
        lines.push(format!("Recent turns: {} message(s)", messages.len()));
    }

    for (idx, msg) in messages.iter().enumerate().skip(start) {
        let mut preview = compact_ws_single_line(&msg.event_preview());
        if preview.is_empty() {
            preview = "[empty]".into();
        }
        if let Some(tool_calls) = msg.tool_calls.as_ref()
            && !tool_calls.is_empty()
        {
            preview.push_str(&format!(" [tool_calls:{}]", tool_calls.len()));
        }
        if let Some(call_id) = msg.tool_call_id.as_ref()
            && !call_id.is_empty()
        {
            preview.push_str(&format!(" [call_id:{call_id}]"));
        }

        lines.push(format!(
            "  {:>4}. {:<9} {}",
            idx + 1,
            role_label(&msg.role),
            truncate_chars(&preview, 220)
        ));
    }
    lines
}

fn keymaps_help_lines() -> Vec<String> {
    vec![
        "KEYMAPS".into(),
        String::new(),
        "Core".into(),
        "  Enter        Send".into(),
        "  Shift+Enter  Newline (terminal-dependent)".into(),
        "  Ctrl+I / J   Newline (reliable fallback)".into(),
        "  Up / Down    Composer history".into(),
        "  Tab          Cycle agent profile".into(),
        String::new(),
        "Navigation".into(),
        "  Ctrl+P       Command palette".into(),
        "  Ctrl+F       Transcript search (TUI)".into(),
        "  Ctrl+R       Composer history search (TUI)".into(),
        "  Wheel        Transcript scroll".into(),
        "  Drag         Select transcript text".into(),
        String::new(),
        "Session".into(),
        "  Ctrl+K       Pin latest message".into(),
        "  Ctrl+O       Pinned notes list".into(),
        "  Ctrl+G       Sub-agent dashboard".into(),
        "  Ctrl+V       Paste image / stage image".into(),
        "  Ctrl+L       Clear screen".into(),
        "  Ctrl+C       Cancel current turn".into(),
        String::new(),
        "Editing (Emacs-style)".into(),
        "  Ctrl+←/→     Move word backward / forward".into(),
        "  Alt+B / Alt+F  Move word backward / forward".into(),
        "  Ctrl+W        Delete word backward".into(),
        "  Alt+⌫         Delete word backward".into(),
        "  Alt+D         Delete word forward".into(),
        "  Ctrl+K        Kill to end of line (saves to kill ring)".into(),
        "  Ctrl+Y        Yank (paste) from kill ring".into(),
        "  Home / End    Start / end of current line".into(),
        "  ↑ / ↓        Move cursor up/down in multiline; history at edges".into(),
        String::new(),
        "Clipboard".into(),
        "  F6           Copy latest assistant response".into(),
        "  F7           Copy last tool output".into(),
        "  Click header Copy tool output / assistant response (TUI)".into(),
        String::new(),
        "Leader (Ctrl+X)".into(),
        "  Ctrl+X M     Switch model".into(),
        "  Ctrl+X E     Open editor".into(),
        "  Ctrl+X L     Switch session".into(),
        "  Ctrl+X N     New session".into(),
        "  Ctrl+X C     Compact".into(),
        "  Ctrl+X S     View status".into(),
        "  Ctrl+X A     Agent picker".into(),
        "  Ctrl+X P     Project picker".into(),
        "  Ctrl+X H     Help".into(),
        "  Ctrl+X Q     Exit".into(),
        String::new(),
        "Also available: /help, /keymaps".into(),
    ]
}

/// Special input prefixes
#[allow(dead_code)]
const INPUT_PREFIXES: &[&str] = &[
    "!",  // Bash mode - run shell command directly
    "@",  // File reference - fuzzy file search
    "\\", // Multiline continuation
];

/// Agent profiles inspired by OpenCode's multi-agent system.
/// Each profile modifies behavior and system prompt emphasis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AgentProfile {
    /// Default full-access agent for development work
    #[default]
    Build,
    /// Read-only agent for analysis and planning - denies edits
    Plan,
    /// Focused code review agent
    Review,
    /// Bug diagnosis and fix agent
    Fix,
    /// Testing and validation agent
    Test,
}

impl AgentProfile {
    /// Get the display name for this profile (shown in prompt)
    pub fn label(&self) -> &'static str {
        match self {
            AgentProfile::Build => "build",
            AgentProfile::Plan => "plan",
            AgentProfile::Review => "review",
            AgentProfile::Fix => "fix",
            AgentProfile::Test => "test",
        }
    }

    /// Get system prompt modifier for this profile
    #[allow(dead_code)]
    pub fn system_modifier(&self) -> &'static str {
        match self {
            AgentProfile::Build => "",
            AgentProfile::Plan => {
                "Profile: PLAN MODE (read-only)\n- You must not modify files or run shell commands.\n\
                 - Inspect, search, read, research the web, and propose the next steps only.\n\
                 - If asked to change code, explain what would change instead of claiming it was done."
            }
            AgentProfile::Review => {
                "Profile: REVIEW MODE\n- Focus on identifying bugs, regressions, security issues, and code quality problems.\n\
                 - Check for missing tests, edge cases, and error handling.\n\
                 - Be specific about severity: critical, major, minor, or suggestion."
            }
            AgentProfile::Fix => {
                "Profile: FIX MODE\n- Diagnose the issue thoroughly before making changes.\n\
                 - Prefer minimal, verified fixes over broad rewrites.\n\
                 - Always explain the root cause and the fix."
            }
            AgentProfile::Test => {
                "Profile: TEST MODE\n- Focus on validating code correctness and edge cases.\n\
                 - Run tests, checks, or lints when tools allow.\n\
                 - Report clearly what passed, what failed, and any issues found."
            }
        }
    }

    /// Get reedline suggestion color for this profile
    #[allow(dead_code)]
    pub fn style(&self) -> &'static str {
        match self {
            AgentProfile::Build => "",
            AgentProfile::Plan => "cyan",
            AgentProfile::Review => "yellow",
            AgentProfile::Fix => "red",
            AgentProfile::Test => "green",
        }
    }

    /// Cycle to the next profile (for Tab switching)
    pub fn next(self) -> Self {
        match self {
            AgentProfile::Build => AgentProfile::Plan,
            AgentProfile::Plan => AgentProfile::Review,
            AgentProfile::Review => AgentProfile::Fix,
            AgentProfile::Fix => AgentProfile::Test,
            AgentProfile::Test => AgentProfile::Build,
        }
    }

    /// All profiles in cycle order
    pub const ALL: [Self; 5] = [Self::Build, Self::Plan, Self::Review, Self::Fix, Self::Test];
}

impl std::fmt::Display for AgentProfile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

/// Session state for REPL
/// A detached shell command tracked by `/run-bg`, `/ps`, `/stop`.
struct BackgroundJob {
    id: u32,
    cmd: String,
    output: std::sync::Arc<std::sync::Mutex<String>>,
    done: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: tokio::task::JoinHandle<()>,
}

pub struct Repl {
    runtime: SessionRuntime,
    prompt: DcodeAiPrompt,
    run_mode: bool,
    safe_mode: bool,
    history_path: std::path::PathBuf,
    agent_profile: AgentProfile,
    current_agent_label: String,
    background_jobs: Vec<BackgroundJob>,
    next_job_id: u32,
    /// Stack of message snapshots for ephemeral `/side` asides (supports nesting).
    side_snapshots: Vec<Vec<dcode_ai_common::message::Message>>,
}

impl Repl {
    pub fn new(runtime: SessionRuntime, safe_mode: bool, run_mode: bool) -> Self {
        let history_path = runtime.workspace_root().join(".dcode-ai/.history");
        let agent_profile = AgentProfile::default();
        let current_agent_label = format!("@{}", agent_profile.label());
        Self {
            runtime,
            prompt: DcodeAiPrompt::new(safe_mode, run_mode),
            run_mode,
            safe_mode,
            history_path,
            agent_profile,
            current_agent_label,
            background_jobs: Vec::new(),
            next_job_id: 1,
            side_snapshots: Vec::new(),
        }
    }

    /// Spawn a detached shell command, capturing its output for `/ps`.
    fn start_background_job(&mut self, cmd: &str) -> u32 {
        use std::sync::Arc;
        use std::sync::atomic::AtomicBool;
        use tokio::io::{AsyncBufReadExt, BufReader};
        let id = self.next_job_id;
        self.next_job_id += 1;
        let output = Arc::new(std::sync::Mutex::new(String::new()));
        let done = Arc::new(AtomicBool::new(false));
        let out_clone = output.clone();
        let done_clone = done.clone();
        let cmd_str = cmd.to_string();
        let ws = self.runtime.workspace_root().to_path_buf();
        let handle = tokio::spawn(async move {
            // Stream stdout/stderr live so `/ps` reflects progress instead of
            // staying empty until the process exits.
            let spawned = dcode_ai_common::provider_runtime::system_shell_command(&cmd_str)
                .current_dir(&ws)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true)
                .spawn();
            let mut child = match spawned {
                Ok(c) => c,
                Err(e) => {
                    if let Ok(mut buf) = out_clone.lock() {
                        buf.push_str(&format!("error: {e}"));
                    }
                    done_clone.store(true, std::sync::atomic::Ordering::SeqCst);
                    return;
                }
            };
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();
            let out_a = out_clone.clone();
            let t_out = tokio::spawn(async move {
                let Some(r) = stdout else { return };
                let mut lines = BufReader::new(r).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Ok(mut buf) = out_a.lock() {
                        buf.push_str(&line);
                        buf.push('\n');
                    }
                }
            });
            let out_b = out_clone.clone();
            let t_err = tokio::spawn(async move {
                let Some(r) = stderr else { return };
                let mut lines = BufReader::new(r).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Ok(mut buf) = out_b.lock() {
                        buf.push_str(&line);
                        buf.push('\n');
                    }
                }
            });
            let _ = child.wait().await;
            let _ = t_out.await;
            let _ = t_err.await;
            done_clone.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        self.background_jobs.push(BackgroundJob {
            id,
            cmd: cmd.to_string(),
            output,
            done,
            handle,
        });
        id
    }

    fn should_offer_startup_approve_all_popup(&self) -> bool {
        self.runtime.config().permissions.startup_approve_all
            && !matches!(
                self.runtime.permission_mode(),
                PermissionMode::Plan | PermissionMode::DontAsk
            )
    }

    /// Run the interactive REPL until the user exits.
    pub async fn run(&mut self) -> anyhow::Result<()> {
        // Read the cached "update available" version (and refresh in the
        // background if stale) before entering the TUI. The banner reads the
        // cached result; the network call never blocks startup.
        let _ = crate::update_check::init_and_pending_upgrade();
        let mut editor = self.build_editor()?;

        let _spawn_task = {
            let spawn_rx = self.runtime.take_spawn_rx();
            let event_tx = self.runtime.event_tx();
            if let Some(srx) = spawn_rx {
                Some(dcode_ai_runtime::supervisor::spawn_subagent_consumer(
                    srx,
                    self.runtime.session_id().to_string(),
                    self.runtime.workspace_root().to_path_buf(),
                    self.runtime.config().clone(),
                    self.runtime.messages().to_vec(),
                    event_tx,
                ))
            } else {
                None
            }
        };

        if self.run_mode {
            self.print_banner();
        }

        loop {
            // Update prompt with current agent profile
            self.prompt.set_agent(&self.current_agent_label);
            let sig = editor.read_line(&self.prompt);
            match sig {
                Ok(Signal::Success(input)) => {
                    if input.is_empty() {
                        continue;
                    }

                    // Tab switches agent profile (OpenCode-style)
                    if input == "\t" {
                        self.switch_agent();
                        continue;
                    }

                    // Bash mode: ! prefix runs shell command directly
                    if input.starts_with('!') {
                        let cmd = input.trim_start_matches('!');
                        self.run_bash_command(cmd).await;
                        continue;
                    }

                    // Slash commands
                    if input.starts_with('/') {
                        if !self.handle_command(&input, ReplOutput::Stdio).await? {
                            break;
                        }
                        continue;
                    }

                    let expanded = match expand_at_file_mentions_default(
                        &input,
                        self.runtime.workspace_root(),
                    ) {
                        Ok(s) => s,
                        Err(e) => {
                            eprintln!("file mention expansion: {e}");
                            continue;
                        }
                    };
                    match self.runtime.run_turn(&expanded).await {
                        Ok(output) => {
                            println!("{output}");
                        }
                        Err(err) => {
                            eprintln!("error: {err}");
                        }
                    }
                }
                Ok(Signal::CtrlD) => {
                    // Ctrl+D - exit
                    eprintln!("\n[exit]");
                    break;
                }
                Ok(Signal::CtrlC) => {
                    // Ctrl+C - cancel current or exit
                    eprintln!(
                        "\n[cancel] Press Ctrl+D to exit, or wait for current operation to complete"
                    );
                }
                Err(err) => {
                    eprintln!("read error: {err}");
                    break;
                }
            }
        }

        self.runtime.finish(EndReason::UserExit).await;
        Ok(())
    }

    fn print_banner(&self) {
        eprintln!(
            r#"
╔══════════════════════════════════════════════════════════════╗
║  dcode-ai - Native CLI AI                                          ║
║  Interactive terminal mode                                     ║
╠══════════════════════════════════════════════════════════════╣
║  Shortcuts:                                                   ║
║    ! <cmd>   Run shell command (bash mode)                    ║
║    @path     Inline file mentions (expanded before send)      ║
║    / <cmd>   Slash commands                                  ║
║    Tab       Switch agent profile (@build/@plan/@review...)   ║
║    Ctrl+D    Exit                                            ║
║    Ctrl+C    Cancel current request                           ║
║    Ctrl+L    Clear screen                                     ║
║    Ctrl+R    Search command history                           ║
╚══════════════════════════════════════════════════════════════╝
"#
        );
    }

    /// Switch to the next agent profile (called on Tab press)
    fn switch_agent(&mut self) {
        let next = self.agent_profile.next();
        self.agent_profile = next;
        self.current_agent_label = format!("@{}", next.label());
        self.prompt.set_agent(&self.current_agent_label);

        // Update runtime permission mode based on profile
        if next == AgentProfile::Plan {
            self.runtime.set_permission_mode(PermissionMode::Plan);
        }

        eprintln!("\n[agent] Switched to @{} mode", next.label());
        if next == AgentProfile::Plan {
            eprintln!("[agent] Plan mode: file edits and shell commands are disabled");
        }
    }

    /// Run a shell command directly (bash mode) - Claude Code style
    /// Output is returned to the conversation context
    async fn run_bash_command(&self, cmd: &str) {
        let cmd = cmd.trim();
        if cmd.is_empty() {
            eprintln!("! usage: !<command> [args]");
            return;
        }

        eprintln!("[bash] {cmd}");

        let output = dcode_ai_common::provider_runtime::system_shell_command(cmd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);

                if !stdout.is_empty() {
                    println!("{stdout}");
                }
                if !stderr.is_empty() {
                    eprintln!("[stderr] {stderr}");
                }
                if out.status.success() {
                    eprintln!("[bash] completed (exit 0)");
                } else {
                    eprintln!("[bash] failed (exit {})", out.status.code().unwrap_or(-1));
                }
            }
            Err(e) => {
                eprintln!("[bash] failed to execute: {e}");
            }
        }
    }

    /// Open the configured external editor (`DCODE_AI_EDITOR`, `[ui].editor`, `EDITOR`, `vim`).
    async fn open_external_editor(&self, seed: Option<&str>) -> Option<String> {
        let editor_cmd = self.runtime.config().effective_editor_command();
        let temp_file = format!("dcode-ai-prompt-{}.txt", std::process::id());
        let temp_path = std::env::temp_dir().join(&temp_file);
        std::fs::write(&temp_path, seed.unwrap_or("")).ok()?;

        #[cfg(windows)]
        let editor_line = format!("{} \"{}\"", editor_cmd, temp_path.display());
        #[cfg(not(windows))]
        let editor_line = format!("{} '{}'", editor_cmd, temp_path.display());
        let output = dcode_ai_common::provider_runtime::system_shell_command(&editor_line)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        match output {
            Ok(_) => {
                let content = std::fs::read_to_string(&temp_path).ok()?;
                let _ = std::fs::remove_file(&temp_path);
                let content = content.trim().to_string();
                if content.is_empty() {
                    None
                } else {
                    Some(content)
                }
            }
            Err(e) => {
                eprintln!("[editor] Failed to open: {e}");
                None
            }
        }
    }

    async fn apply_provider_in_session(
        &mut self,
        p: ProviderKind,
        out: ReplOutput<'_>,
    ) -> anyhow::Result<()> {
        let mut cfg = self.runtime.config().clone();
        cfg.set_default_provider(p);
        if p == ProviderKind::OpenAi
            && cfg
                .provider
                .openai
                .base_url
                .to_ascii_lowercase()
                .contains("githubcopilot.com")
        {
            cfg.provider.openai.base_url = "https://api.openai.com".to_string();
        }
        match self.runtime.apply_dcode_ai_config(cfg) {
            Ok(()) => {
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.model = self.runtime.model().to_string();
                }
                match self.runtime.config().save_global() {
                    Ok(()) => out.println(&format!(
                        "[provider] {} — model {} — saved",
                        p.display_name(),
                        self.runtime.model()
                    )),
                    Err(e) => {
                        out.eprintln(&format!("[provider] applied but global save failed: {e}"))
                    }
                }
            }
            Err(e) => out.eprintln(&format!("[provider] {e}")),
        }
        Ok(())
    }

    /// Connect a local OpenAI-compatible server (Ollama / LM Studio / vLLM):
    /// probe its /models endpoint, pick a model, and persist the provider
    /// config — no manual OPENAI_BASE_URL needed.
    async fn connect_local_preset(
        &mut self,
        label: &str,
        base_url: &str,
        out: &ReplOutput<'_>,
    ) -> anyhow::Result<()> {
        out.println(&format!("[connect] Probing {label} at {base_url}…"));
        let models_url = format!("{}/models", base_url.trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(3))
            .build()?;
        let model_ids: Vec<String> = match client.get(&models_url).send().await {
            Ok(resp) if resp.status().is_success() => {
                let value: serde_json::Value = resp.json().await.unwrap_or_default();
                value["data"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|m| m["id"].as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default()
            }
            Ok(resp) => {
                out.eprintln(&format!(
                    "[connect] {label} responded {} at {models_url}",
                    resp.status()
                ));
                return Ok(());
            }
            Err(e) => {
                out.eprintln(&format!(
                    "[connect] {label} not reachable at {base_url} — is it running? ({e})"
                ));
                return Ok(());
            }
        };

        let mut cfg = self.runtime.config().clone();
        cfg.set_default_provider(ProviderKind::OpenAi);
        cfg.provider.openai.base_url = base_url.to_string();
        // Local servers ignore auth; a sentinel key keeps the OpenAI provider
        // from demanding a real one (or falling into the OAuth path).
        cfg.provider.openai.api_key = Some("local".to_string());
        let keep_current = model_ids.iter().any(|m| m == &cfg.provider.openai.model);
        if !keep_current && let Some(first) = model_ids.first() {
            cfg.provider.openai.model = first.clone();
        }
        cfg.sync_default_model_from_provider();

        match self.runtime.apply_dcode_ai_config(cfg) {
            Ok(()) => {
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.model = self.runtime.model().to_string();
                }
                match self.runtime.config().save_global() {
                    Ok(()) => {
                        out.println(&format!(
                            "[connect] {label} connected — model {} ({} available; /models to list, /model to switch)",
                            self.runtime.model(),
                            model_ids.len()
                        ));
                    }
                    Err(e) => {
                        out.eprintln(&format!("[connect] applied but global save failed: {e}"))
                    }
                }
            }
            Err(e) => out.eprintln(&format!("[connect] {e}")),
        }
        Ok(())
    }

    /// `/login vertex <project-id> [location]` — Gemini on Vertex AI in the
    /// user's own GCP project via gcloud Application Default Credentials.
    async fn connect_vertex_project(
        &mut self,
        project: Option<String>,
        location: String,
        out: &ReplOutput<'_>,
    ) -> anyhow::Result<()> {
        let Some(project) = project.filter(|p| !p.trim().is_empty()) else {
            out.println("usage: /login vertex <project-id> [location]");
            out.println("Uses your Google Cloud project on Vertex AI (billed to that project).");
            out.println("Prerequisites:");
            out.println("  1. gcloud CLI installed");
            out.println("  2. gcloud auth application-default login");
            out.println("  3. Vertex AI API enabled on the project");
            return Ok(());
        };
        let project = project.trim().to_string();

        out.println("[vertex] Checking gcloud Application Default Credentials…");
        // Token minting is a blocking subprocess call; keep the TUI responsive.
        let token =
            tokio::task::spawn_blocking(dcode_ai_core::provider::antigravity::adc_access_token)
                .await
                .map_err(|e| anyhow::anyhow!("adc probe task failed: {e}"))?;
        if let Err(e) = token {
            out.eprintln(&format!("[vertex] {e}"));
            return Ok(());
        }

        let mut store = dcode_ai_common::auth::AuthStore::load().unwrap_or_default();
        store.vertex = Some(dcode_ai_common::auth::VertexAuth {
            project_id: project.clone(),
            location: location.clone(),
        });
        store.preferred_provider = Some(dcode_ai_common::auth::LoggedProvider::Antigravity);
        if let Err(e) = store.save() {
            out.eprintln(&format!("[vertex] failed to save auth: {e}"));
            return Ok(());
        }

        let mut cfg = self.runtime.config().clone();
        cfg.set_default_provider(ProviderKind::Antigravity);
        // The shared openai model slot may hold a non-Gemini id; Vertex only
        // serves Gemini publisher models here.
        if !cfg
            .provider
            .openai
            .model
            .to_ascii_lowercase()
            .contains("gemini")
        {
            // Project allowlists vary; start on the broadly-available flash
            // and let /models probe what this project can actually call.
            cfg.provider.openai.model = "gemini-2.5-flash".to_string();
        }
        cfg.sync_default_model_from_provider();
        match self.runtime.apply_dcode_ai_config(cfg) {
            Ok(()) => {
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.model = self.runtime.model().to_string();
                }
                let _ = self.runtime.config().save_global();
                out.println(&format!(
                    "[vertex] Connected — project {project}, location {location}, model {} \
                     (/model to switch, /logout vertex to disconnect)",
                    self.runtime.model()
                ));
            }
            Err(e) => out.eprintln(&format!("[vertex] {e}")),
        }
        Ok(())
    }

    async fn apply_provider_after_oauth_login(
        &mut self,
        provider: OAuthProvider,
        out: &ReplOutput<'_>,
    ) -> anyhow::Result<()> {
        let mut cfg = self.runtime.config().clone();
        let target = match provider {
            OAuthProvider::Openai => {
                cfg.set_default_provider(ProviderKind::OpenAi);
                if cfg
                    .provider
                    .openai
                    .base_url
                    .to_ascii_lowercase()
                    .contains("githubcopilot.com")
                {
                    cfg.provider.openai.base_url = "https://api.openai.com".to_string();
                }
                ProviderKind::OpenAi
            }
            OAuthProvider::Copilot => {
                cfg.set_default_provider(ProviderKind::OpenAi);
                cfg.provider.openai.base_url = "https://api.githubcopilot.com".to_string();
                ProviderKind::OpenAi
            }
            OAuthProvider::Anthropic => {
                cfg.set_default_provider(ProviderKind::Anthropic);
                ProviderKind::Anthropic
            }
            OAuthProvider::Antigravity => {
                cfg.set_default_provider(ProviderKind::Antigravity);
                // Antigravity shares the `openai` model slot; a leftover OpenAI
                // model id (e.g. gpt-5) is rejected by the Google backend, so
                // default to a Gemini model unless one is already selected.
                if !cfg
                    .provider
                    .openai
                    .model
                    .to_ascii_lowercase()
                    .contains("gemini")
                {
                    cfg.provider.openai.model = "gemini-3-pro".to_string();
                }
                ProviderKind::Antigravity
            }
            OAuthProvider::Opencodezen => {
                cfg.set_default_provider(ProviderKind::OpenCodeZen);
                ProviderKind::OpenCodeZen
            }
        };
        match self.runtime.apply_dcode_ai_config(cfg) {
            Ok(()) => {
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.model = self.runtime.model().to_string();
                }
                if let Err(e) = self.runtime.config().save_global() {
                    out.eprintln(&format!(
                        "[login] logged in and switched provider, but global save failed: {e}"
                    ));
                } else {
                    out.println(&format!(
                        "[login] using {} - model {}",
                        target.display_name(),
                        self.runtime.model()
                    ));
                }
            }
            Err(e) => out.eprintln(&format!(
                "[login] logged in, but failed to apply provider {}: {e}",
                target.display_name()
            )),
        }
        Ok(())
    }

    async fn apply_copilot_provider_in_session(
        &mut self,
        out: ReplOutput<'_>,
    ) -> anyhow::Result<()> {
        let auth = dcode_ai_common::auth::AuthStore::load().unwrap_or_default();
        if auth.copilot.is_none() {
            out.eprintln("[provider] Copilot is not logged in. Run: /login copilot");
            return Ok(());
        }
        let mut cfg = self.runtime.config().clone();
        cfg.set_default_provider(ProviderKind::OpenAi);
        cfg.provider.openai.base_url = "https://api.githubcopilot.com".to_string();
        match self.runtime.apply_dcode_ai_config(cfg) {
            Ok(()) => {
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.model = self.runtime.model().to_string();
                }
                match self.runtime.config().save_global() {
                    Ok(()) => out.println(&format!(
                        "[provider] Copilot - model {} - saved",
                        self.runtime.model()
                    )),
                    Err(e) => {
                        out.eprintln(&format!("[provider] applied but global save failed: {e}"))
                    }
                }
            }
            Err(e) => out.eprintln(&format!("[provider] {e}")),
        }
        Ok(())
    }

    async fn apply_codex_provider_in_session(&mut self, out: ReplOutput<'_>) -> anyhow::Result<()> {
        let mut cfg = self.runtime.config().clone();
        cfg.set_default_provider(ProviderKind::OpenAi);
        if cfg
            .provider
            .openai
            .base_url
            .to_ascii_lowercase()
            .contains("githubcopilot.com")
        {
            cfg.provider.openai.base_url = "https://api.openai.com".to_string();
        }
        match self.runtime.apply_dcode_ai_config(cfg) {
            Ok(()) => {
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.model = self.runtime.model().to_string();
                }
                match self.runtime.config().save_global() {
                    Ok(()) => out.println(&format!(
                        "[provider] OpenAI Codex - model {} - saved",
                        self.runtime.model()
                    )),
                    Err(e) => {
                        out.eprintln(&format!("[provider] applied but global save failed: {e}"))
                    }
                }
            }
            Err(e) => out.eprintln(&format!("[provider] {e}")),
        }
        Ok(())
    }

    async fn save_provider_api_key(
        &mut self,
        p: ProviderKind,
        key: &str,
        out: ReplOutput<'_>,
    ) -> anyhow::Result<()> {
        if oauth_only_provider(p) {
            let login = oauth_login_slug_for_provider(p).unwrap_or("openai");
            out.eprintln(&format!(
                "[apikey] {} uses OAuth login. Run: dcode-ai login {}",
                p.display_name(),
                login
            ));
            return Ok(());
        }

        // Secrets go to the 0600 credentials store, not the shareable config
        // files. A leftover inline plaintext key is cleared so old configs
        // migrate on the next save.
        let mut cfg = self.runtime.config().clone();
        let env_name = cfg.provider.api_key_env_for(p).to_string();
        if let Err(e) = dcode_ai_common::credentials::set(&env_name, key) {
            out.eprintln(&format!("[apikey] failed to store credential: {e}"));
            return Ok(());
        }
        cfg.set_provider_api_key(p, "");
        match self.runtime.apply_dcode_ai_config(cfg) {
            Ok(()) => out.println(&format!(
                "[apikey] saved for {} (~/.dcode-ai/credentials.toml)",
                p.display_name()
            )),
            Err(e) => out.eprintln(&format!("[apikey] {e}")),
        }
        Ok(())
    }

    fn build_editor(&self) -> anyhow::Result<Reedline> {
        let mut builder = Reedline::create()
            .with_quick_completions(true)
            .with_partial_completions(true)
            .with_ansi_colors(true);

        // Try to load history from disk
        if let Some(parent) = self.history_path.parent() {
            std::fs::create_dir_all(parent).ok();
            if let Ok(history) = FileBackedHistory::with_file(100, self.history_path.clone()) {
                builder = builder.with_history(Box::new(history));
            }
        }

        // Support vim mode if enabled via env
        if std::env::var("DCODE_AI_EDITOR_MODE")
            .map(|v| v.eq_ignore_ascii_case("vi") || v.eq_ignore_ascii_case("vim"))
            .unwrap_or(false)
        {
            builder = builder.with_edit_mode(Box::new(Vi::default()));
        } else {
            builder = builder.with_edit_mode(Box::new(Emacs::default()));
        }

        Ok(builder)
    }

    async fn handle_command(&mut self, input: &str, out: ReplOutput<'_>) -> anyhow::Result<bool> {
        let mut parts = input.split_whitespace();
        let command = parts.next().unwrap_or_default();
        let rest = input
            .strip_prefix(command)
            .map(str::trim)
            .unwrap_or_default();

        match command {
            "/q" | "/quit" | "/exit" => return Ok(false),
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
            "/ps" => {
                // Drop finished+empty handles; report status.
                if self.background_jobs.is_empty() {
                    out.println("[ps] no background jobs. Start one with /run-bg <cmd>");
                } else {
                    let mut lines = vec!["Background jobs:".to_string(), String::new()];
                    for job in &self.background_jobs {
                        let running =
                            !job.done.load(std::sync::atomic::Ordering::SeqCst);
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
                    dcode_ai_runtime::model_limits::detect_context_window(&model) as u64;

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
                lines.push(format!("Memory:      {}", self.runtime.memory_store_path().display()));
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
                let snapshot = self.runtime.snapshot();
                let session_line = snapshot
                    .session_name
                    .as_ref()
                    .map(|name| format!("Session:     {} ({})", name, snapshot.id))
                    .unwrap_or_else(|| format!("Session:     {}", snapshot.id));
                let mut lines = vec![
                    session_line,
                    format!("Model:       {}", self.runtime.model()),
                    format!("Permission:  {:?}", self.runtime.permission_mode()),
                    format!("Messages:    {}", self.runtime.messages().len()),
                    format!("Children:    {}", snapshot.child_session_ids.len()),
                ];
                if let Some(summary) = &snapshot.session_summary {
                    lines.push(String::new());
                    lines.push(format!("Summary: {}", compact_ws_single_line(summary)));
                }
                lines.push(String::new());
                lines.extend(recent_context_lines(self.runtime.messages(), 14));
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_info_modal("context", lines);
                    }
                } else {
                    for l in &lines {
                        out.println(l);
                    }
                }
            }
            "/session-name" | "/rename" => {
                let raw = rest.trim();
                if raw.is_empty() {
                    if let Some(name) = self.runtime.session_name() {
                        out.println(&format!("[session-name] {name} ({})", self.runtime.session_id()));
                    } else {
                        out.println(&format!("[session-name] unset ({})", self.runtime.session_id()));
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
                let applied = self.runtime.session_name().unwrap_or(self.runtime.session_id());
                if let Err(e) = self.runtime.save().await {
                    out.eprintln(&format!("[session-name] set to `{applied}`, but save failed: {e}"));
                } else {
                    out.println(&format!("[session-name] set to `{applied}`"));
                }
            }
            "/agent" => {
                if let Some(target) = parts.next() {
                    let target_clean = target.trim_start_matches('@').to_lowercase();
                    let matched = AgentProfile::ALL.iter().find(|p| {
                        p.label() == target_clean
                    });
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
                            g.set_permission_mode(&format!(
                                "{:?}",
                                self.runtime.permission_mode()
                            ));
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
                        let marker = if profile == self.agent_profile { " *" } else { "" };
                        out.println(&format!("  @{}{}", profile.label(), marker));
                    }
                }
            }
            "/plan" => {
                self.run_preset(
                    "Create a step-by-step implementation plan before coding. Call the `update_plan` \
tool with the ordered steps (status pending/in_progress/completed) and keep it updated as you \
work. Focus on concrete steps, risks, and validation.\n\nTask:\n",
                    rest,
                    out,
                )
                .await?
            }
            "/review" => {
                self.run_preset(
                    "Review the requested code or changes. Prioritize bugs, regressions, risks, and missing tests.\n\nReview target:\n",
                    rest,
                    out,
                )
                .await?
            }
            "/fix" => {
                self.run_preset(
                    "Diagnose and fix the issue below. Prefer a minimal verified change.\n\nIssue:\n",
                    rest,
                    out,
                )
                .await?
            }
            "/test" => {
                self.run_preset(
                    "Validate the requested area. Run tests or checks if tools allow, and report what passed or failed.\n\nTarget:\n",
                    rest,
                    out,
                )
                .await?
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
            "/undo" => {
                match self.runtime.undo_last_turn() {
                    Ok(Some(msg)) => out.println(&format!("[undo] {msg}")),
                    Ok(None) => out.println("[undo] nothing to undo"),
                    Err(e) => out.eprintln(&format!("[undo] {e}")),
                }
            }
            "/redo" => {
                match self.runtime.redo_last_turn() {
                    Ok(Some(msg)) => out.println(&format!("[redo] {msg}")),
                    Ok(None) => out.println("[redo] nothing to redo"),
                    Err(e) => out.eprintln(&format!("[redo] {e}")),
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
                        let mut diff_buf =
                            String::from_utf8_lossy(&cmd_out.stdout).into_owned();
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
                    dcode_ai_runtime::model_limits::detect_context_window(&model) as u64;
                let (tin, tout, cost, ctx) = if let ReplOutput::Tui(st) = &out {
                    st.lock()
                        .ok()
                        .map(|g| (g.input_tokens, g.output_tokens, g.cost_usd, g.context_tokens))
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
            "/fork" => {
                match self.runtime.fork_session().await {
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
                }
            }
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
                    None => {
                        out.eprintln("[approve] nothing to approve (no recent tool approval request)")
                    }
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
            "/goals" => {
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
                                if depth > 0 { format!(" ({depth} aside(s) still open)") } else { String::new() }
                            ));
                        }
                        None => out.eprintln("[side] not in a side conversation (start one with /side)"),
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
                    format!("Memory:      {}", self.runtime.memory_store_path().display()),
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
            "/permissions" => {
                if let Some(mode) = parts.next() {
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
                            "invalid mode; expected one of: default, plan, accept-edits, dont-ask, bypass-permissions",
                        );
                    }
                } else if let ReplOutput::Tui(st) = &out {
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
                if rest.is_empty() {
                    let store = MemoryStore::new(self.runtime.memory_store_path());
                    let mem = store.load().await.map_err(anyhow::Error::msg)?;
                    if mem.notes.is_empty() {
                        let lines = vec!["No memory notes stored.".into()];
                        if let ReplOutput::Tui(st) = &out {
                            if let Ok(mut g) = st.lock() {
                                g.open_info_modal("memory", lines);
                            }
                        } else {
                            out.println("no memory notes stored");
                        }
                    } else {
                        let lines: Vec<String> = mem
                            .notes
                            .iter()
                            .rev()
                            .take(20)
                            .map(|note| {
                                format!("{} {} {}", note.id, note.kind, note.content.replace('\n', " "))
                            })
                            .collect();
                        if let ReplOutput::Tui(st) = &out {
                            if let Ok(mut g) = st.lock() {
                                g.open_info_modal("memory", lines);
                            }
                        } else {
                            for l in lines.iter().take(5) {
                                out.println(l);
                            }
                        }
                    }
                } else {
                    self.runtime
                        .append_memory_note("note", Some(rest.to_string()))
                        .await
                        .map_err(anyhow::Error::msg)?;
                    out.println("memory note saved");
                }
            }
            "/compact" => {
                if rest == "--preview" {
                    let preview = self.runtime.compaction_preview();
                    out.println(&format!("compaction preview:\n{}", preview));
                    return Ok(true);
                }
                let summary = self.runtime.compact_summary();
                self.runtime.set_session_summary(Some(summary.clone()));
                self.runtime
                    .append_memory_note("session-summary", Some(summary.clone()))
                    .await
                    .map_err(anyhow::Error::msg)?;
                self.runtime.save().await.map_err(anyhow::Error::msg)?;
                out.println(&format!("saved session summary:\n{}", summary));
            }
            "/models" => {
                let catalog =
                    dcode_ai_runtime::model_limits_api::fetch_provider_model_ids(self.runtime.config())
                        .await;
                let (provider_models, catalog_error) = match catalog {
                    Ok(models) => (models, None),
                    Err(error) => (Vec::new(), Some(error.to_string())),
                };
                let auth = dcode_ai_common::auth::AuthStore::load().unwrap_or_default();
                let connected = active_provider_connected(self.runtime.config(), &auth);
                if let ReplOutput::Tui(st) = &out {
                    let entries = build_model_picker_entries(self.runtime.config(), &provider_models);
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
                        out.println("no provider connected for current selection; run /connect or /login");
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
                                    dcode_ai_core::tools::mcp::load_mcp_tools(&manager, &[srv]).await
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
                } else {
                    // /mcp — show server status overview.
                    let mut lines: Vec<String> = vec![format!(
                        "MCP Servers ({} configured)",
                        servers.len()
                    )];
                    lines.push(String::new());
                    if servers.is_empty() {
                        lines.push("No servers configured.".into());
                        lines.push(String::new());
                        lines.push("Add to .dcode-ai/config.local.toml:".into());
                        lines.push("  [[mcp.servers]]".into());
                        lines.push("  name = \"my-server\"".into());
                        lines.push("  command = \"npx\"".into());
                        lines.push("  args = [\"-y\", \"@modelcontextprotocol/server-filesystem\"]".into());
                    } else {
                        for server in servers {
                            let status = if server.enabled { "●" } else { "○" };
                            let transport = if server.url.is_some() {
                                "http"
                            } else {
                                "stdio"
                            };
                            lines.push(format!(
                                "{status} {:<20} [{transport}] {}",
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
            "/agents" => {
                let snapshot = self.runtime.snapshot();
                let lines: Vec<String> = if snapshot.child_session_ids.is_empty() {
                    vec!["No child sessions yet.".into()]
                } else {
                    snapshot.child_session_ids.clone()
                };
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
            "/logs" => {
                match tokio::fs::read_to_string(self.runtime.event_log_path()).await {
                    Ok(data) => {
                        if let ReplOutput::Tui(st) = &out {
                            let lines: Vec<String> = data.lines().rev().take(100).map(String::from).collect();
                            let lines: Vec<String> = lines.into_iter().rev().collect();
                            if let Ok(mut g) = st.lock() {
                                g.open_info_modal("logs (last 100)", lines);
                            }
                        } else {
                            out.print(&data);
                        }
                    }
                    Err(err) => {
                        out.eprintln(&format!("failed to read log: {err}"))
                    }
                }
            }
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
            "/image" => {
                let st = match &out {
                    ReplOutput::Tui(st) => st,
                    ReplOutput::Stdio => {
                        out.eprintln(
                            "[image] stage images from the full-screen TUI (Ctrl+V, /image paste, /image <path>)",
                        );
                        return Ok(true);
                    }
                };
                let workspace = self.runtime.workspace_root().to_path_buf();
                let sid = self.runtime.session_id().to_string();
                let rest_trim = rest.trim();
                if rest_trim.is_empty() || rest_trim.eq_ignore_ascii_case("paste") {
                    match crate::image_attach::paste_clipboard_image(&workspace, &sid) {
                        Ok(att) => {
                            let path = att.path.clone();
                            let n = if let Ok(mut g) = st.lock() {
                                g.staged_image_attachments.push(att);
                                g.staged_image_attachments.len()
                            } else {
                                0
                            };
                            out.println(&format!(
                                "[image] staged {path} — press Enter to send ({n} attached)"
                            ));
                        }
                        Err(e) => out.eprintln(&format!("[image] {e}")),
                    }
                } else if rest_trim.eq_ignore_ascii_case("clear") {
                    if let Ok(mut g) = st.lock() {
                        g.staged_image_attachments.clear();
                    }
                    out.println("[image] cleared staged images");
                } else {
                    let path_text = rest_trim
                        .trim()
                        .trim_matches(|c| matches!(c, '\'' | '"' | '`'));
                    let p = std::path::Path::new(path_text);
                    match crate::image_attach::import_image_file(&workspace, &sid, p) {
                        Ok(att) => {
                            let path = att.path.clone();
                            let n = if let Ok(mut g) = st.lock() {
                                g.staged_image_attachments.push(att);
                                g.staged_image_attachments.len()
                            } else {
                                0
                            };
                            out.println(&format!(
                                "[image] staged {path} — press Enter to send ({n} attached)"
                            ));
                        }
                        Err(e) => out.eprintln(&format!("[image] {e}")),
                    }
                }
            }
            "/config" => {
                let config = self.runtime.config();
                let lines = vec![
                    format!("Provider:    {}", config.provider.default.display_name()),
                    format!("Model:       {}", self.runtime.model()),
                    format!("Permission:  {:?}", self.runtime.permission_mode()),
                    format!("Memory:      {}", self.runtime.memory_store_path().display()),
                    format!("Editor:      {}", config.effective_editor_command()),
                    format!("Thinking:    {} (budget: {})", config.model.enable_thinking, config.model.thinking_budget),
                    format!("Max tokens:  {}", config.model.max_tokens),
                    String::new(),
                    "Provider endpoints:".into(),
                    format!("  OpenAI:      {}", config.provider.base_url_for(ProviderKind::OpenAi)),
                    format!("  Anthropic:   {}", config.provider.base_url_for(ProviderKind::Anthropic)),
                    format!("  OpenRouter:  {}", config.provider.base_url_for(ProviderKind::OpenRouter)),
                    format!("  Antigravity: {}", config.provider.base_url_for(ProviderKind::Antigravity)),
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
                    // OpenCode Zen has no automatic OAuth callback — the official
                    // flow (same as `opencode auth login`) is a browser sign-in
                    // where you copy your API key. Open opencode.ai/auth and
                    // prompt for the key, rather than the dead callback flow.
                    if matches!(
                        provider_hint.to_ascii_lowercase().as_str(),
                        "opencodezen" | "opencode" | "zen" | "minimax"
                    ) {
                        let opened =
                            crate::oauth_login::try_open_browser("https://opencode.ai/auth");
                        if let ReplOutput::Tui(st) = &out {
                            out.println(if opened {
                                "[login] Opened opencode.ai/auth — sign in, copy your API key, and paste it here."
                            } else {
                                "[login] Go to https://opencode.ai/auth, sign in, copy your API key, and paste it here."
                            });
                            if let Ok(mut g) = st.lock() {
                                g.open_api_key_modal(
                                    ProviderKind::OpenCodeZen,
                                    self.runtime
                                        .config()
                                        .provider
                                        .api_key_present_for(ProviderKind::OpenCodeZen),
                                    true,
                                );
                            }
                        } else {
                            out.println(
                                "OpenCode Zen login: open https://opencode.ai/auth, sign in, copy your API key, then set OPENCODE_API_KEY or provider.opencodezen.api_key in config.",
                            );
                        }
                        return Ok(true);
                    }
                    if let Some((label, base_url)) = local_preset_for(provider_hint) {
                        self.connect_local_preset(label, base_url, &out).await?;
                        return Ok(true);
                    }
                    // "Use a Google Cloud project" (Antigravity CLI option 2):
                    // Gemini on Vertex AI, billed to the user's own project,
                    // authenticated via gcloud ADC.
                    let mut hint_tokens = provider_hint.split_whitespace();
                    if matches!(
                        hint_tokens.next().map(str::to_ascii_lowercase).as_deref(),
                        Some("vertex" | "gcp" | "cloudproject" | "cloud-project")
                    ) {
                        let project = hint_tokens.next().map(str::to_string);
                        let location = hint_tokens
                            .next()
                            .map(str::to_string)
                            .unwrap_or_else(dcode_ai_common::auth::default_vertex_location);
                        self.connect_vertex_project(project, location, &out).await?;
                        return Ok(true);
                    }
                    if let Some(oauth_provider) = parse_oauth_provider(provider_hint) {
                        if let ReplOutput::Tui(st) = &out
                            && matches!(oauth_provider, OAuthProvider::Anthropic)
                        {
                            let prompt = crate::oauth_login::begin_anthropic_login_prompt();
                            let opened = crate::oauth_login::try_open_browser(&prompt.authorization_url);
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
                    } else if let Some(p) = ProviderKind::from_cli_name(provider_hint)
                        .or_else(|| ProviderKind::parse_display_name(provider_hint))
                    {
                        if let ReplOutput::Tui(st) = &out {
                            if oauth_only_provider(p) {
                                if let Some(oauth_provider) =
                                    oauth_login_slug_for_provider(p).and_then(parse_oauth_provider)
                                {
                                    out.println(&format!(
                                        "[login] {} uses OAuth. Running login flow…",
                                        p.display_name(),
                                    ));
                                    match crate::oauth_login::login_with_output(
                                        oauth_provider,
                                        |line| out.println(line),
                                    )
                                    .await
                                    {
                                        Ok(()) => {
                                            self.apply_provider_after_oauth_login(oauth_provider, &out)
                                            .await?;
                                        }
                                        Err(e) => out.eprintln(&format!("[login] {e}")),
                                    }
                                }
                            } else if let Ok(mut g) = st.lock() {
                                g.open_api_key_modal(
                                    p,
                                    self.runtime.config().provider.api_key_present_for(p),
                                    false,
                                );
                            }
                        } else if oauth_only_provider(p) {
                            out.println(&format!("dcode-ai login {}", provider_hint.to_lowercase()));
                        } else {
                            out.println(&format!("dcode-ai apikey {} <secret>", provider_hint));
                        }
                    } else {
                        out.eprintln(
                            "unknown provider. OAuth: openai/codex, anthropic, copilot, antigravity. API-key: openrouter, opencodezen",
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
                    out.println("  /model <name>                 — set model after switching provider");
                    out.println(&format!(
                        "  current: {} → {}",
                        self.runtime.config().provider.default.display_name(),
                        self.runtime.model()
                    ));
                }
            }
            "/logout" => {
                let target = rest.trim();
                let parsed = if target.is_empty() {
                    Some(LogoutTarget::All)
                } else {
                    parse_logout_target(target)
                };
                if let Some(target) = parsed {
                    match crate::oauth_login::logout_with_output(target, |line| out.println(line))
                    {
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
                    if store.anthropic.is_some() {
                        "logged in"
                    } else if has_claude_cli() {
                        "local claude cli"
                    } else {
                        "not logged in"
                    }
                ));
                out.println(&format!(
                    "  openai:      {}",
                    if store.openai_oauth.is_some() {
                        "logged in"
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
                out.println(&format!(
                    "  antigravity: {}",
                    if store.antigravity.is_some() {
                        "logged in"
                    } else {
                        "not logged in"
                    }
                ));
                out.println(&format!(
                    "  vertex:      {}",
                    match &store.vertex {
                        Some(v) => format!("project {} ({})", v.project_id, v.location),
                        None => "not connected".to_string(),
                    }
                ));
                out.println(&format!(
                    "  opencodezen: {}",
                    if store.opencodezen_oauth.is_some() {
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
                    format!("  Provider:    {}", self.runtime.config().provider.default.display_name()),
                    format!("  Model:       {}", self.runtime.model()),
                    format!("  Editor:      {}", self.runtime.config().effective_editor_command()),
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
                    let names: Vec<String> = theme::ALL_THEMES
                        .iter()
                        .map(|t| t.name.to_string())
                        .collect();
                    if let ReplOutput::Tui(st) = out {
                        let cur_idx = names
                            .iter()
                            .position(|n| n == active.name)
                            .unwrap_or(0);
                        if let Ok(mut g) = st.lock() {
                            g.open_theme_picker(names, cur_idx);
                        }
                        out.println("[theme] choose with ↑↓ + Enter, Esc to cancel");
                    } else {
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
                    if let Err(e) = self
                        .runtime
                        .config()
                        .save_workspace_file(self.runtime.workspace_root())
                    {
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
                    let _ = crossterm::execute!(
                        std::io::stdout(),
                        crossterm::terminal::SetTitle(t)
                    );
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
            "/hooks" => {
                let h = &self.runtime.config().hooks;
                let groups: [(&str, &[dcode_ai_common::config::HookCommand]); 8] = [
                    ("session_start", &h.session_start),
                    ("session_end", &h.session_end),
                    ("pre_tool_use", &h.pre_tool_use),
                    ("post_tool_use", &h.post_tool_use),
                    ("post_tool_failure", &h.post_tool_failure),
                    ("approval_requested", &h.approval_requested),
                    ("subagent_start", &h.subagent_start),
                    ("subagent_stop", &h.subagent_stop),
                ];
                let mut lines = vec!["Lifecycle hooks:".to_string(), String::new()];
                let mut any = false;
                for (name, cmds) in groups {
                    for c in cmds {
                        any = true;
                        let m = c
                            .matcher
                            .as_deref()
                            .map(|m| format!(" [{m}]"))
                            .unwrap_or_default();
                        let block = if c.blocking { " (blocking)" } else { "" };
                        lines.push(format!("  {name}{m}{block}: {}", c.command));
                    }
                }
                if !any {
                    lines.push("  (none configured — add under [hooks] in .dcode.toml)".into());
                }
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_info_modal("hooks", lines.clone());
                    }
                } else {
                    for l in &lines {
                        out.println(l);
                    }
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
                    "palette", "search", "history", "pin", "expand", "subagents", "thinking",
                    "clear",
                ];
                if args.is_empty() {
                    out.println(&format!("[keymap] remappable actions: {}", actions.join(", ")));
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
                    } else if crate::tui::app::parse_key_combo(&key).is_none() {
                        out.eprintln(&format!(
                            "[keymap] could not parse key '{key}' (try ctrl+b, alt+enter, f5)"
                        ));
                    } else {
                        self.runtime
                            .config_mut()
                            .ui
                            .keymap
                            .insert(action.clone(), key.clone());
                        let bindings =
                            crate::tui::app::build_key_bindings(&self.runtime.config().ui.keymap);
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
                        out.println(
                            "usage: /provider <openai|codex|copilot|anthropic|openrouter|antigravity|opencodezen>",
                        );
                    }
                } else if rest.eq_ignore_ascii_case("codex") {
                    self.apply_codex_provider_in_session(out).await?;
                } else if rest.eq_ignore_ascii_case("copilot")
                    || rest.eq_ignore_ascii_case("github")
                {
                    self.apply_copilot_provider_in_session(out).await?;
                } else if let Some(p) = ProviderKind::from_cli_name(rest)
                    .or_else(|| ProviderKind::parse_display_name(rest))
                {
                    self.apply_provider_in_session(p, out).await?;
                } else {
                    out.eprintln(
                        "unknown provider; try: openai/codex, copilot, anthropic, openrouter, antigravity, opencodezen",
                    );
                }
            }
            "/apikey" => {
                let mut toks = rest.split_whitespace();
                let p_name = toks.next();
                let key = toks.collect::<Vec<_>>().join(" ");
                let key = key.trim();
                if let Some(pn) = p_name {
                    let p = ProviderKind::from_cli_name(pn)
                        .or_else(|| ProviderKind::parse_display_name(pn));
                    if let Some(p) = p {
                        if oauth_only_provider(p) {
                            let login = oauth_login_slug_for_provider(p).unwrap_or("openai");
                            out.eprintln(&format!(
                                "{} uses OAuth login. Run: dcode-ai login {}",
                                p.display_name(),
                                login
                            ));
                            return Ok(true);
                        }
                        if key.is_empty() {
                            if let ReplOutput::Tui(st) = out {
                                if let Ok(mut g) = st.lock() {
                                    g.open_api_key_modal(
                                        p,
                                        self.runtime.config().provider.api_key_present_for(p),
                                        false,
                                    );
                                }
                            } else {
                                out.println("usage: /apikey <provider> <secret|remove>");
                            }
                        } else if matches!(key, "remove" | "clear" | "unset") {
                            // Clear both the credentials store and any inline
                            // plaintext key left in config.
                            let mut cfg = self.runtime.config().clone();
                            let env_name = cfg.provider.api_key_env_for(p).to_string();
                            match dcode_ai_common::credentials::remove(&env_name) {
                                Ok(removed) => {
                                    cfg.set_provider_api_key(p, "");
                                    let _ = cfg.save_global();
                                    let _ = self.runtime.apply_dcode_ai_config(cfg);
                                    out.println(&format!(
                                        "[apikey] {} for {} (env var {} still applies if set)",
                                        if removed { "removed" } else { "nothing stored" },
                                        p.display_name(),
                                        env_name
                                    ));
                                }
                                Err(e) => out.eprintln(&format!("[apikey] remove failed: {e}")),
                            }
                        } else {
                            self.save_provider_api_key(p, key, out).await?;
                        }
                    } else {
                        out.eprintln(
                            "unknown provider; try: openai, copilot, anthropic, openrouter, antigravity, opencodezen",
                        );
                    }
                } else if let ReplOutput::Tui(st) = out {
                    if let Ok(mut g) = st.lock() {
                        g.open_provider_picker(self.runtime.config().provider.default, true);
                    }
                    out.println("[apikey] pick provider, then paste key + Enter");
                } else {
                    out.println("usage: /apikey <provider> <secret>");
                }
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
            "/doctor" => {
                let mut lines = Vec::new();
                for provider in dcode_ai_common::config::ProviderKind::ALL {
                    let configured = self
                        .runtime
                        .config()
                        .provider
                        .api_key_present_for(provider);
                    lines.push(format!(
                        "{}{} API key {} ({})",
                        provider.display_name(),
                        if provider == self.runtime.config().provider.default {
                            " [selected]"
                        } else {
                            ""
                        },
                        if configured { "✓ configured" } else { "✗ missing" },
                        self.runtime.config().provider.api_key_env_for(provider)
                    ));
                }
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.open_info_modal("doctor", lines);
                    }
                } else {
                    for l in &lines {
                        out.println(l);
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
                        let mut entries: Vec<SessionPickerEntry> = Vec::new();
                        for id in &ids {
                            let (label, search_text) = match store.load_snapshot(id).await {
                                Ok(snapshot) => {
                                    if let Some(name) = snapshot.session_name {
                                        (format!("{name} ({id})"), format!("{name} {id}"))
                                    } else {
                                        (id.clone(), id.clone())
                                    }
                                }
                                Err(_) => (id.clone(), id.clone()),
                            };
                            let preview =
                                session_event_preview(&sessions_dir, id, 3).unwrap_or_default();
                            entries.push(SessionPickerEntry {
                                id: id.clone(),
                                label,
                                search_text,
                                preview,
                            });
                        }
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
                    let instructions_content =
                        "# Project Instructions for dcode-ai\n\n\
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
                            Err(e) => out.eprintln(&format!("[init] Failed to write instructions.md: {e}")),
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
                    out.println("[init] Tip: add `.dcode-ai/` to your .gitignore (sessions + local config)");
                    out.println("[init] Tip: commit `.dcode.toml` to share project config with teammates");
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
            "/new" => {
                let summary = self.runtime.compact_summary();
                self.runtime.set_session_summary(Some(summary.clone()));
                self.runtime
                    .append_memory_note("session-summary", Some(summary))
                    .await
                    .map_err(anyhow::Error::msg)?;
                self.runtime.save().await.map_err(anyhow::Error::msg)?;
                self.runtime.new_session().await.map_err(anyhow::Error::msg)?;
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
                        let mut md_lines = vec![
                            format!("# Session {}", snapshot.id),
                            String::new(),
                        ];
                        for line in raw.lines() {
                            if let Ok(val) = serde_json::from_str::<serde_json::Value>(line)
                                && let Some(kind) = val.get("kind").and_then(|v| v.as_str())
                            {
                                match kind {
                                    "MessageReceived" => {
                                        let role = val.get("role").and_then(|v| v.as_str()).unwrap_or("?");
                                        let content =
                                            val.get("content").and_then(|v| v.as_str()).unwrap_or("");
                                        md_lines.push(format!("## {role}"));
                                        md_lines.push(String::new());
                                        md_lines.push(content.to_string());
                                        md_lines.push(String::new());
                                    }
                                    "ToolCallStarted" => {
                                        let tool = val.get("tool").and_then(|v| v.as_str()).unwrap_or("?");
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
                let export_path = self.runtime.workspace_root().join(format!(".dcode-ai/export-{}.md", snapshot.id));
                if let Some(parent) = export_path.parent() {
                    let _ = tokio::fs::create_dir_all(parent).await;
                }
                match tokio::fs::write(&export_path, &md).await {
                    Ok(()) => out.println(&format!("exported to {}", export_path.display())),
                    Err(e) => out.eprintln(&format!("[export] {e}")),
                }
            }
            "/thinking" => {
                let mut cfg = self.runtime.config().clone();
                cfg.model.enable_thinking = !cfg.model.enable_thinking;
                let new_state = cfg.model.enable_thinking;
                match self.runtime.apply_dcode_ai_config(cfg) {
                    Ok(()) => {
                        if let Err(e) = self.runtime.config().save_workspace_file(self.runtime.workspace_root()) {
                            out.eprintln(&format!("[thinking] toggled but save failed: {e}"));
                        } else {
                            out.println(&format!("thinking {} (budget: {})", if new_state { "enabled" } else { "disabled" }, self.runtime.config().model.thinking_budget));
                        }
                    }
                    Err(e) => out.eprintln(&format!("[thinking] {e}")),
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

            // ── /run <shell cmd> ──────────────────────────────────────────────
            "/run" => {
                if rest.trim().is_empty() {
                    out.eprintln("[run] usage: /run <shell command>");
                    return Ok(true);
                }
                let shell_cmd = rest.trim().to_string();
                out.println(&format!("[run] $ {shell_cmd}"));
                let result = Command::new(if cfg!(windows) { "cmd" } else { "sh" })
                    .args(if cfg!(windows) {
                        vec!["/C", &shell_cmd]
                    } else {
                        vec!["-c", &shell_cmd]
                    })
                    .current_dir(self.runtime.workspace_root())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .output()
                    .await;
                match result {
                    Ok(cmd_out) => {
                        let stdout = String::from_utf8_lossy(&cmd_out.stdout).to_string();
                        let stderr = String::from_utf8_lossy(&cmd_out.stderr).to_string();
                        let combined = if stderr.is_empty() {
                            stdout
                        } else if stdout.is_empty() {
                            stderr
                        } else {
                            format!("{stdout}{stderr}")
                        };
                        const MAX_SHOW: usize = 8_000;
                        let truncated = if combined.len() > MAX_SHOW {
                            let mut end = MAX_SHOW;
                            while end > 0 && !combined.is_char_boundary(end) {
                                end -= 1;
                            }
                            format!("{}…\n[output truncated]", &combined[..end])
                        } else {
                            combined.clone()
                        };
                        if truncated.is_empty() {
                            out.println("[run] (no output)");
                        } else {
                            out.print(&truncated);
                        }
                        let exit_label = if cmd_out.status.success() {
                            "[exit 0]".to_string()
                        } else {
                            format!("[exit {}]", cmd_out.status.code().unwrap_or(-1))
                        };
                        out.println(&format!("[run] {exit_label} — output staged (send a message to use it)"));
                        // Stage the output so the user's next message carries it.
                        if let ReplOutput::Tui(st) = &out
                            && let Ok(mut g) = st.lock()
                        {
                            g.pending_context.push(format!(
                                "Output of `{shell_cmd}` ({exit_label}):\n```\n{truncated}\n```"
                            ));
                        }
                    }
                    Err(e) => out.eprintln(&format!("[run] error: {e}")),
                }
            }

            // ── /web <url> ────────────────────────────────────────────────────
            "/web" => {
                let arg = rest.trim();
                if arg.is_empty() {
                    out.eprintln("[web] usage: /web <url | search query>");
                    return Ok(true);
                }
                // Treat the argument as a URL when it has a scheme, or looks like
                // a bare domain (no spaces, has a dot). Otherwise it's a search
                // query → fetch DuckDuckGo results and stage them as context.
                let is_url = arg.starts_with("http://")
                    || arg.starts_with("https://")
                    || (!arg.contains(char::is_whitespace)
                        && arg.contains('.')
                        && !arg.ends_with('.'));

                let (source_url, fetch_url, label) = if is_url {
                    let url = if arg.starts_with("http") {
                        arg.to_string()
                    } else {
                        format!("https://{arg}")
                    };
                    out.println(&format!("[web] Fetching {url}…"));
                    (url.clone(), url, format!("Content fetched from <{arg}>"))
                } else {
                    out.println(&format!("[web] Searching the web for: {arg}…"));
                    let search_url = match reqwest::Url::parse_with_params(
                        "https://html.duckduckgo.com/html/",
                        &[("q", arg)],
                    ) {
                        Ok(u) => u.to_string(),
                        Err(_) => {
                            out.eprintln("[web] could not build search URL");
                            return Ok(true);
                        }
                    };
                    (
                        arg.to_string(),
                        search_url,
                        format!("Web search results for \"{arg}\""),
                    )
                };

                match fetch_url_as_text(&fetch_url).await {
                    Ok(text) => {
                        let preview: String = text.lines().take(3).collect::<Vec<_>>().join(" ");
                        let preview = truncate_str_bytes(&preview, 80);
                        out.println(&format!("[web] {} lines: {preview}…", text.lines().count()));
                        out.println("[web] Staged — send a message to use this context");
                        let block = format!("{label}:\n```text\n{text}\n```");
                        if let ReplOutput::Tui(st) = &out {
                            if let Ok(mut g) = st.lock() {
                                g.pending_context.push(block);
                            }
                        } else {
                            // In stdio mode: run a turn that uses the fetched text.
                            let prompt = format!(
                                "{label} <{source_url}>:\n\n```\n{text}\n```\n\nSummarize the most relevant findings."
                            );
                            let _ = self.runtime.run_turn(&prompt).await;
                        }
                    }
                    Err(e) => out.eprintln(&format!("[web] failed: {e}")),
                }
            }

            // ── /commit ───────────────────────────────────────────────────────
            "/commit" => {
                // Check for staged changes.
                let diff_out = Command::new("git")
                    .args(["diff", "--staged"])
                    .current_dir(self.runtime.workspace_root())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .output()
                    .await;
                let diff = match diff_out {
                    Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
                    Err(e) => {
                        out.eprintln(&format!("[commit] git error: {e}"));
                        return Ok(true);
                    }
                };
                if diff.trim().is_empty() {
                    out.eprintln("[commit] No staged changes — run `git add` first");
                    return Ok(true);
                }
                out.println("[commit] Generating commit message…");
                let prompt = format!(
                    "Write a git commit message for the following staged diff.\n\
                     Use Conventional Commits format (feat/fix/chore/docs/…).\n\
                     Output ONLY the commit message text — no code fences, no extra commentary.\n\
                     Keep the subject line ≤72 chars.\n\
                     \n\
                     ```diff\n{diff}\n```"
                );
                let msg = match self.runtime.run_turn(&prompt).await {
                    Ok(m) => m,
                    Err(e) => {
                        out.eprintln(&format!("[commit] model error: {e}"));
                        return Ok(true);
                    }
                };
                // Strip fences if model wrapped the message anyway.
                let msg = msg
                    .trim()
                    .trim_start_matches("```")
                    .trim_end_matches("```")
                    .trim()
                    .to_string();
                if msg.is_empty() {
                    out.eprintln("[commit] Model returned an empty message — aborting");
                    return Ok(true);
                }
                let commit_out = Command::new("git")
                    .args(["commit", "-m", &msg])
                    .current_dir(self.runtime.workspace_root())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .output()
                    .await;
                match commit_out {
                    Ok(o) if o.status.success() => {
                        let first_line = msg.lines().next().unwrap_or(&msg);
                        out.println(&format!("[commit] ✓ {first_line}"));
                    }
                    Ok(o) => {
                        let stderr = String::from_utf8_lossy(&o.stderr);
                        out.eprintln(&format!("[commit] git commit failed: {stderr}"));
                    }
                    Err(e) => out.eprintln(&format!("[commit] error: {e}")),
                }
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
                            out.println("[project] no projects yet. Add one with: /project add <path>");
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
                        let block = format!(
                            "Imported Claude Code chat \"{title}\":\n```text\n{text}\n```"
                        );
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
                                Err(e) => {
                                    out.eprintln(&format!("[import] failed to write AGENTS.md: {e}"))
                                }
                            }
                        }
                    }
                    Err(_) => {
                        out.println("[import] No CLAUDE.md found in this workspace — nothing to import.");
                        out.println("  (Imports CLAUDE.md → AGENTS.md. For prior chats, use /import chats.)");
                    }
                }
            }

            // ── /effort <level> ───────────────────────────────────────────────
            "/effort" => {
                let level = rest.trim().to_ascii_lowercase();
                let (thinking, budget, max_tok) = match level.as_str() {
                    "low" | "l" => (false, 0, 4096),
                    "medium" | "m" | "med" => (false, 0, 8192),
                    "high" | "h" => (true, 8192, 16384),
                    "xhigh" | "x" | "max" => (true, 32768, 32768),
                    "" => {
                        let cfg = self.runtime.config();
                        let current = if !cfg.model.enable_thinking {
                            if cfg.model.max_tokens <= 4096 {
                                "low"
                            } else {
                                "medium"
                            }
                        } else if cfg.model.thinking_budget >= 32768 {
                            "xhigh"
                        } else {
                            "high"
                        };
                        out.println(&format!("[effort] current: {current} (thinking={}, budget={}, max_tokens={})",
                            cfg.model.enable_thinking, cfg.model.thinking_budget, cfg.model.max_tokens));
                        return Ok(true);
                    }
                    _ => {
                        out.eprintln("[effort] usage: /effort low|medium|high|xhigh");
                        return Ok(true);
                    }
                };
                let mut cfg = self.runtime.config().clone();
                cfg.model.enable_thinking = thinking;
                cfg.model.thinking_budget = budget;
                cfg.model.max_tokens = max_tok;
                match self.runtime.apply_dcode_ai_config(cfg) {
                    Ok(()) => {
                        // Keep the status-bar effort chip in sync.
                        if let ReplOutput::Tui(st) = &out
                            && let Ok(mut g) = st.lock()
                        {
                            g.thinking_enabled = thinking;
                            g.thinking_budget = budget;
                        }
                        out.println(&format!(
                            "[effort] set to {level} (thinking={thinking}, budget={budget}, max_tokens={max_tok})"
                        ));
                    }
                    Err(e) => out.eprintln(&format!("[effort] {e}")),
                }
            }

            // ── /history <query> ──────────────────────────────────────────
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
                    out.println(&format!("[history] {} match(es) for \"{query}\":", results.len()));
                    for r in &results {
                        out.println(r);
                    }
                }
            }

            _ => {
                if command.starts_with('/')
                    && self
                        .try_run_skill(command.trim_start_matches('/'), rest, &out)
                        .await?
                {
                    return Ok(true);
                }
                out.eprintln(&format!("unknown command: {command}"));
            }
        }

        Ok(true)
    }

    async fn run_preset(
        &mut self,
        prefix: &str,
        task: &str,
        out: ReplOutput<'_>,
    ) -> anyhow::Result<()> {
        if task.trim().is_empty() {
            out.println("usage: /<command> <task description>");
            return Ok(());
        }
        let prompt = format!("{prefix}{}", task.trim());
        let prompt = match expand_at_file_mentions_default(&prompt, self.runtime.workspace_root()) {
            Ok(s) => s,
            Err(e) => {
                out.eprintln(&format!("file mentions: {e}"));
                return Ok(());
            }
        };
        match self.runtime.run_turn(&prompt).await {
            Ok(output) => {
                if matches!(out, ReplOutput::Stdio) {
                    out.println(&output);
                }
            }
            Err(err) => {
                out.eprintln(&format!("error: {err}"));
            }
        }
        Ok(())
    }

    async fn try_run_skill(
        &mut self,
        skill_name: &str,
        task: &str,
        out: &ReplOutput<'_>,
    ) -> anyhow::Result<bool> {
        let skills = SkillCatalog::discover(
            self.runtime.workspace_root(),
            &self.runtime.config().harness.skill_directories,
        )
        .map_err(anyhow::Error::msg)?;
        let Some(skill) = skills.into_iter().find(|skill| skill.command == skill_name) else {
            return Ok(false);
        };

        if let Some(model) = &skill.model {
            self.runtime
                .set_model(self.runtime.config().model.resolve_alias(model));
        }
        if let Some(mode) = skill.permission_mode {
            self.runtime.set_permission_mode(mode);
        }

        let prompt = skill.prompt_for_task(task);
        let prompt = match expand_at_file_mentions_default(&prompt, self.runtime.workspace_root()) {
            Ok(s) => s,
            Err(e) => {
                out.eprintln(&format!("file mentions: {e}"));
                return Ok(true);
            }
        };
        match self.runtime.run_turn(&prompt).await {
            Ok(output) => {
                if matches!(out, ReplOutput::Stdio) {
                    out.println(&output);
                }
            }
            Err(err) => {
                out.eprintln(&format!("error: {err}"));
            }
        }
        Ok(true)
    }

    /// Full-screen TUI: transcript + streaming + composer (default on TTY).
    pub async fn run_with_tui(&mut self) -> anyhow::Result<()> {
        // Apply the configured theme before the TUI draws its first frame.
        crate::tui::theme::set_by_name(self.runtime.config().ui.theme.as_deref());

        let session_id = self.runtime.session_id().to_string();
        let model = self.runtime.model().to_string();
        let perm = format!("{:?}", self.runtime.permission_mode());
        let tui_state: Arc<Mutex<TuiSessionState>> = Arc::new(Mutex::new(TuiSessionState::new(
            session_id,
            model,
            self.current_agent_label.clone(),
            perm,
            self.runtime.workspace_root().to_path_buf(),
            self.runtime.config().ui.code_line_numbers,
        )));
        // Koda-style fullscreen behavior: mouse capture is always on for
        // wheel scroll + in-app drag-select copy.
        let effective_mouse_capture = true;
        if let Ok(mut g) = tui_state.lock() {
            g.mouse_capture_on = effective_mouse_capture;
            g.notifications_enabled = self.runtime.config().ui.notifications;
            g.mcp_server_count = self
                .runtime
                .config()
                .mcp
                .servers
                .iter()
                .filter(|s| s.enabled)
                .count();
            g.thinking_enabled = self.runtime.config().model.enable_thinking;
            g.thinking_budget = self.runtime.config().model.thinking_budget;
            g.statusline_hidden = self.runtime.config().ui.statusline_hidden.clone();
            g.quiet_startup = self.runtime.config().ui.quiet_startup;
            g.key_bindings = crate::tui::app::build_key_bindings(&self.runtime.config().ui.keymap);
        }

        let log_path = self.runtime.event_log_path();
        replay_event_log_into_state(&log_path, &tui_state).await;

        // Populate the git branch name immediately so it appears on first render.
        let workspace = self.runtime.workspace_root();
        if let Some(branch) = git_current_branch(workspace)
            && let Ok(mut g) = tui_state.lock()
        {
            g.set_current_branch(&branch);
        }

        if self.should_offer_startup_approve_all_popup()
            && let Ok(mut g) = tui_state.lock()
            && g.active_question.is_none()
        {
            g.active_question = Some(InteractiveQuestionPayload {
                question_id: STARTUP_APPROVE_ALL_QUESTION_ID.to_string(),
                call_id: STARTUP_APPROVE_ALL_QUESTION_ID.to_string(),
                prompt: "Grant one-time approval for ALL tools in this session?".into(),
                options: vec![
                    QuestionOption {
                        id: "approve_all".into(),
                        label: "Approve all tools (session only)".into(),
                    },
                    QuestionOption {
                        id: "keep_default".into(),
                        label: "Keep default approval flow".into(),
                    },
                ],
                allow_custom: false,
                suggested_answer: "approve_all".into(),
            });
            g.open_question_modal();
        }

        let rx = self
            .runtime
            .take_event_rx()
            .ok_or_else(|| anyhow::anyhow!("internal: event channel already taken"))?;
        let ipc = self.runtime.take_ipc_handle();
        let approval = self.runtime.take_ipc_approval_pending();
        let question = self.runtime.question_pending();
        let bridge_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>> =
            Arc::new(Mutex::new(Some(spawn_tui_bridge(
                rx,
                log_path,
                ipc,
                approval.clone(),
                question.clone(),
                tui_state.clone(),
            ))));

        let _spawn_task = {
            let spawn_rx = self.runtime.take_spawn_rx();
            let event_tx = self.runtime.event_tx();
            if let Some(srx) = spawn_rx {
                Some(dcode_ai_runtime::supervisor::spawn_subagent_consumer(
                    srx,
                    self.runtime.session_id().to_string(),
                    self.runtime.workspace_root().to_path_buf(),
                    self.runtime.config().clone(),
                    self.runtime.messages().to_vec(),
                    event_tx,
                ))
            } else {
                None
            }
        };

        // Answers must bypass the main `cmd_rx` loop: while `run_turn` is blocked inside
        // `ask_question`, that task never receives `TuiCmd::Submit` or `QuestionAnswer`.
        let (answer_tx, mut answer_rx) =
            tokio::sync::mpsc::unbounded_channel::<(String, QuestionSelection)>();
        let qp_dispatch = question.clone();
        let question_state = tui_state.clone();
        tokio::spawn(async move {
            while let Some((qid, sel)) = answer_rx.recv().await {
                if !dispatch_question_answer(&qp_dispatch, &qid, sel)
                    && let Ok(mut g) = question_state.lock()
                {
                    if qid == STARTUP_APPROVE_ALL_QUESTION_ID {
                        continue;
                    }
                    // A stale answer (double-submit, post-cancel) is not an error;
                    // clear any leftover prompt UI silently like Codex does.
                    if g.active_question.as_ref().map(|q| q.question_id.as_str())
                        == Some(qid.as_str())
                    {
                        g.active_question = None;
                        g.close_question_modal();
                    }
                }
            }
        });
        let answer_for_tui = answer_tx.clone();
        drop(answer_tx);

        let (approval_tx, mut approval_rx) =
            tokio::sync::mpsc::unbounded_channel::<ApprovalAnswer>();
        let approval_dispatch = approval.clone();
        let approval_state = tui_state.clone();
        tokio::spawn(async move {
            while let Some(answer) = approval_rx.recv().await {
                let (call_id, verdict) = match answer {
                    ApprovalAnswer::Verdict { call_id, approved } => (
                        call_id,
                        if approved {
                            dcode_ai_core::approval::ApprovalVerdict::Approved
                        } else {
                            dcode_ai_core::approval::ApprovalVerdict::Denied
                        },
                    ),
                    ApprovalAnswer::AllowPattern { call_id, pattern } => (
                        call_id,
                        dcode_ai_core::approval::ApprovalVerdict::AllowPattern(pattern),
                    ),
                    ApprovalAnswer::ModifiedApproval {
                        call_id,
                        modified_input,
                    } => (
                        call_id,
                        dcode_ai_core::approval::ApprovalVerdict::ApprovedModified(modified_input),
                    ),
                };
                if !dispatch_tool_approval(&approval_dispatch, &call_id, verdict)
                    && let Ok(mut g) = approval_state.lock()
                {
                    // A verdict with no matching pending entry is normal, not an
                    // error: it happens on a double-press, after the turn was
                    // cancelled, or after the approval timed out. Codex silently
                    // ignores these; clear any stale prompt UI without scaring the
                    // user with a red error line.
                    g.clear_active_approval_if_matches(&call_id);
                }
            }
        });
        let approval_for_tui = approval_tx.clone();
        drop(approval_tx);

        let (cmd_tx, mut cmd_rx) = tokio::sync::mpsc::unbounded_channel::<TuiCmd>();
        let st = tui_state.clone();
        let banner = self.run_mode;
        let cancel_flag = self.runtime.cancel_handle();
        let mouse_capture = effective_mouse_capture;
        let scroll_speed = self.runtime.config().ui.scroll_speed;
        let ui = tokio::task::spawn_blocking(move || {
            run_blocking(
                st,
                cmd_tx,
                Some(answer_for_tui),
                Some(approval_for_tui),
                banner,
                Some(cancel_flag),
                mouse_capture,
                scroll_speed,
            )
        });

        let mut queued_steering: VecDeque<String> = VecDeque::new();
        let mut queued_followup: VecDeque<String> = VecDeque::new();
        let mut pending_cmds: VecDeque<TuiCmd> = VecDeque::new();

        loop {
            if pending_cmds.is_empty() && queued_steering.is_empty() && queued_followup.is_empty() {
                let cmd = cmd_rx.recv().await;
                let Some(cmd) = cmd else { break };
                pending_cmds.push_back(cmd);
            }

            while let Ok(cmd) = cmd_rx.try_recv() {
                pending_cmds.push_back(cmd);
            }

            let mut retained: VecDeque<TuiCmd> = VecDeque::new();
            while let Some(cmd) = pending_cmds.pop_front() {
                match cmd {
                    TuiCmd::QueueSteering(line) => queued_steering.push_back(line),
                    TuiCmd::QueueFollowUp(line) => queued_followup.push_back(line),
                    other => retained.push_back(other),
                }
            }
            pending_cmds = retained;

            if let Ok(mut g) = tui_state.lock() {
                g.queued_steering = queued_steering.len();
                g.queued_followup = queued_followup.len();
                g.queue_preview_items = queued_followup
                    .iter()
                    .chain(queued_steering.iter())
                    .take(6)
                    .map(|s| truncate_chars(s, 80))
                    .collect();
            }

            let cmd = if let Some(cmd) = pending_cmds.pop_front() {
                cmd
            } else if let Some(line) = queued_steering.pop_front() {
                if let Ok(mut g) = tui_state.lock() {
                    g.queued_steering = queued_steering.len();
                    g.queued_followup = queued_followup.len();
                    g.queue_preview_items = queued_followup
                        .iter()
                        .chain(queued_steering.iter())
                        .take(6)
                        .map(|s| truncate_chars(s, 80))
                        .collect();
                }
                TuiCmd::Submit(line)
            } else if let Some(line) = queued_followup.pop_front() {
                if let Ok(mut g) = tui_state.lock() {
                    g.queued_steering = queued_steering.len();
                    g.queued_followup = queued_followup.len();
                    g.queue_preview_items = queued_followup
                        .iter()
                        .chain(queued_steering.iter())
                        .take(6)
                        .map(|s| truncate_chars(s, 80))
                        .collect();
                }
                TuiCmd::Submit(line)
            } else {
                continue;
            };

            match cmd {
                TuiCmd::Backtrack {
                    user_index_from_end,
                    text,
                } => {
                    match self
                        .runtime
                        .rewind_to_user_message(user_index_from_end, &text)
                    {
                        Ok(()) => {
                            let _ = self.runtime.save().await;
                            if let Ok(mut g) = tui_state.lock() {
                                g.push_block(DisplayBlock::System(
                                    "Rewound — edit the restored message and press Enter".into(),
                                ));
                                g.touch_transcript();
                            }
                        }
                        Err(e) => {
                            if let Ok(mut g) = tui_state.lock() {
                                g.push_error(format!("[backtrack] {e}"));
                            }
                        }
                    }
                    continue;
                }
                TuiCmd::QueueSteering(line) => {
                    queued_steering.push_back(line);
                    if let Ok(mut g) = tui_state.lock() {
                        g.queued_steering = queued_steering.len();
                        g.queued_followup = queued_followup.len();
                        g.queue_preview_items = queued_followup
                            .iter()
                            .chain(queued_steering.iter())
                            .take(6)
                            .map(|s| truncate_chars(s, 80))
                            .collect();
                    }
                    continue;
                }
                TuiCmd::QueueFollowUp(line) => {
                    queued_followup.push_back(line);
                    if let Ok(mut g) = tui_state.lock() {
                        g.queued_steering = queued_steering.len();
                        g.queued_followup = queued_followup.len();
                        g.queue_preview_items = queued_followup
                            .iter()
                            .chain(queued_steering.iter())
                            .take(6)
                            .map(|s| truncate_chars(s, 80))
                            .collect();
                    }
                    continue;
                }
                TuiCmd::Exit => {
                    if let Ok(mut g) = tui_state.lock() {
                        g.should_exit = true;
                    }
                    break;
                }
                TuiCmd::CycleAgent => {
                    let next = self.agent_profile.next();
                    self.agent_profile = next;
                    self.current_agent_label = format!("@{}", next.label());
                    if next == AgentProfile::Plan {
                        self.runtime.set_permission_mode(PermissionMode::Plan);
                    } else {
                        self.runtime.set_permission_mode(PermissionMode::Default);
                    }
                    if let Ok(mut g) = tui_state.lock() {
                        g.set_agent_profile(&self.current_agent_label);
                        g.set_permission_mode(&format!("{:?}", self.runtime.permission_mode()));
                    }
                }
                TuiCmd::CancelTurn => {
                    self.runtime.request_cancel();
                }
                TuiCmd::OpenBranchPicker => {
                    let workspace = self.runtime.workspace_root();
                    let branches = git_list_branches(workspace);
                    let current = git_current_branch(workspace).unwrap_or_default();
                    if let Ok(mut g) = tui_state.lock() {
                        g.open_branch_picker(branches, &current);
                        g.set_current_branch(&current);
                    }
                }
                TuiCmd::SwitchBranch(name) => {
                    let workspace = self.runtime.workspace_root();
                    if git_switch_branch(workspace, &name) {
                        if let Ok(mut g) = tui_state.lock() {
                            g.set_current_branch(&name);
                            g.blocks.push(DisplayBlock::System(format!(
                                "Switched to branch: {}",
                                name
                            )));
                            g.touch_transcript();
                        }
                    } else if let Ok(mut g) = tui_state.lock() {
                        g.push_error(format!("Failed to switch to branch: {}", name));
                    }
                }
                TuiCmd::CreateBranch(name) => {
                    let workspace = self.runtime.workspace_root();
                    if git_create_branch(workspace, &name) {
                        if let Ok(mut g) = tui_state.lock() {
                            g.set_current_branch(&name);
                            g.blocks.push(DisplayBlock::System(format!(
                                "Created and switched to branch: {}",
                                name
                            )));
                            g.touch_transcript();
                        }
                    } else if let Ok(mut g) = tui_state.lock() {
                        g.push_error(format!("Failed to create branch: {}", name));
                    }
                }
                TuiCmd::ApplyDefaultProvider(p) => {
                    self.apply_provider_in_session(p, ReplOutput::Tui(&tui_state))
                        .await?;
                }
                TuiCmd::PromptApiKey(p, connect_after_save) => {
                    if let Ok(mut g) = tui_state.lock() {
                        g.open_api_key_modal(
                            p,
                            self.runtime.config().provider.api_key_present_for(p),
                            connect_after_save,
                        );
                    }
                }
                TuiCmd::ApplyModel(model_name) => {
                    let resolved = self.runtime.config().model.resolve_alias(&model_name);
                    let mut cfg = self.runtime.config().clone();
                    cfg.apply_model_override(&resolved);
                    cfg.model.track_recent_model(&resolved);
                    let workspace = self.runtime.workspace_root().to_path_buf();
                    match self.runtime.apply_dcode_ai_config(cfg) {
                        Ok(()) => {
                            let save_result = self
                                .runtime
                                .config()
                                .save_workspace_file(&workspace)
                                .or_else(|_| self.runtime.config().save_global());
                            if let Err(e) = save_result {
                                if let Ok(mut g) = tui_state.lock() {
                                    g.push_error(format!("[model] save failed: {e}"));
                                }
                            } else if let Ok(mut g) = tui_state.lock() {
                                g.model = self.runtime.model().to_string();
                                g.blocks.push(DisplayBlock::System(format!(
                                    "[model] switched to {} (saved)",
                                    self.runtime.model()
                                )));
                                g.touch_transcript();
                            }
                        }
                        Err(e) => {
                            if let Ok(mut g) = tui_state.lock() {
                                g.push_error(format!("[model] {e}"));
                            }
                        }
                    }
                }
                TuiCmd::ApplyModelProvider(p) => {
                    self.apply_provider_in_session(p, ReplOutput::Tui(&tui_state))
                        .await?;
                    let catalog = dcode_ai_runtime::model_limits_api::fetch_provider_model_ids(
                        self.runtime.config(),
                    )
                    .await;
                    let provider_models = catalog.unwrap_or_default();
                    let entries =
                        build_model_picker_entries(self.runtime.config(), &provider_models);
                    if let Ok(mut g) = tui_state.lock() {
                        // An unavailable live catalog is non-fatal in the picker.
                        g.open_model_picker(entries);
                    }
                }
                TuiCmd::ApplyPermission(idx) => {
                    let mode = permission_mode_from_index(idx);
                    self.runtime.set_permission_mode(mode);
                    if let Ok(mut g) = tui_state.lock() {
                        g.set_permission_mode(&format!("{mode:?}"));
                        g.blocks.push(DisplayBlock::System(format!(
                            "permission mode set to {mode:?}"
                        )));
                        g.touch_transcript();
                    }
                }
                TuiCmd::SwitchAgent(idx) => {
                    if let Some(&profile) = AgentProfile::ALL.get(idx) {
                        self.agent_profile = profile;
                        self.current_agent_label = format!("@{}", profile.label());
                        if profile == AgentProfile::Plan {
                            self.runtime.set_permission_mode(PermissionMode::Plan);
                        } else {
                            self.runtime.set_permission_mode(PermissionMode::Default);
                        }
                        if let Ok(mut g) = tui_state.lock() {
                            g.set_agent_profile(&self.current_agent_label);
                            g.set_permission_mode(&format!("{:?}", self.runtime.permission_mode()));
                            g.blocks.push(DisplayBlock::System(format!(
                                "switched to @{}",
                                profile.label()
                            )));
                            g.touch_transcript();
                        }
                    }
                }
                TuiCmd::OpenEditor => {
                    self.handle_command("/editor", ReplOutput::Tui(&tui_state))
                        .await?;
                }
                TuiCmd::NewSession => {
                    self.handle_command("/new", ReplOutput::Tui(&tui_state))
                        .await?;
                }
                TuiCmd::RunCompact => {
                    self.handle_command("/compact", ReplOutput::Tui(&tui_state))
                        .await?;
                }
                TuiCmd::OpenModelPicker => {
                    self.handle_command("/models", ReplOutput::Tui(&tui_state))
                        .await?;
                }
                TuiCmd::OpenStatus => {
                    self.handle_command("/status", ReplOutput::Tui(&tui_state))
                        .await?;
                }
                TuiCmd::OpenHelp => {
                    self.handle_command("/help", ReplOutput::Tui(&tui_state))
                        .await?;
                }
                TuiCmd::OpenAgentPicker => {
                    let current_idx = AgentProfile::ALL
                        .iter()
                        .position(|p| *p == self.agent_profile)
                        .unwrap_or(0);
                    if let Ok(mut g) = tui_state.lock() {
                        g.open_agent_picker(current_idx);
                    }
                }
                TuiCmd::OpenPermissionPicker => {
                    let current_idx = permission_mode_index(self.runtime.permission_mode());
                    if let Ok(mut g) = tui_state.lock() {
                        g.open_permission_picker(current_idx);
                    }
                }
                TuiCmd::OpenSessions => {
                    self.handle_command("/sessions", ReplOutput::Tui(&tui_state))
                        .await?;
                }
                TuiCmd::ResumeSession(session_id) => {
                    // Signal busy so the TUI status bar shows activity.
                    if let Ok(mut g) = tui_state.lock() {
                        g.set_busy(true);
                    }

                    let current = self.runtime.session_id().to_string();
                    if session_id == current {
                        if let Ok(mut g) = tui_state.lock() {
                            g.set_busy(false);
                            g.blocks
                                .push(DisplayBlock::System("Already on this session.".into()));
                        }
                    } else {
                        let safe_mode = self.safe_mode;
                        match self
                            .runtime
                            .resume_in_process(&session_id, safe_mode, true, None)
                            .await
                        {
                            Ok(()) => {
                                // Abort the old bridge — its channel is dead after runtime swap.
                                if let Ok(mut h) = bridge_handle.lock()
                                    && let Some(old) = h.take()
                                {
                                    old.abort();
                                }

                                let new_rx = self.runtime.take_event_rx();
                                let new_log = self.runtime.event_log_path();
                                let new_ipc = self.runtime.take_ipc_handle();
                                let new_approval = self.runtime.take_ipc_approval_pending();
                                let new_question = self.runtime.question_pending();

                                if let Some(rx) = new_rx {
                                    let new_bridge = spawn_tui_bridge(
                                        rx,
                                        new_log.clone(),
                                        new_ipc,
                                        new_approval,
                                        new_question,
                                        tui_state.clone(),
                                    );
                                    if let Ok(mut h) = bridge_handle.lock() {
                                        *h = Some(new_bridge);
                                    }
                                }

                                // Reset the TUI transcript and replay the resumed session.
                                if let Ok(mut g) = tui_state.lock() {
                                    g.blocks.clear();
                                    g.flushed_block_count = 0;
                                    g.request_clear = true; // purge native scrollback
                                    g.streaming_assistant = None;
                                    g.streaming_thinking = None;
                                    g.session_id = self.runtime.session_id().to_string();
                                    g.model = self.runtime.model().to_string();
                                    g.touch_transcript();
                                }
                                replay_event_log_into_state(&new_log, &tui_state).await;
                                if let Ok(mut g) = tui_state.lock() {
                                    let restored = g.blocks.len();
                                    g.push_block(DisplayBlock::System(format!(
                                        "Resumed session {session_id} — {restored} messages restored"
                                    )));
                                    g.transcript_follow_tail = true;
                                    g.touch_transcript();
                                    g.set_busy(false);
                                }
                            }
                            Err(e) => {
                                if let Ok(mut g) = tui_state.lock() {
                                    g.set_busy(false);
                                    g.push_error(format!("Failed to resume session: {e}"));
                                }
                            }
                        }
                    }
                }
                TuiCmd::CycleModel(forward) => {
                    let recent = &self.runtime.config().model.recent_models;
                    if recent.len() >= 2 {
                        let current = self.runtime.model().to_string();
                        let pos = recent.iter().position(|m| m == &current).unwrap_or(0);
                        let next_pos = if forward {
                            (pos + 1) % recent.len()
                        } else {
                            pos.checked_sub(1).unwrap_or(recent.len() - 1)
                        };
                        let next_model = recent[next_pos].clone();
                        let mut cfg = self.runtime.config().clone();
                        cfg.apply_model_override(&next_model);
                        if let Ok(()) = self.runtime.apply_dcode_ai_config(cfg) {
                            let _ = self
                                .runtime
                                .config()
                                .save_workspace_file(self.runtime.workspace_root());
                            if let Ok(mut g) = tui_state.lock() {
                                g.model = self.runtime.model().to_string();
                                g.blocks.push(DisplayBlock::System(format!(
                                    "[F2] switched to {}",
                                    self.runtime.model()
                                )));
                                g.touch_transcript();
                            }
                        }
                    } else if let Ok(mut g) = tui_state.lock() {
                        g.blocks.push(DisplayBlock::System(
                            "[F2] no recent models to cycle (need 2+ in model.recent_models)"
                                .into(),
                        ));
                        g.touch_transcript();
                    }
                }
                TuiCmd::ValidateApiKey(provider, api_key) => {
                    // Set validating state
                    if let Ok(mut g) = tui_state.lock() {
                        g.validation_status =
                            Some(crate::tui::state::OnboardingValidation::Validating);
                    }
                    // Look up base_url from config
                    let base_url = self
                        .runtime
                        .config()
                        .provider
                        .base_url_for(provider)
                        .to_string();
                    // Run async validation
                    let result = dcode_ai_core::provider::validate::validate_api_key(
                        provider, &api_key, &base_url,
                    )
                    .await;
                    if let Ok(mut g) = tui_state.lock() {
                        match &result {
                            dcode_ai_core::provider::validate::ValidationResult::Valid => {
                                // Save key and complete onboarding
                                g.validation_status =
                                    Some(crate::tui::state::OnboardingValidation::Valid);
                                g.close_api_key_modal();
                                g.close_connect_modal();
                                g.onboarding_mode = false;
                            }
                            dcode_ai_core::provider::validate::ValidationResult::InvalidKey(
                                msg,
                            ) => {
                                g.validation_status = Some(
                                    crate::tui::state::OnboardingValidation::Failed(msg.clone()),
                                );
                            }
                            dcode_ai_core::provider::validate::ValidationResult::NetworkError(
                                msg,
                            ) => {
                                g.validation_status = Some(
                                    crate::tui::state::OnboardingValidation::Failed(msg.clone()),
                                );
                            }
                        }
                    }
                    // If validation succeeded, save key + complete onboarding
                    if matches!(
                        result,
                        dcode_ai_core::provider::validate::ValidationResult::Valid
                    ) {
                        // Store the secret in the credentials file, then
                        // switch provider (resolution reads the store).
                        let mut cfg = self.runtime.config().clone();
                        let env_name = cfg.provider.api_key_env_for(provider).to_string();
                        if let Err(e) = dcode_ai_common::credentials::set(&env_name, &api_key) {
                            tracing::warn!("onboarding: credential store failed: {e}");
                            cfg.set_provider_api_key(provider, &api_key);
                        }
                        cfg.set_default_provider(provider);
                        if let Err(e) = self.runtime.apply_dcode_ai_config(cfg) {
                            tracing::warn!("onboarding: provider apply failed: {e}");
                            if let Ok(mut g) = tui_state.lock() {
                                g.validation_status =
                                    Some(crate::tui::state::OnboardingValidation::Failed(format!(
                                        "Failed to apply provider: {e}"
                                    )));
                                g.onboarding_mode = true;
                            }
                            continue;
                        }
                        // Sync TUI model display
                        if let Ok(mut g) = tui_state.lock() {
                            g.model = self.runtime.model().to_string();
                        }
                        // Persist onboarding flag to global config only (not workspace)
                        let mut cfg = self.runtime.config().clone();
                        cfg.ui.onboarding_completed = true;
                        if let Err(e) = cfg.save_global() {
                            tracing::warn!("onboarding: global config save failed: {e}");
                        }
                        let _ = self.runtime.apply_dcode_ai_config(cfg);
                    }
                }
                TuiCmd::CompleteAnthropicOAuth {
                    code_verifier,
                    authorization_code,
                } => {
                    if let Ok(mut g) = tui_state.lock() {
                        g.blocks.push(DisplayBlock::System(
                            "[login] completing anthropic OAuth exchange...".into(),
                        ));
                        g.touch_transcript();
                    }
                    match crate::oauth_login::finish_anthropic_login(
                        &code_verifier,
                        &authorization_code,
                    )
                    .await
                    {
                        Ok(()) => {
                            self.apply_provider_after_oauth_login(
                                OAuthProvider::Anthropic,
                                &ReplOutput::Tui(&tui_state),
                            )
                            .await?;
                        }
                        Err(e) => {
                            if let Ok(mut g) = tui_state.lock() {
                                g.push_error(format!("[login] {e}"));
                            }
                        }
                    }
                }
                TuiCmd::ApplyTheme(name) => {
                    use crate::tui::theme;
                    let applied = theme::set_by_name(Some(&name));
                    self.runtime.config_mut().ui.theme = Some(applied.name.to_string());
                    if let Err(e) = self
                        .runtime
                        .config()
                        .save_workspace_file(self.runtime.workspace_root())
                    {
                        tracing::warn!("theme persist failed: {e}");
                    }
                    if let Ok(mut g) = tui_state.lock() {
                        g.blocks.push(DisplayBlock::System(format!(
                            "theme set to {}",
                            applied.name
                        )));
                        g.touch_transcript();
                    }
                }
                TuiCmd::SwitchProject(idx) => {
                    // Signal busy so the TUI status bar shows activity.
                    if let Ok(mut g) = tui_state.lock() {
                        g.set_busy(true);
                    }

                    // Read the target, mark it active, then drop the lock before awaiting.
                    let target = if let Ok(mut g) = tui_state.lock() {
                        if idx < g.connected_projects.len() {
                            let name = g.connected_projects[idx].name.clone();
                            let path = g.connected_projects[idx].path.clone();
                            g.switch_project(idx);
                            Some((name, path))
                        } else {
                            None
                        }
                    } else {
                        None
                    };

                    if let Some((proj_name, proj_path)) = target {
                        let current_ws = self.runtime.workspace_root().to_path_buf();
                        if proj_path == current_ws {
                            if let Ok(mut g) = tui_state.lock() {
                                g.push_block(DisplayBlock::System(
                                    "Already on this project.".into(),
                                ));
                                g.touch_transcript();
                            }
                        } else {
                            let safe_mode = self.safe_mode;
                            match self
                                .runtime
                                .reroot_in_process(&proj_path, safe_mode, true, None)
                                .await
                            {
                                Ok(()) => {
                                    let _ = std::env::set_current_dir(&proj_path);
                                    // Old bridge channel is dead after the swap.
                                    if let Ok(mut h) = bridge_handle.lock()
                                        && let Some(old) = h.take()
                                    {
                                        old.abort();
                                    }
                                    let new_rx = self.runtime.take_event_rx();
                                    let new_log = self.runtime.event_log_path();
                                    let new_ipc = self.runtime.take_ipc_handle();
                                    let new_approval = self.runtime.take_ipc_approval_pending();
                                    let new_question = self.runtime.question_pending();
                                    if let Some(rx) = new_rx {
                                        let nb = spawn_tui_bridge(
                                            rx,
                                            new_log,
                                            new_ipc,
                                            new_approval,
                                            new_question,
                                            tui_state.clone(),
                                        );
                                        if let Ok(mut h) = bridge_handle.lock() {
                                            *h = Some(nb);
                                        }
                                    }
                                    let branch = git_current_branch(&proj_path).unwrap_or_default();
                                    if let Ok(mut g) = tui_state.lock() {
                                        g.blocks.clear();
                                        g.flushed_block_count = 0;
                                        g.request_clear = true;
                                        g.streaming_assistant = None;
                                        g.streaming_thinking = None;
                                        g.session_id = self.runtime.session_id().to_string();
                                        g.model = self.runtime.model().to_string();
                                        g.workspace_root = proj_path.clone();
                                        g.workspace_display = proj_path.display().to_string();
                                        g.set_current_branch(&branch);
                                        g.push_block(DisplayBlock::System(format!(
                                            "Switched to project: {proj_name}  ({})",
                                            proj_path.display()
                                        )));
                                        g.transcript_follow_tail = true;
                                        g.touch_transcript();
                                        g.set_busy(false);
                                    }
                                }
                                Err(e) => {
                                    if let Ok(mut g) = tui_state.lock() {
                                        g.set_busy(false);
                                        g.push_error(format!("Failed to switch project: {e}"));
                                    }
                                }
                            }
                        }
                    } else {
                        if let Ok(mut g) = tui_state.lock() {
                            g.set_busy(false);
                        }
                    }
                }
                TuiCmd::AddProject(path) => {
                    if let Ok(canonical) = dcode_ai_common::config::canonicalize_simplified(&path) {
                        let name = canonical
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("project")
                            .to_string();
                        if let Ok(mut g) = tui_state.lock() {
                            g.add_project(name.clone(), canonical);
                            g.push_block(DisplayBlock::System(format!("Added project: {name}")));
                            g.touch_transcript();
                        }
                    }
                }
                TuiCmd::OpenProjectPicker => {
                    if let Ok(mut g) = tui_state.lock() {
                        g.open_project_picker();
                    }
                }
                TuiCmd::CompleteOnboarding => {
                    let mut cfg = self.runtime.config().clone();
                    cfg.ui.onboarding_completed = true;
                    if let Err(e) = cfg.save_global() {
                        tracing::warn!("onboarding flag save failed: {e}");
                    }
                    if let Err(e) = self.runtime.apply_dcode_ai_config(cfg) {
                        tracing::warn!("onboarding config apply failed: {e}");
                    }
                }
                TuiCmd::QuestionAnswer(selection) => {
                    let qid = if let Ok(g) = tui_state.lock() {
                        g.active_question.as_ref().map(|q| q.question_id.clone())
                    } else {
                        None
                    };
                    if let Some(qid) = qid {
                        if qid == STARTUP_APPROVE_ALL_QUESTION_ID {
                            let approved = match selection {
                                QuestionSelection::Suggested => true,
                                QuestionSelection::Option { option_id } => {
                                    option_id == "approve_all"
                                }
                                QuestionSelection::Custom { .. } => false,
                            };
                            if approved {
                                self.runtime.add_session_allow_pattern("*".to_string());
                            }
                            if let Ok(mut g) = tui_state.lock() {
                                g.active_question = None;
                                g.close_question_modal();
                                if approved {
                                    if !g.quiet_startup {
                                        g.blocks.push(DisplayBlock::System(
                                            "[permissions] startup approval granted for this session."
                                                .into(),
                                        ));
                                    }
                                } else {
                                    g.blocks.push(DisplayBlock::System(
                                        "[permissions] startup approval declined; using default approval flow."
                                            .into(),
                                    ));
                                }
                                g.touch_transcript();
                            }
                        } else if !self.runtime.submit_question_answer(&qid, selection)
                            && let Ok(mut g) = tui_state.lock()
                        {
                            g.push_error(
                                "failed to submit answer (expired or already answered)".into(),
                            );
                        }
                    }
                }
                TuiCmd::Submit(line) => {
                    let line = line.trim().to_string();
                    let api_key_modal_state = tui_state.lock().ok().and_then(|g| {
                        g.api_key_modal_open.then_some((
                            g.api_key_target_provider,
                            g.api_key_input.clone(),
                            g.api_key_connect_after_save,
                        ))
                    });
                    if let Some((Some(p), key_input, connect_after_save)) = api_key_modal_state {
                        let typed = if line.starts_with('/') {
                            ""
                        } else {
                            key_input.trim()
                        };
                        let had_existing = self.runtime.config().provider.api_key_present_for(p);
                        if line.starts_with('/') {
                            if let Ok(mut g) = tui_state.lock() {
                                g.close_api_key_modal();
                            }
                        } else if typed.is_empty() {
                            if had_existing {
                                if let Ok(mut g) = tui_state.lock() {
                                    g.close_api_key_modal();
                                    g.blocks.push(DisplayBlock::System(format!(
                                        "[apikey] keeping existing key for {}",
                                        p.display_name()
                                    )));
                                    g.touch_transcript();
                                }
                                if connect_after_save {
                                    self.apply_provider_in_session(p, ReplOutput::Tui(&tui_state))
                                        .await?;
                                }
                            } else if let Ok(mut g) = tui_state.lock() {
                                g.push_error(format!(
                                    "[apikey] paste a key for {} or Esc to cancel",
                                    p.display_name()
                                ));
                            }
                            continue;
                        } else {
                            self.save_provider_api_key(p, typed, ReplOutput::Tui(&tui_state))
                                .await?;
                            if let Ok(mut g) = tui_state.lock() {
                                g.close_api_key_modal();
                            }
                            if connect_after_save {
                                self.apply_provider_in_session(p, ReplOutput::Tui(&tui_state))
                                    .await?;
                            }
                            continue;
                        }
                    }
                    if line.is_empty() {
                        if let Ok(mut g) = tui_state.lock()
                            && g.pending_api_key_provider.take().is_some()
                        {
                            g.blocks.push(DisplayBlock::System(
                                "[apikey] entry cancelled (empty line)".into(),
                            ));
                            g.touch_transcript();
                        }
                        continue;
                    }
                    if let Some(p) = tui_state
                        .lock()
                        .ok()
                        .and_then(|g| g.pending_api_key_provider)
                    {
                        if !line.starts_with('/') {
                            let mut cfg = self.runtime.config().clone();
                            let env_name = cfg.provider.api_key_env_for(p).to_string();
                            if let Err(e) =
                                dcode_ai_common::credentials::set(&env_name, line.trim())
                            {
                                if let Ok(mut g) = tui_state.lock() {
                                    g.push_error(format!("[apikey] credential store failed: {e}"));
                                }
                                continue;
                            }
                            cfg.set_provider_api_key(p, "");
                            match self.runtime.apply_dcode_ai_config(cfg) {
                                Ok(()) => {
                                    if let Ok(mut g) = tui_state.lock() {
                                        g.pending_api_key_provider = None;
                                        g.blocks.push(DisplayBlock::System(format!(
                                            "[apikey] saved for {} (~/.dcode-ai/credentials.toml)",
                                            p.display_name()
                                        )));
                                        g.touch_transcript();
                                    }
                                }
                                Err(e) => {
                                    if let Ok(mut g) = tui_state.lock() {
                                        g.push_error(format!("[apikey] {e}"));
                                    }
                                }
                            }
                            continue;
                        }
                        if let Ok(mut g) = tui_state.lock() {
                            g.pending_api_key_provider = None;
                        }
                    }
                    if line.starts_with('!') {
                        let shell_cmd = line.trim_start_matches('!').trim();
                        // Intercept `!cd <path>` — cd is a shell built-in that
                        // can't work in a child process. Redirect to /cd.
                        if shell_cmd == "cd" || shell_cmd.starts_with("cd ") {
                            let path = shell_cmd.strip_prefix("cd").unwrap_or("").trim();
                            let cd_cmd = format!("/cd {path}");
                            if !self
                                .handle_command(&cd_cmd, ReplOutput::Tui(&tui_state))
                                .await?
                            {
                                if let Ok(mut g) = tui_state.lock() {
                                    g.should_exit = true;
                                }
                                break;
                            }
                            continue;
                        }
                        let output = self.run_bash_tui_capture(shell_cmd, &tui_state).await;
                        // Auto-send the shell output to the AI — but DON'T
                        // show the synthetic prompt in the transcript (it looks
                        // like the user typed it). Just show the AI's response.
                        if let Some(output) = output
                            && !output.trim().is_empty()
                        {
                            let prompt = format!(
                                "I ran `{shell_cmd}` and got this output. Analyze it briefly and suggest what to do next if the output indicates a problem:\n\n```\n{output}\n```"
                            );
                            if let Ok(mut g) = tui_state.lock() {
                                g.set_busy(true);
                                // Suppress the auto-generated user message in the
                                // transcript by setting a flag the event handler
                                // can check to skip the UserBlock push.
                                g.suppress_next_user_block = true;
                            }
                            if let Err(e) = self.runtime.run_turn(&prompt).await
                                && let Ok(mut g) = tui_state.lock()
                            {
                                g.push_error(e.to_string());
                            }
                            if let Ok(mut g) = tui_state.lock() {
                                g.set_busy(false);
                            }
                        }
                        continue;
                    }
                    if line.starts_with('/') {
                        if !self
                            .handle_command(&line, ReplOutput::Tui(&tui_state))
                            .await?
                        {
                            if let Ok(mut g) = tui_state.lock() {
                                g.should_exit = true;
                            }
                            break;
                        }
                        continue;
                    }
                    let expanded =
                        match expand_at_file_mentions_default(&line, self.runtime.workspace_root())
                        {
                            Ok(s) => s,
                            Err(e) => {
                                if let Ok(mut g) = tui_state.lock() {
                                    g.push_error(format!("file mentions: {e}"));
                                }
                                continue;
                            }
                        };
                    // Prepend any pending context blocks (from /web or /run).
                    let expanded = if let Ok(mut g) = tui_state.lock()
                        && !g.pending_context.is_empty()
                    {
                        let ctx = g
                            .pending_context
                            .drain(..)
                            .collect::<Vec<_>>()
                            .join("\n\n---\n\n");
                        format!("{ctx}\n\n---\n\n{expanded}")
                    } else {
                        expanded
                    };
                    if let Ok(mut g) = tui_state.lock() {
                        g.set_busy(true);
                    }
                    let attachments = if let Ok(mut g) = tui_state.lock() {
                        std::mem::take(&mut g.staged_image_attachments)
                    } else {
                        Vec::new()
                    };
                    let turn = if attachments.is_empty() {
                        self.runtime.run_turn(&expanded).await
                    } else {
                        self.runtime
                            .run_turn_with_images(&expanded, attachments)
                            .await
                    };
                    if let Err(e) = turn
                        && let Ok(mut g) = tui_state.lock()
                    {
                        g.push_error(e.to_string());
                    }
                    if let Ok(mut g) = tui_state.lock() {
                        g.set_busy(false);
                    }
                }
            }
        }

        let _ = ui.await;
        self.runtime.finish(EndReason::UserExit).await;
        Ok(())
    }

    /// Run a shell command from `!` prefix, display output, and return captured text.
    async fn run_bash_tui_capture(
        &self,
        cmd: &str,
        st: &Arc<Mutex<TuiSessionState>>,
    ) -> Option<String> {
        fn log(st: &Arc<Mutex<TuiSessionState>>, s: &str) {
            if let Ok(mut g) = st.lock() {
                g.blocks.push(DisplayBlock::System(s.to_string()));
                g.touch_transcript();
            }
        }
        if cmd.is_empty() {
            log(st, "! usage: !<command>");
            return None;
        }
        log(st, &format!("[bash] $ {cmd}"));
        let shell = if cfg!(windows) { "cmd" } else { "sh" };
        let flag = if cfg!(windows) { "/C" } else { "-c" };
        let output = Command::new(shell)
            .arg(flag)
            .arg(cmd)
            .current_dir(self.runtime.workspace_root())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;
        match output {
            Ok(out) => {
                let stdout = String::from_utf8_lossy(&out.stdout).to_string();
                let stderr = String::from_utf8_lossy(&out.stderr).to_string();
                if !stdout.is_empty()
                    && let Ok(mut g) = st.lock()
                {
                    for line in stdout.lines().take(50) {
                        g.blocks.push(DisplayBlock::System(line.to_string()));
                    }
                    if stdout.lines().count() > 50 {
                        g.blocks.push(DisplayBlock::System(format!(
                            "… {} more lines",
                            stdout.lines().count() - 50
                        )));
                    }
                    g.touch_transcript();
                }
                if !stderr.is_empty() {
                    log(st, &format!("[stderr] {}", stderr.trim()));
                }
                if out.status.success() {
                    log(st, "[bash] ✓ exit 0");
                } else if let Ok(mut g) = st.lock() {
                    g.push_error(format!("[bash] ✗ exit {}", out.status.code().unwrap_or(-1)));
                }
                let combined = if stderr.is_empty() {
                    stdout
                } else {
                    format!("{stdout}\n{stderr}")
                };
                Some(combined)
            }
            Err(e) => {
                log(st, &format!("[bash] {e}"));
                None
            }
        }
    }
}

/// Tab completion for REPL commands and skills
impl Completer for Repl {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let mut suggestions = Vec::new();

        if let Some((at_byte, prefix)) = at_token_before_cursor(line, pos) {
            let files = discover_workspace_files(self.runtime.workspace_root());
            for path in filter_paths_prefix(&files, &prefix) {
                suggestions.push(Suggestion {
                    value: format!("@{path}"),
                    description: Some("workspace file".to_string()),
                    extra: None,
                    span: reedline::Span {
                        start: at_byte,
                        end: pos,
                    },
                    append_whitespace: false,
                    style: None,
                });
            }
            if !suggestions.is_empty() {
                return suggestions;
            }
        }

        // Complete REPL commands starting with /
        if line.starts_with('/') {
            for cmd in SLASH_COMMANDS {
                if cmd.starts_with(line) {
                    suggestions.push(Suggestion {
                        value: cmd.to_string(),
                        description: Some("REPL command".to_string()),
                        extra: None,
                        span: reedline::Span { start: 0, end: 0 },
                        append_whitespace: true,
                        style: None,
                    });
                }
            }
        }

        // Complete bash mode commands (starting with !)
        if line.starts_with('!') {
            // Common shell commands
            let bash_commands = [
                "git", "ls", "cat", "find", "grep", "npm", "cargo", "make", "docker", "curl",
            ];
            let _prefix = line.trim_start_matches('!');
            for cmd in bash_commands {
                let full = format!("!{}", cmd);
                if full.starts_with(line) {
                    suggestions.push(Suggestion {
                        value: full,
                        description: Some("Shell command".to_string()),
                        extra: None,
                        span: reedline::Span { start: 0, end: 0 },
                        append_whitespace: true,
                        style: None,
                    });
                }
            }
        }

        // Load skills for completion
        if let Ok(skills) = SkillCatalog::discover(
            self.runtime.workspace_root(),
            &self.runtime.config().harness.skill_directories,
        ) {
            for skill in skills {
                let skill_cmd = format!("/{}", skill.command);
                if skill_cmd.starts_with(line) {
                    suggestions.push(Suggestion {
                        value: skill_cmd,
                        description: skill.description,
                        extra: None,
                        span: reedline::Span { start: 0, end: 0 },
                        append_whitespace: true,
                        style: None,
                    });
                }
            }
        }

        suggestions
    }
}

use crate::repl_helpers::*;

#[cfg(test)]
mod tests {
    use super::*;
    use dcode_ai_common::message::Message;

    #[test]
    fn parses_permission_aliases() {
        assert_eq!(
            parse_permission_mode("accept-edits"),
            Some(PermissionMode::AcceptEdits)
        );
        assert_eq!(
            parse_permission_mode("dontask"),
            Some(PermissionMode::DontAsk)
        );
        assert_eq!(
            parse_permission_mode("bypass_permissions"),
            Some(PermissionMode::BypassPermissions)
        );
        assert_eq!(parse_permission_mode("invalid"), None);
    }

    #[test]
    fn parses_logout_aliases() {
        assert!(matches!(
            parse_logout_target("openai"),
            Some(LogoutTarget::Openai)
        ));
        assert!(matches!(
            parse_logout_target("codex"),
            Some(LogoutTarget::Openai)
        ));
        assert!(matches!(
            parse_logout_target("claude"),
            Some(LogoutTarget::Anthropic)
        ));
        assert!(matches!(
            parse_logout_target("opencode"),
            Some(LogoutTarget::Opencodezen)
        ));
        assert!(matches!(
            parse_logout_target("all"),
            Some(LogoutTarget::All)
        ));
        assert!(parse_logout_target("nope").is_none());
    }

    #[test]
    fn recent_context_lines_include_roles_and_truncation() {
        let messages = vec![
            Message::system("setup"),
            Message::user("hello world"),
            Message::assistant("response body"),
        ];
        let lines = recent_context_lines(&messages, 5);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("Recent turns: 3 message(s)"))
        );
        assert!(lines.iter().any(|l| l.contains("user")));
        assert!(lines.iter().any(|l| l.contains("assistant")));
    }
}
