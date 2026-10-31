use crate::file_mentions::{
    at_token_before_cursor, discover_workspace_files, expand_at_file_mentions_default,
    filter_paths_prefix,
};
use crate::oauth_login::{LogoutTarget, OAuthProvider};
use crate::prompt::DcodeAiPrompt;
use crate::runner::{SessionRuntime, dispatch_question_answer, dispatch_tool_approval};
use crate::slash_commands::SLASH_COMMANDS;
use crate::tui::app::ApprovalAnswer;
use crate::tui::state::RewindScope;
use crate::tui::{
    DisplayBlock, SessionPickerEntry, TuiCmd, TuiSessionState, git_create_branch,
    git_current_branch, git_list_branches, git_switch_branch, run_blocking, spawn_tui_bridge,
};
use dcode_ai_common::config::{PermissionMode, ProviderKind};
use dcode_ai_common::event::{
    EndReason, InteractiveQuestionPayload, QuestionOption, QuestionSelection,
};
use dcode_ai_core::skills::SkillCatalog;
use dcode_ai_runtime::memory_store::MemoryStore;
use reedline::{Completer, Emacs, FileBackedHistory, Reedline, Signal, Suggestion, Vi};
use std::collections::VecDeque;
use std::io::Write;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use tokio::process::Command;

const STARTUP_APPROVE_ALL_QUESTION_ID: &str = "startup-approve-all";

// Slash commands, by area (each a method on `Repl`).
mod cmd_choice;
mod cmd_doctor;
mod cmd_ext;
mod cmd_info;
mod cmd_mcp;
mod cmd_session;
mod cmd_settings;
mod cmd_team;
mod cmd_work;

/// A downloaded, verified update that needs sudo to install.
pub(crate) struct PendingSudo {
    pub tag: String,
    pub staged: std::path::PathBuf,
    pub target: std::path::PathBuf,
    pub work: std::path::PathBuf,
    pub tries: u32,
}

/// What the sudo popup says it is for.
pub(crate) fn sudo_message(p: &PendingSudo) -> String {
    format!(
        "Install dcode-ai {} to {}. That folder needs administrator rights, so sudo asks for your password.",
        p.tag,
        p.target.display()
    )
}

/// Footer text while a goal is active.
fn goal_footer_label(cell: &dcode_ai_runtime::goal::GoalCell) -> Option<String> {
    let guard = cell.lock().ok()?;
    let goal = guard.as_ref()?;
    match goal.status {
        dcode_ai_runtime::goal::GoalStatus::Active => Some(format!("◎ goal · turn {}", goal.turns)),
        dcode_ai_runtime::goal::GoalStatus::Paused => Some("◎ goal paused".into()),
        _ => None,
    }
}

impl Repl {
    /// Ask a cheap model for a session title in the background; the answer
    /// comes back as `TuiCmd::SessionTitle`. `replace` renames a named one.
    fn spawn_session_title(&self, st: &Arc<Mutex<TuiSessionState>>, replace: bool) {
        let ctx = st
            .lock()
            .ok()
            .and_then(|g| g.btw_ctx.lock().ok().and_then(|c| c.clone()));
        let (Some(ctx), Some(tx)) = (ctx, self.self_cmd_tx.clone()) else {
            return;
        };
        let session_id = self.runtime.session_id().to_string();
        tokio::spawn(async move {
            match crate::tui::btw::suggest_title(ctx).await {
                Ok(title) => {
                    let _ = tx.send(TuiCmd::SessionTitle {
                        session_id,
                        title,
                        replace,
                    });
                }
                Err(e) => tracing::info!("session title: {e}"),
            }
        });
    }
}

/// After a resume: say that the session's goal is back, paused.
fn show_restored_goal(cell: &dcode_ai_runtime::goal::GoalCell, st: &Arc<Mutex<TuiSessionState>>) {
    let goal = cell.lock().ok().and_then(|g| g.clone());
    let Some(goal) = goal.filter(|g| g.status == dcode_ai_runtime::goal::GoalStatus::Paused) else {
        return;
    };
    if let Ok(mut g) = st.lock() {
        g.push_block(DisplayBlock::System(format!(
            "◎ Goal paused: {} · /goal resume continues it, /goal clear drops it",
            goal.objective
        )));
        g.goal_label = goal_footer_label(cell);
        g.touch_transcript();
    }
}

/// Which reviewer `run_review` sends over the change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReviewKind {
    Verify,
    SecondOpinion,
    Security,
}

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

pub(crate) fn parse_oauth_provider(value: &str) -> Option<OAuthProvider> {
    match value.trim().to_ascii_lowercase().as_str() {
        "anthropic" | "claude" => Some(OAuthProvider::Anthropic),
        "copilot" | "github" => Some(OAuthProvider::Copilot),
        _ => None,
    }
}

fn parse_logout_target(value: &str) -> Option<LogoutTarget> {
    match value.trim().to_ascii_lowercase().as_str() {
        "anthropic" | "claude" => Some(LogoutTarget::Anthropic),
        "copilot" | "github" => Some(LogoutTarget::Copilot),
        "all" | "*" => Some(LogoutTarget::All),
        _ => None,
    }
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
        "  Ctrl+V       Paste image (Alt+V on Windows/WSL)".into(),
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
    /// Session whose title was already asked for (once per session).
    titled_session: Option<String>,
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
    /// Sender into the TUI command loop (background tasks queue follow-ups).
    self_cmd_tx: Option<tokio::sync::mpsc::UnboundedSender<TuiCmd>>,
    /// A verified update waiting for a sudo password.
    pending_sudo: Option<PendingSudo>,
    /// `/watch`: the live read-only page for this session.
    watch: Option<crate::watch::WatchHandle>,
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
            self_cmd_tx: None,
            titled_session: None,
            pending_sudo: None,
            watch: None,
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
            let mut command = dcode_ai_common::provider_runtime::system_shell_command(&cmd_str);
            dcode_ai_common::shell::prepare_agent_command(&mut command);
            let spawned = command
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

    /// Apply a startup permission choice (`manual`, `accept_edits`, `auto`);
    /// returns the line describing it.
    fn apply_startup_permission(&mut self, choice: &str) -> &'static str {
        match choice {
            "auto" => {
                self.set_mode_and_profile(PermissionMode::Auto);
                "Auto — runs without asking; risky actions (delete, push, publish, sudo) still ask"
            }
            "accept_edits" => {
                self.set_mode_and_profile(PermissionMode::AcceptEdits);
                "Accept edits — file edits run without asking; commands still ask"
            }
            _ => "Manual — dcode-ai asks before edits and commands",
        }
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
                // Host commands and external breaks are not used here.
                Ok(_) => {}
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

    /// Set the permission mode, keeping the agent profile in step: plan
    /// mode uses the read-only @plan profile, leaving it returns to @build.
    fn set_mode_and_profile(&mut self, mode: PermissionMode) {
        self.runtime.set_permission_mode(mode);
        let profile = if mode == PermissionMode::Plan {
            AgentProfile::Plan
        } else if self.agent_profile == AgentProfile::Plan {
            AgentProfile::Build
        } else {
            self.agent_profile
        };
        if profile != self.agent_profile {
            self.agent_profile = profile;
            self.current_agent_label = format!("@{}", profile.label());
            self.prompt.set_agent(&self.current_agent_label);
        }
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
        match self.runtime.apply_dcode_ai_config(cfg) {
            Ok(()) => {
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.model = self.runtime.model().to_string();
                }
                match persist_model_choice(
                    self.runtime.workspace_root(),
                    self.runtime.config().provider.default,
                    self.runtime.model(),
                ) {
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
    async fn apply_provider_after_oauth_login(
        &mut self,
        provider: OAuthProvider,
        out: &ReplOutput<'_>,
    ) -> anyhow::Result<()> {
        let mut cfg = self.runtime.config().clone();
        let target = match provider {
            OAuthProvider::Copilot => ProviderKind::Copilot,
            OAuthProvider::Anthropic => ProviderKind::Anthropic,
        };
        cfg.set_default_provider(target);
        match self.runtime.apply_dcode_ai_config(cfg) {
            Ok(()) => {
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.model = self.runtime.model().to_string();
                }
                if let Err(e) = persist_model_choice(
                    self.runtime.workspace_root(),
                    self.runtime.config().provider.default,
                    self.runtime.model(),
                ) {
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
        cfg.set_default_provider(ProviderKind::Copilot);
        match self.runtime.apply_dcode_ai_config(cfg) {
            Ok(()) => {
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.model = self.runtime.model().to_string();
                }
                match persist_model_choice(
                    self.runtime.workspace_root(),
                    self.runtime.config().provider.default,
                    self.runtime.model(),
                ) {
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

        // Tab opens a completion menu (slash commands, skills, `@` files).
        builder = builder
            .with_completer(Box::new(ReplCompleter {
                workspace_root: self.runtime.workspace_root().to_path_buf(),
                skill_directories: self.runtime.config().harness.skill_directories.clone(),
            }))
            .with_menu(reedline::ReedlineMenu::EngineCompleter(Box::new(
                <reedline::ColumnarMenu as reedline::MenuBuilder>::with_name(
                    reedline::ColumnarMenu::default(),
                    "completion_menu",
                ),
            )));
        let tab = reedline::ReedlineEvent::UntilFound(vec![
            reedline::ReedlineEvent::Menu("completion_menu".into()),
            reedline::ReedlineEvent::MenuNext,
        ]);

        // Support vim mode if enabled via env
        if std::env::var("DCODE_AI_EDITOR_MODE")
            .map(|v| v.eq_ignore_ascii_case("vi") || v.eq_ignore_ascii_case("vim"))
            .unwrap_or(false)
        {
            let mut insert = reedline::default_vi_insert_keybindings();
            insert.add_binding(reedline::KeyModifiers::NONE, reedline::KeyCode::Tab, tab);
            builder = builder.with_edit_mode(Box::new(Vi::new(
                insert,
                reedline::default_vi_normal_keybindings(),
                reedline::default_vi_visual_keybindings(),
            )));
        } else {
            let mut keys = reedline::default_emacs_keybindings();
            keys.add_binding(reedline::KeyModifiers::NONE, reedline::KeyCode::Tab, tab);
            builder = builder.with_edit_mode(Box::new(Emacs::new(keys)));
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
            "/stop" | "/run-bg" | "/bashes" | "/ps" | "/session-name" | "/rename" | "/rewind"
            | "/clear" | "/undo" | "/redo" | "/fork" | "/approve" | "/input" | "/side"
            | "/delete" | "/compact" | "/logs" | "/attach" | "/sessions" | "/sessions-clean"
            | "/new" | "/export" | "/share" | "/retry" | "/cd" | "/project" | "/import"
            | "/history" => {
                return self.cmd_session(input, command, rest, parts, out).await;
            }
            "/help" | "/keymaps" | "/status" | "/context" | "/diff" | "/copy" | "/dashboard"
            | "/dash" | "/transcript" | "/cost" | "/usage" | "/release-notes" | "/goals"
            | "/todos" | "/stats" | "/doctor" | "/map" | "/feedback" => {
                return self.cmd_info(input, command, rest, parts, out).await;
            }
            "/agent" | "/update" | "/vim" | "/permissions" | "/permission-bypass" | "/models"
            | "/config" | "/login" | "/connect" | "/logout" | "/auth" | "/sidebar" | "/quiet"
            | "/settings" | "/theme" | "/title" | "/raw" | "/mention" | "/output-style"
            | "/output-styles" | "/personality" | "/keymap" | "/statusline" | "/provider"
            | "/apikey" | "/editor" | "/set-editor" | "/auto-answer" | "/thinking" | "/effort"
            | "/terminal-setup" | "/project-config" => {
                return self.cmd_settings(input, command, rest, parts, out).await;
            }
            "/reload" | "/add-dir" | "/skills" | "/memory" | "/browser" | "/setup" | "/plugins"
            | "/plugin" | "/mcp" | "/agents" | "/hooks" | "/init" | "/ide" => {
                return self.cmd_ext(input, command, rest, parts, out).await;
            }
            "/timeline" | "/files" | "/sounds" | "/speak" | "/watch" | "/handoff" => {
                return self.cmd_team(input, command, rest, parts, out).await;
            }
            "/btw" => {
                if let ReplOutput::Tui(st) = &out {
                    if let Ok(mut g) = st.lock() {
                        g.ask_btw(rest);
                    }
                } else if rest.is_empty() {
                    out.eprintln("usage: /btw <question>");
                } else {
                    let prompt = crate::tui::btw::side_question_prompt(
                        self.runtime.workspace_root(),
                        self.runtime.messages(),
                        rest,
                    );
                    match self.runtime.complete_text(&prompt).await {
                        Ok(answer) => out.println(&format!("[btw] {answer}")),
                        Err(e) => out.eprintln(&format!("[btw] {e}")),
                    }
                }
            }
            "/verify" | "/second-opinion" | "/security-review" | "/pr-comments" | "/goal"
            | "/plan" | "/review" | "/fix" | "/test" | "/image" | "/run" | "/web" | "/commit" => {
                return self.cmd_work(input, command, rest, parts, out).await;
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

    /// `/verify`: a read-only verifier sub-agent reviews the uncommitted
    /// changes (and runs checks); real problems go back to the main agent.
    async fn run_verifier(&mut self, focus: &str, out: &ReplOutput<'_>) -> anyhow::Result<()> {
        self.run_review(focus, out, ReviewKind::Verify).await
    }

    /// `/verify` (same model) or `/second-opinion` (a model from a different
    /// family) over the uncommitted changes; real problems go back to the
    /// main agent.
    async fn run_review(
        &mut self,
        focus: &str,
        out: &ReplOutput<'_>,
        kind: ReviewKind,
    ) -> anyhow::Result<()> {
        let second_opinion = kind == ReviewKind::SecondOpinion;
        let ws = self.runtime.workspace_root().to_path_buf();
        let mut changed = git_changed_files(&ws);
        let mut base_note = String::new();
        if changed.is_empty()
            && kind == ReviewKind::Security
            && let Some((base, files)) = git_branch_changes(&ws)
        {
            base_note = format!(
                "\n\nThere are no uncommitted changes: review the branch's commits, \
                 `git diff {base}...HEAD` (run it with run_validation or read the files)."
            );
            changed = files;
        }
        if changed.is_empty() {
            out.println("Nothing to review — no uncommitted changes.");
            return Ok(());
        }
        let (child_config, reviewer_label) = if kind == ReviewKind::Security {
            (self.runtime.config().clone(), "Security review".to_string())
        } else if second_opinion {
            match second_opinion_config(self.runtime.config(), self.runtime.model()).await {
                Ok(v) => v,
                Err(e) => {
                    out.eprintln(&format!("[second-opinion] {e}"));
                    return Ok(());
                }
            }
        } else {
            (self.runtime.config().clone(), "Verifier".to_string())
        };
        out.println(&format!(
            "{reviewer_label} reviewing {} changed file(s)…",
            changed.len()
        ));
        let mut task = String::from(if kind == ReviewKind::Security {
            "Security-review the pending changes in this workspace."
        } else {
            "Verify the uncommitted changes in this workspace."
        });
        task.push_str(&base_note);
        if !focus.is_empty() {
            task.push_str(&format!("\n\nFocus especially on: {focus}"));
        }
        let cfg = dcode_ai_runtime::supervisor::ChildSessionConfig {
            parent_session_id: self.runtime.session_id().to_string(),
            task,
            agent: Some(match kind {
                ReviewKind::SecondOpinion => dcode_ai_core::agent_defs::second_opinion(),
                ReviewKind::Security => dcode_ai_core::agent_defs::security_reviewer(),
                ReviewKind::Verify => dcode_ai_core::agent_defs::verifier(),
            }),
            workspace_root: ws,
            config: child_config,
            parent_summary: dcode_ai_runtime::supervisor::build_parent_summary(
                self.runtime.messages(),
            ),
            use_worktree: false,
            explore: false,
            focus_files: changed,
            // The reviewer has no edit tools; auto mode lets it run checks
            // while risky commands are still refused.
            parent_mode: Some(PermissionMode::Auto),
            child_id: None,
            approval: None,
        };
        let result =
            dcode_ai_runtime::supervisor::spawn_child_session(cfg, self.runtime.event_sender())
                .await;
        // The reviewer shows in the agents rail while it works; mark it done.
        if let Ok(r) = &result
            && let Some(tx) = self.runtime.event_sender()
        {
            let _ = tx
                .send(dcode_ai_common::event::AgentEvent::ChildSessionCompleted {
                    parent_session_id: self.runtime.session_id().to_string(),
                    child_session_id: r.child_session_id.clone(),
                    status: r.status.clone(),
                })
                .await;
        }
        let report = match result {
            Ok(r) if r.status == "completed" => r.output,
            Ok(r) => {
                out.eprintln(&format!("[verify] verifier {}: {}", r.status, r.output));
                return Ok(());
            }
            Err(e) => {
                out.eprintln(&format!("[verify] {e}"));
                return Ok(());
            }
        };
        if let ReplOutput::Tui(st) = out
            && let Ok(mut g) = st.lock()
        {
            g.push_block(DisplayBlock::Assistant(format!(
                "**{reviewer_label}**\n\n{}",
                report.trim()
            )));
            g.touch_transcript();
        } else {
            out.println(&format!("{reviewer_label}:\n{}", report.trim()));
        }
        // Like Claude Code, a security review reports; fixing is the user's call.
        if kind != ReviewKind::Security && verifier_found_problems(&report) {
            let prompt = if second_opinion {
                format!(
                    "A reviewer using a different model ({reviewer_label}) checked your uncommitted \
                     changes and reported:\n\n{}\n\nFor each point, either fix it or say in one line \
                     why it is not a problem. Re-run the relevant checks after fixing.",
                    report.trim()
                )
            } else {
                format!(
                    "A read-only verifier reviewed your uncommitted changes and reported:\n\n{}\n\n\
                     Fix the real problems (ignore anything that is not actually wrong) and re-run the relevant checks.",
                    report.trim()
                )
            };
            if let ReplOutput::Tui(st) = out
                && let Ok(mut g) = st.lock()
            {
                g.suppress_next_user_block = true;
            }
            if let Err(e) = self.runtime.run_turn(&prompt).await {
                out.eprintln(&format!("error: {e}"));
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

        // A command's `model:` applies to its own run only, and only on the
        // Claude provider (its ids/aliases mean nothing to Copilot).
        let previous_model = self.runtime.model().to_string();
        let switched_model = match &skill.model {
            Some(model)
                if self.runtime.config().provider.default
                    == dcode_ai_common::config::ProviderKind::Anthropic =>
            {
                self.runtime
                    .set_model(self.runtime.config().model.resolve_alias(model));
                true
            }
            _ => false,
        };
        if let Some(mode) = skill.permission_mode {
            self.runtime.set_permission_mode(mode);
        }

        let prompt = skill.prompt_for_task(task);
        // Claude Code command files: `!`cmd`` runs (when allowed-tools
        // permits it) and its output is inlined into the prompt.
        let prompt =
            expand_inline_shell(&prompt, &skill.allowed_tools, self.runtime.workspace_root()).await;
        let prompt = match expand_at_file_mentions_default(&prompt, self.runtime.workspace_root()) {
            Ok(s) => s,
            Err(e) => {
                out.eprintln(&format!("file mentions: {e}"));
                return Ok(true);
            }
        };
        // Show what was typed (`/review src/`), not the expanded template.
        if let ReplOutput::Tui(st) = out
            && let Ok(mut g) = st.lock()
        {
            let typed = format!("/{skill_name} {}", task.trim());
            g.push_block(DisplayBlock::User(typed.trim_end().to_string()));
            g.suppress_next_user_block = true;
            g.touch_transcript();
        }
        // `allowed-tools` pre-approves those tools for this command's turn only.
        let saved_allows = self.runtime.session_allows();
        for pattern in dcode_ai_core::skills::allowed_tool_patterns(&skill.allowed_tools) {
            self.runtime.add_session_allow_pattern(pattern);
        }
        let result = self.runtime.run_turn(&prompt).await;
        self.runtime.set_session_allows(saved_allows);
        if switched_model {
            self.runtime.set_model(previous_model);
        }
        match result {
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
        // The TUI delivers sub-agent completion notifications, so agents can
        // run in the background here.
        self.runtime.set_background_subagents(true);
        // Cached "newer version" for the welcome box, plus a background check
        // when the cache is stale (it never blocks startup).
        let _ = crate::update_check::init_and_pending_upgrade();
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
        // sudo in agent / `!` commands asks through the TUI's password popup.
        crate::askpass::start(tui_state.clone());
        // Koda-style fullscreen behavior: mouse capture is always on for
        // wheel scroll + in-app drag-select copy.
        let effective_mouse_capture = true;
        if let Ok(mut g) = tui_state.lock() {
            g.mouse_capture_on = effective_mouse_capture;
            g.notifications_enabled = self.runtime.config().ui.notifications;
            g.working_style = self.runtime.config().ui.working_style.clone();
            g.shimmer = self.runtime.config().ui.shimmer;
            g.sounds = self.runtime.config().ui.sounds;
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
            g.statusline_command = self.runtime.config().ui.statusline_command.clone();
            g.quiet_startup = self.runtime.config().ui.quiet_startup;
            g.vim_enabled = self.runtime.config().ui.vim_mode;
            g.key_bindings =
                crate::tui::keymap::build_key_bindings(&self.runtime.config().ui.keymap);
        }

        if self.runtime.config().ui.statusline_command.is_some() {
            crate::tui::statusline::spawn_poller(tui_state.clone());
        }

        crate::tui::markdown::warm_up_highlighting();
        let log_path = self.runtime.event_log_path();
        let saved = self.runtime.messages().to_vec();
        crate::tui::replay::restore_transcript(&log_path, &saved, &tui_state).await;
        show_restored_goal(&self.runtime.goal_cell(), &tui_state);

        // Populate the git branch name immediately so it appears on first render.
        let workspace = self.runtime.workspace_root();
        if let Some(branch) = git_current_branch(workspace)
            && let Ok(mut g) = tui_state.lock()
        {
            g.set_current_branch(&branch);
        }

        // Only for a fresh conversation: a resumed one (replayed blocks)
        // opens straight onto its history, like Claude Code.
        // A folder answered before keeps its choice: asked once per folder.
        let remembered = self
            .should_offer_startup_approve_all_popup()
            .then(|| dcode_ai_common::folders::remembered_permission(self.runtime.workspace_root()))
            .flatten();
        if let Some(choice) = remembered {
            // Applied quietly: the footer shows the mode.
            self.apply_startup_permission(&choice);
            if let Ok(mut g) = tui_state.lock() {
                g.set_agent_profile(&self.current_agent_label);
                g.set_permission_mode(&format!("{:?}", self.runtime.permission_mode()));
            }
        } else if self.should_offer_startup_approve_all_popup()
            && let Ok(mut g) = tui_state.lock()
            && g.active_question.is_none()
            && !g.blocks.iter().any(|b| matches!(b, DisplayBlock::User(_)))
        {
            g.active_question = Some(InteractiveQuestionPayload {
                question_id: STARTUP_APPROVE_ALL_QUESTION_ID.to_string(),
                call_id: STARTUP_APPROVE_ALL_QUESTION_ID.to_string(),
                prompt: "How should dcode-ai ask for permission this session?".into(),
                options: vec![
                    QuestionOption {
                        id: "manual".into(),
                        label: "Manual — ask before edits and commands".into(),
                        description: None,
                    },
                    QuestionOption {
                        id: "accept_edits".into(),
                        label: "Accept edits — edit files freely, ask before commands".into(),
                        description: None,
                    },
                    QuestionOption {
                        id: "auto".into(),
                        label: "Auto — run without asking; risky actions (delete, push, sudo) still ask".into(),
                        description: None,
                    },
                ],
                allow_custom: false,
                suggested_answer: "manual".into(),
                header: Some("Permissions".into()),
                ..Default::default()
            });
            g.open_question_modal();
        }

        // The pending approval / question maps belong to the session
        // runtime; `/resume` and `/project` swap it, so answers look the
        // current maps up here instead of keeping the first ones.
        if let Ok(mut g) = tui_state.lock() {
            let ctx = g.btw_ctx.clone();
            g.btw_tx = Some(crate::tui::btw::spawn_btw_worker(ctx, tui_state.clone()));
        }
        let live = LiveMaps::default();
        let bridge_handle: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>> = Arc::new(Mutex::new(
            Some(attach_runtime_to_tui(&mut self.runtime, &tui_state, &live)),
        ));
        let _ = log_path;

        // Answers must bypass the main `cmd_rx` loop: while `run_turn` is blocked inside
        // `ask_question`, that task never receives `TuiCmd::Submit` or `QuestionAnswer`.
        let (answer_tx, mut answer_rx) =
            tokio::sync::mpsc::unbounded_channel::<(String, QuestionSelection)>();
        let qp_dispatch = live.clone();
        let question_state = tui_state.clone();
        tokio::spawn(async move {
            while let Some((qid, sel)) = answer_rx.recv().await {
                let current = qp_dispatch.lock().ok().and_then(|m| m.question.clone());
                if !dispatch_question_answer(&current, &qid, sel)
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
        let approval_dispatch = live.clone();
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
                let current = approval_dispatch
                    .lock()
                    .ok()
                    .and_then(|m| m.approval.clone());
                if !dispatch_tool_approval(&current, &call_id, verdict)
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
        self.self_cmd_tx = Some(cmd_tx.clone());
        let st = tui_state.clone();
        let banner = self.run_mode;
        let cancel_flag = self.runtime.cancel_handle();
        let tui_cancel = cancel_flag.clone();
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

        let steer_inbox = self.runtime.steer_handle();
        if let Ok(mut g) = tui_state.lock() {
            g.steer_inbox = Some(steer_inbox.clone());
        }
        let mut pending_cmds: VecDeque<TuiCmd> = VecDeque::new();
        let mut mcp_auth: Option<cmd_mcp::McpAuthTask> = None;

        loop {
            // Messages typed during the last turn: ones the agent did not
            // get to (sent as Enter right as it finished) go first, then the
            // queued ones, one turn each.
            if pending_cmds.is_empty()
                && let Ok(mut g) = tui_state.lock()
                && g.active_approval.is_none()
                && g.active_question.is_none()
            {
                let leftover = g.steering_pending();
                if !leftover.is_empty() {
                    if let Some(inbox) = &g.steer_inbox
                        && let Ok(mut inbox) = inbox.lock()
                    {
                        inbox.clear();
                    }
                    pending_cmds.push_back(TuiCmd::Submit(leftover.join("\n\n")));
                } else if let Some(line) = g.followup_queue.pop_front() {
                    pending_cmds.push_back(TuiCmd::Submit(line));
                }
            }
            if pending_cmds.is_empty() {
                // Wake up now and then so a background job that finishes
                // while the agent is idle is reported without user input.
                match tokio::time::timeout(std::time::Duration::from_millis(500), cmd_rx.recv())
                    .await
                {
                    Ok(Some(cmd)) => pending_cmds.push_back(cmd),
                    Ok(None) => break,
                    Err(_) => {}
                }
            }
            // Finished background jobs: tell the agent in an automatic turn
            // once it is idle (Claude Code's task notifications). Turns run
            // inside this loop, so nothing is running at this point.
            if pending_cmds.is_empty() {
                let notes = match tui_state.lock() {
                    Ok(mut g) if g.active_approval.is_none() && g.active_question.is_none() => {
                        let notes = std::mem::take(&mut g.pending_job_notifications);
                        if !notes.is_empty() {
                            // The `●` notice line already shows it; do not
                            // echo the raw notification as a user message.
                            g.suppress_next_user_block = true;
                        }
                        notes
                    }
                    _ => Vec::new(),
                };
                if !notes.is_empty() {
                    pending_cmds.push_back(TuiCmd::Submit(notes.join("\n\n")));
                }
            }
            // An active `/goal` continues once everything else is done.
            if pending_cmds.is_empty() {
                let idle = tui_state.lock().is_ok_and(|g| {
                    g.active_approval.is_none()
                        && g.active_question.is_none()
                        && g.followup_queue.is_empty()
                });
                if idle {
                    let cell = self.runtime.goal_cell();
                    let (next, note) =
                        dcode_ai_runtime::goal::next_step(&cell, self.runtime.tokens_used());
                    if let Ok(mut g) = tui_state.lock() {
                        if let Some(note) = note {
                            g.push_block(DisplayBlock::System(note));
                            g.touch_transcript();
                        }
                        g.goal_label = goal_footer_label(&cell);
                        if next.is_some() {
                            g.suppress_next_user_block = true;
                        }
                    }
                    if let Some(prompt) = next {
                        pending_cmds.push_back(TuiCmd::Submit(prompt));
                    }
                }
            }

            while let Ok(cmd) = cmd_rx.try_recv() {
                pending_cmds.push_back(cmd);
            }

            let Some(cmd) = pending_cmds.pop_front() else {
                continue;
            };

            match cmd {
                TuiCmd::Backtrack {
                    user_index_from_end,
                    text,
                    scope,
                } => {
                    match self
                        .runtime
                        .rewind_to_user_message(user_index_from_end, &text, scope)
                    {
                        Ok((files, page)) => {
                            let _ = self.runtime.save().await;
                            let mut page_note = String::new();
                            let connected = dcode_ai_core::browser::shared()
                                .lock()
                                .await
                                .as_ref()
                                .is_some_and(|p| p.is_alive());
                            if let Some(url) = page
                                && connected
                                && dcode_ai_core::browser::last_url().as_ref() != Some(&url)
                            {
                                let root = self.runtime.workspace_root().to_path_buf();
                                let nav = serde_json::json!({"action": "navigate", "url": url});
                                if dcode_ai_core::browser::run_action(&nav, &root)
                                    .await
                                    .is_ok()
                                {
                                    page_note = format!(
                                        "\nBrowser back on {url} (anything already sent to a site stays sent)"
                                    );
                                }
                            }
                            let files_note = match files {
                                0 => "no files changed since".to_string(),
                                1 => "1 file restored".to_string(),
                                n => format!("{n} files restored"),
                            };
                            let msg = match scope {
                                RewindScope::Both => format!(
                                    "Rewound code and conversation ({files_note}) — edit the message and press Enter"
                                ),
                                RewindScope::Conversation => {
                                    "Rewound conversation — edit the message and press Enter"
                                        .to_string()
                                }
                                RewindScope::Code => format!("Rewound code ({files_note})"),
                            };
                            if let Ok(mut g) = tui_state.lock() {
                                g.push_block(DisplayBlock::System(format!("{msg}{page_note}")));
                                g.touch_transcript();
                            }
                        }
                        Err(e) => {
                            if let Ok(mut g) = tui_state.lock() {
                                g.push_error(format!("[rewind] {e}"));
                            }
                        }
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
                TuiCmd::CyclePermissionMode => {
                    let next = match self.runtime.permission_mode() {
                        PermissionMode::Default => PermissionMode::AcceptEdits,
                        PermissionMode::AcceptEdits => PermissionMode::Plan,
                        PermissionMode::Plan => PermissionMode::Auto,
                        _ => PermissionMode::Default,
                    };
                    self.set_mode_and_profile(next);
                    if let Ok(mut g) = tui_state.lock() {
                        g.set_agent_profile(&self.current_agent_label);
                        g.set_permission_mode(&format!("{next:?}"));
                    }
                }
                TuiCmd::ExitPlan { accept_edits } => {
                    let mode = if accept_edits {
                        PermissionMode::AcceptEdits
                    } else {
                        PermissionMode::Default
                    };
                    self.set_mode_and_profile(mode);
                    if let Ok(mut g) = tui_state.lock() {
                        g.set_agent_profile(&self.current_agent_label);
                        g.set_permission_mode(&format!("{mode:?}"));
                    }
                    pending_cmds.push_front(TuiCmd::Submit(
                        "The plan is approved. Implement it now, step by step.".into(),
                    ));
                }
                TuiCmd::SudoPassword(password) => {
                    // Dropping `password` without a pending install wipes it.
                    let Some(mut pending) = self.pending_sudo.take() else {
                        continue;
                    };
                    let (staged, target) = (pending.staged.clone(), pending.target.clone());
                    let bytes = password.into_bytes();
                    let result = tokio::task::spawn_blocking(move || {
                        crate::self_update::install_with_sudo(&staged, &target, Some(bytes))
                    })
                    .await
                    .unwrap_or_else(|e| Err(anyhow::anyhow!("{e}")));
                    let line = match result {
                        Ok(()) => {
                            let _ = std::fs::remove_dir_all(&pending.work);
                            format!(
                                "[update] installed {} at {} — restart dcode-ai to use it",
                                pending.tag,
                                pending.target.display()
                            )
                        }
                        Err(e)
                            if e.to_string() == crate::self_update::WRONG_PASSWORD
                                && pending.tries < 2 =>
                        {
                            pending.tries += 1;
                            let message = sudo_message(&pending);
                            self.pending_sudo = Some(pending);
                            if let Ok(mut g) = tui_state.lock() {
                                g.sudo_prompt =
                                    Some(Box::new(crate::tui::sudo_prompt::SudoPrompt::new(
                                        message,
                                        Some("Wrong password — try again.".into()),
                                    )));
                            }
                            continue;
                        }
                        Err(e) => {
                            let _ = std::fs::remove_dir_all(&pending.work);
                            format!("[update] not installed: {e}")
                        }
                    };
                    if let Ok(mut g) = tui_state.lock() {
                        g.push_block(DisplayBlock::System(line));
                        g.touch_transcript();
                    }
                }
                TuiCmd::SudoCancelled => {
                    if let Some(pending) = self.pending_sudo.take() {
                        let _ = std::fs::remove_dir_all(&pending.work);
                        if let Ok(mut g) = tui_state.lock() {
                            g.push_block(DisplayBlock::System(
                                "[update] not installed (cancelled) — /update to try again".into(),
                            ));
                            g.touch_transcript();
                        }
                    }
                }
                TuiCmd::SessionTitle {
                    session_id,
                    title,
                    replace,
                } => {
                    // The user may have moved to another session meanwhile.
                    if session_id == self.runtime.session_id()
                        && (replace || self.runtime.session_name().is_none())
                    {
                        self.runtime.set_session_name(Some(title.clone()));
                        let _ = self.runtime.save().await;
                        if replace && let Ok(mut g) = tui_state.lock() {
                            g.push_block(DisplayBlock::System(format!("[session-name] {title}")));
                            g.touch_transcript();
                        }
                    }
                }
                TuiCmd::Choice(action, index) => {
                    self.handle_choice(action, index, &tui_state).await;
                }
                TuiCmd::ChoiceDetail(action) => {
                    self.show_choice_detail(action, &tui_state);
                }
                TuiCmd::McpAction(action) => {
                    self.handle_mcp_action(action, &tui_state, &mut mcp_auth)
                        .await;
                }
                TuiCmd::PluginAction(action) => {
                    spawn_plugin_action(Arc::clone(&tui_state), action, self.self_cmd_tx.clone());
                }
                TuiCmd::ReloadExtensions => {
                    let msg = match self.runtime.reload_extensions().await {
                        Ok(m) => format!("Reloaded: {m}"),
                        Err(e) => format!("Reload failed: {e}"),
                    };
                    let count = self
                        .runtime
                        .config()
                        .mcp
                        .servers
                        .iter()
                        .filter(|s| s.enabled)
                        .count();
                    if let Ok(mut g) = tui_state.lock() {
                        g.mcp_server_count = count;
                        g.reload_slash_entries = true;
                        g.push_block(DisplayBlock::System(msg));
                        g.touch_transcript();
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
                TuiCmd::ApplyModel(model_name) => {
                    let resolved = self.runtime.config().model.resolve_alias(&model_name);
                    let mut cfg = self.runtime.config().clone();
                    cfg.apply_model_override(&resolved);
                    cfg.model.track_recent_model(&resolved);
                    let workspace = self.runtime.workspace_root().to_path_buf();
                    match self.runtime.apply_dcode_ai_config(cfg) {
                        Ok(()) => {
                            let save_result = persist_model_choice(
                                &workspace,
                                self.runtime.config().provider.default,
                                self.runtime.model(),
                            );
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
                    // The startup choices become this folder's remembered one.
                    let choice = match mode {
                        PermissionMode::Default => Some("manual"),
                        PermissionMode::AcceptEdits => Some("accept_edits"),
                        PermissionMode::Auto => Some("auto"),
                        _ => None,
                    };
                    if let Some(c) = choice {
                        let _ = dcode_ai_common::folders::remember_permission(
                            self.runtime.workspace_root(),
                            c,
                        );
                    }
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
                // `/resume` opens the picker; `/resume <id>` (or its last
                // digits) switches straight to that session — Claude Code's
                // `/resume`.
                TuiCmd::Submit(line)
                    if line.trim() == "/resume" || line.trim().starts_with("/resume ") =>
                {
                    let query = line.trim().trim_start_matches("/resume").trim();
                    if query.is_empty() {
                        self.handle_command("/sessions", ReplOutput::Tui(&tui_state))
                            .await?;
                    } else {
                        match self.runtime.list_session_ids().await {
                            Ok(ids) => match pick_session_by_suffix(&ids, query) {
                                Ok(id) => pending_cmds.push_front(TuiCmd::ResumeSession(id)),
                                Err(e) => {
                                    if let Ok(mut g) = tui_state.lock() {
                                        g.push_error(e);
                                    }
                                }
                            },
                            Err(e) => {
                                if let Ok(mut g) = tui_state.lock() {
                                    g.push_error(format!("Failed to list sessions: {e}"));
                                }
                            }
                        }
                    }
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
                                // Old bridge's channel is dead after the swap.
                                if let Ok(mut h) = bridge_handle.lock()
                                    && let Some(old) = h.take()
                                {
                                    old.abort();
                                }
                                self.runtime
                                    .adopt_turn_handles(tui_cancel.clone(), steer_inbox.clone());
                                let new_log = self.runtime.event_log_path();
                                let nb =
                                    attach_runtime_to_tui(&mut self.runtime, &tui_state, &live);
                                if let Ok(mut h) = bridge_handle.lock() {
                                    *h = Some(nb);
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
                                let saved = self.runtime.messages().to_vec();
                                crate::tui::replay::restore_transcript(
                                    &new_log, &saved, &tui_state,
                                )
                                .await;
                                if let Ok(mut g) = tui_state.lock() {
                                    let restored = g.blocks.len();
                                    g.push_block(DisplayBlock::System(format!(
                                        "Resumed session {session_id} — {restored} messages restored"
                                    )));
                                    g.transcript_follow_tail = true;
                                    g.touch_transcript();
                                    g.set_busy(false);
                                }
                                show_restored_goal(&self.runtime.goal_cell(), &tui_state);
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
                            let _ = persist_model_choice(
                                self.runtime.workspace_root(),
                                self.runtime.config().provider.default,
                                self.runtime.model(),
                            );
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
                    if let Err(e) = persist_theme_globally(applied.name) {
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
                                    self.runtime.adopt_turn_handles(
                                        tui_cancel.clone(),
                                        steer_inbox.clone(),
                                    );
                                    let nb =
                                        attach_runtime_to_tui(&mut self.runtime, &tui_state, &live);
                                    if let Ok(mut h) = bridge_handle.lock() {
                                        *h = Some(nb);
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
                            let choice = match selection {
                                QuestionSelection::Option { option_id } => option_id,
                                QuestionSelection::Suggested
                                | QuestionSelection::Options { .. }
                                | QuestionSelection::Custom { .. } => "manual".to_string(),
                            };
                            let note = self.apply_startup_permission(&choice);
                            if let Err(e) = dcode_ai_common::folders::remember_permission(
                                self.runtime.workspace_root(),
                                &choice,
                            ) {
                                tracing::warn!("could not remember the permission choice: {e}");
                            }
                            if let Ok(mut g) = tui_state.lock() {
                                g.active_question = None;
                                g.close_question_modal();
                                g.set_agent_profile(&self.current_agent_label);
                                g.set_permission_mode(&format!(
                                    "{:?}",
                                    self.runtime.permission_mode()
                                ));
                                g.blocks.push(DisplayBlock::System(format!(
                                    "[permissions] {note}. Shift+Tab changes the mode any time."
                                )));
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
                    if line.is_empty() {
                        continue;
                    }
                    // `# note` saves a memory note (recalled in future sessions).
                    if let Some(note) = memory_shortcut(&line) {
                        let msg = match self
                            .runtime
                            .append_memory_note("note", Some(note.to_string()))
                            .await
                        {
                            Ok(()) => format!("Saved to memory: {note}"),
                            Err(e) => format!("Could not save memory: {e}"),
                        };
                        if let Ok(mut g) = tui_state.lock() {
                            g.push_block(DisplayBlock::System(msg));
                            g.touch_transcript();
                        }
                        continue;
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
                        let ctx = std::mem::take(&mut g.pending_context).join("\n\n---\n\n");
                        format!("{ctx}\n\n---\n\n{expanded}")
                    } else {
                        expanded
                    };
                    if let Ok(mut g) = tui_state.lock() {
                        g.set_busy(true);
                    }
                    let attachments = if let Ok(mut g) = tui_state.lock() {
                        g.take_images_for_message(&expanded)
                    } else {
                        Vec::new()
                    };
                    // `think hard` / `ultrathink`: raise effort for this turn.
                    let effort_restore = match thinking_keyword_effort(&expanded) {
                        Some(level) => {
                            let current = self
                                .runtime
                                .config()
                                .provider
                                .anthropic
                                .effort
                                .clone()
                                .unwrap_or_else(|| "high".into());
                            if effort_rank(level) > effort_rank(&current) {
                                let saved = self.runtime.config().clone();
                                let mut cfg = saved.clone();
                                cfg.provider.anthropic.effort = Some(level.into());
                                cfg.provider.copilot.reasoning_effort = Some(level.into());
                                cfg.model.enable_thinking = true;
                                if self.runtime.apply_dcode_ai_config(cfg).is_ok() {
                                    if let Ok(mut g) = tui_state.lock() {
                                        g.show_toast(
                                            format!("Thinking effort {level} for this turn"),
                                            crate::tui::state::ToastKind::Info,
                                        );
                                    }
                                    Some(saved)
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        }
                        None => None,
                    };
                    let before = git_change_snapshot(self.runtime.workspace_root());
                    let turn_started = std::time::Instant::now();
                    let tokens_at_start = self.runtime.tokens_processed();
                    let turn = if attachments.is_empty() {
                        self.runtime.run_turn(&expanded).await
                    } else {
                        self.runtime
                            .run_turn_with_images(&expanded, attachments)
                            .await
                    };
                    if let Some(saved) = effort_restore {
                        let _ = self.runtime.apply_dcode_ai_config(saved);
                    }
                    let turn_ok = turn.is_ok();
                    if !turn_ok
                        && let Some(note) = dcode_ai_runtime::goal::pause_active(
                            &self.runtime.goal_cell(),
                            "the turn failed or was cancelled",
                        )
                        && let Ok(mut g) = tui_state.lock()
                    {
                        g.push_block(DisplayBlock::System(note));
                        g.goal_label = goal_footer_label(&self.runtime.goal_cell());
                    }
                    if let Err(e) = turn
                        // The agent already reported a cancelled run.
                        && !e.to_string().ends_with("run cancelled")
                    {
                        // Let queued events (e.g. the echoed prompt of a
                        // refused turn) render before the error line.
                        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
                        if let Ok(mut g) = tui_state.lock() {
                            g.push_error(e.to_string());
                        }
                    }
                    // `[session] verify_after_edits`: review turns that changed files.
                    if turn_ok
                        && self.runtime.config().session.verify_after_edits
                        && git_change_snapshot(self.runtime.workspace_root()) != before
                    {
                        let _ = self.run_verifier("", &ReplOutput::Tui(&tui_state)).await;
                    }
                    // `[session] cross_review`: a different model reviews the change.
                    if turn_ok
                        && self.runtime.config().session.cross_review
                        && git_change_snapshot(self.runtime.workspace_root()) != before
                    {
                        let _ = self
                            .run_review("", &ReplOutput::Tui(&tui_state), ReviewKind::SecondOpinion)
                            .await;
                    }
                    if let Ok(mut g) = tui_state.lock() {
                        g.set_busy(false);
                        let shells = g.background_shells.len();
                        let tokens = self
                            .runtime
                            .tokens_processed()
                            .saturating_sub(tokens_at_start);
                        if let Some(line) =
                            crate::tui::turn_summary(turn_started.elapsed(), shells, tokens)
                        {
                            g.push_block(DisplayBlock::TurnSummary(line));
                            g.touch_transcript();
                        }
                    }
                    // Name an unnamed session after its first turn, so
                    // `/resume` reads well.
                    let sid = self.runtime.session_id().to_string();
                    if self.runtime.session_name().is_none()
                        && self.titled_session.as_deref() != Some(sid.as_str())
                    {
                        self.titled_session = Some(sid);
                        self.spawn_session_title(&tui_state, false);
                    }
                }
            }
        }

        let _ = ui.await;
        // Claude Code's parting line: how to get back to this conversation.
        let worth_resuming = self
            .runtime
            .messages()
            .iter()
            .any(|m| m.role == dcode_ai_common::message::Role::User);
        let session_id = self.runtime.session_id().to_string();
        self.runtime.finish(EndReason::UserExit).await;
        if worth_resuming {
            println!("\nResume this session with:\ndcode-ai --resume {session_id}");
        }
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
        let mut command = Command::new(shell);
        dcode_ai_common::shell::prepare_agent_command(&mut command);
        let output = command
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

/// Tab completion for the plain (line-editor) prompt: slash commands,
/// skills, `!` shell commands and `@` workspace files.
struct ReplCompleter {
    workspace_root: std::path::PathBuf,
    skill_directories: Vec<std::path::PathBuf>,
}

impl Completer for ReplCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> reedline::CompletionResult {
        reedline::CompletionResult::Fresh {
            suggestions: self.complete_line(line, pos).into(),
            partial: None,
        }
    }
}

impl ReplCompleter {
    fn complete_line(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let mut suggestions = Vec::new();

        if let Some((at_byte, prefix)) = at_token_before_cursor(line, pos) {
            let files = discover_workspace_files(&self.workspace_root);
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
                    ..Default::default()
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
                        description: Some(
                            crate::slash_commands::slash_command_description(cmd).to_string(),
                        ),
                        extra: None,
                        span: reedline::Span { start: 0, end: pos },
                        append_whitespace: true,
                        style: None,
                        ..Default::default()
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
                        span: reedline::Span { start: 0, end: pos },
                        append_whitespace: true,
                        style: None,
                        ..Default::default()
                    });
                }
            }
        }

        // Load skills for completion
        if let Ok(skills) = SkillCatalog::discover(&self.workspace_root, &self.skill_directories) {
            for skill in skills {
                let skill_cmd = format!("/{}", skill.command);
                if skill_cmd.starts_with(line) {
                    suggestions.push(Suggestion {
                        value: skill_cmd,
                        description: skill.description,
                        extra: None,
                        span: reedline::Span { start: 0, end: pos },
                        append_whitespace: true,
                        style: None,
                        ..Default::default()
                    });
                }
            }
        }

        suggestions
    }
}

/// The current session's pending approval and question maps (see
/// [`attach_runtime_to_tui`]).
#[derive(Default)]
struct LiveMapsInner {
    approval: Option<crate::ipc_pending::ApprovalPendingMap>,
    question: Option<crate::ipc_pending::QuestionPendingMap>,
}
type LiveMaps = Arc<Mutex<LiveMapsInner>>;

/// Connect a (new or swapped-in) session runtime to the TUI: its event
/// stream, pending approval / question maps and sub-agent launcher.
fn attach_runtime_to_tui(
    runtime: &mut SessionRuntime,
    tui_state: &Arc<Mutex<crate::tui::state::TuiSessionState>>,
    live: &LiveMaps,
) -> tokio::task::JoinHandle<()> {
    let log_path = runtime.event_log_path();
    if let Ok(g) = tui_state.lock() {
        runtime.set_btw_handle(g.btw_ctx.clone());
    }
    let ipc = runtime.take_ipc_handle();
    let approval = runtime.take_ipc_approval_pending();
    let question = runtime.question_pending();
    if let Ok(mut m) = live.lock() {
        m.approval = approval.clone();
        m.question = question.clone();
    }
    if let Some(srx) = runtime.take_spawn_rx() {
        dcode_ai_runtime::supervisor::spawn_subagent_consumer(
            srx,
            runtime.session_id().to_string(),
            runtime.workspace_root().to_path_buf(),
            runtime.config().clone(),
            runtime.messages().to_vec(),
            runtime.event_tx(),
        );
    }
    match runtime.take_event_rx() {
        Some(rx) => spawn_tui_bridge(rx, log_path, ipc, approval, question, tui_state.clone()),
        None => tokio::spawn(async {}),
    }
}

use crate::repl_helpers::*;

#[cfg(test)]
mod tests {
    use super::*;

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
            parse_logout_target("claude"),
            Some(LogoutTarget::Anthropic)
        ));
        assert!(matches!(
            parse_logout_target("github"),
            Some(LogoutTarget::Copilot)
        ));
        assert!(matches!(
            parse_logout_target("all"),
            Some(LogoutTarget::All)
        ));
        assert!(parse_logout_target("openai").is_none());
    }
}

/// `# remember X` → `Some("remember X")`. Markdown-looking input (`##`,
/// `#!`, `#include`) and a bare `#` are left alone.
fn memory_shortcut(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("# ")?;
    let note = rest.trim();
    (!note.is_empty()).then_some(note)
}

#[cfg(test)]
mod memory_shortcut_tests {
    use super::memory_shortcut;

    #[test]
    fn only_hash_space_saves_memory() {
        assert_eq!(
            memory_shortcut("# use pnpm, not npm"),
            Some("use pnpm, not npm")
        );
        assert_eq!(memory_shortcut("## heading"), None);
        assert_eq!(memory_shortcut("#include <x>"), None);
        assert_eq!(memory_shortcut("# "), None);
        assert_eq!(memory_shortcut("fix # this"), None);
    }
}

/// `/context`: a Claude-style usage grid plus a per-category breakdown.
/// What fills the context, by part (`/context`).
#[derive(Debug, Default, Clone, Copy)]
pub struct ContextBreakdown {
    pub system: u64,
    pub skills: u64,
    pub tools: u64,
    pub mcp_tools: u64,
    pub summary: u64,
    pub conversation: u64,
    /// The provider's count for the last request (0 = none yet).
    pub measured: u64,
    /// The session's window and where clearing and compaction happen
    /// (0 = unknown).
    pub window: u64,
    pub compact_at: u64,
    pub prune_at: u64,
}

fn context_usage_lines(
    model: &str,
    b: ContextBreakdown,
    window: u64,
    message_count: usize,
) -> Vec<String> {
    let estimate = b.system + b.skills + b.tools + b.mcp_tools + b.summary + b.conversation;
    // What the provider counted beyond the estimate: thinking it keeps,
    // its tokenizer, its own framing.
    let extra = b.measured.saturating_sub(estimate);
    let used = estimate + extra;
    let window = if b.window > 0 { b.window } else { window };
    let window = window.max(used).max(1);
    let fmt = |n: u64| -> String {
        if n >= 1_000 {
            format!("{:.1}k", n as f64 / 1_000.0)
        } else {
            n.to_string()
        }
    };
    let pct = |n: u64| n as f64 * 100.0 / window as f64;
    let parts = [
        ('▓', "System prompt", b.system),
        ('▓', "Skills list", b.skills),
        ('▒', "Tools", b.tools),
        ('▒', "MCP tools", b.mcp_tools),
        ('░', "Summary", b.summary),
        ('█', "Conversation", b.conversation),
        ('◆', "Provider extra", extra),
    ];
    // 10×10 grid, one cell per 1% of the window.
    let cells = |n: u64| ((n * 100).div_ceil(window)) as usize;
    let mut grid: Vec<char> = Vec::with_capacity(100);
    for (glyph, _, n) in parts {
        grid.extend(std::iter::repeat_n(glyph, cells(n)));
    }
    grid.truncate(100);
    grid.resize(100, '·');
    let mut lines = vec![
        format!(
            "{model} · {} / {} tokens ({:.0}%)",
            fmt(used),
            fmt(window),
            pct(used)
        ),
        String::new(),
    ];
    for row in grid.chunks(10) {
        lines.push(format!(
            "  {}",
            row.iter()
                .map(|c| c.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        ));
    }
    lines.push(String::new());
    let free = window.saturating_sub(used);
    for (glyph, label, n) in parts.into_iter().chain([('·', "Free space", free)]) {
        if n == 0 && label != "Free space" && label != "Conversation" {
            continue;
        }
        lines.push(format!(
            "  {glyph} {label:<14} {:>7}  ({:.1}%)",
            fmt(n),
            pct(n)
        ));
    }
    lines.push(String::new());
    lines.push(format!(
        "  Fixed part {} — sent with every request; /compact can't shrink it",
        fmt(b.system + b.skills + b.tools + b.mcp_tools)
    ));
    if extra > 0 {
        lines.push(format!(
            "  Provider extra {}: the provider counted more than dcode-ai's estimate",
            fmt(extra)
        ));
        lines.push("  (thinking it keeps, its own tokenizer and framing)".into());
    }
    if b.compact_at > 0 {
        lines.push(format!(
            "  Auto-compact at {} ({:.0}%) · old tool output cleared first",
            fmt(b.compact_at),
            pct(b.compact_at)
        ));
    }
    lines.push(format!(
        "{message_count} messages · estimates (~4 chars/token) · /compact frees space now"
    ));
    lines
}

#[cfg(test)]
mod context_usage_tests {
    use super::{ContextBreakdown, context_usage_lines};

    #[test]
    fn grid_and_breakdown_add_up() {
        let b = ContextBreakdown {
            system: 2_000,
            tools: 8_000,
            conversation: 10_000,
            ..Default::default()
        };
        let lines = context_usage_lines("m", b, 200_000, 12);
        assert!(
            lines[0].contains("20.0k / 200.0k tokens (10%)"),
            "{lines:?}"
        );
        let grid: String = lines[2..12].join("");
        assert_eq!(grid.matches('▓').count(), 1);
        assert_eq!(grid.matches('▒').count(), 4);
        assert_eq!(grid.matches('█').count(), 5);
        assert_eq!(grid.matches('·').count(), 90);
        assert!(
            lines
                .iter()
                .any(|l| l.contains("Free space") && l.contains("180.0k"))
        );
    }

    #[test]
    fn provider_extra_is_what_was_measured_beyond_the_estimate() {
        let b = ContextBreakdown {
            system: 10_000,
            conversation: 20_000,
            measured: 80_000,
            ..Default::default()
        };
        let lines = context_usage_lines("m", b, 200_000, 3);
        assert!(lines[0].contains("80.0k / 200.0k"), "{lines:?}");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("Provider extra") && l.contains("50.0k"))
        );
        assert!(lines.iter().any(|l| l.contains("Fixed part 10.0k")));
    }
}

/// Run a plugin browser action on a blocking thread (git clones, disk) and
/// report back into the open browser.
fn spawn_plugin_action(
    state: Arc<Mutex<TuiSessionState>>,
    action: crate::tui::plugin_browser::BrowserAction,
    reload_tx: Option<tokio::sync::mpsc::UnboundedSender<TuiCmd>>,
) {
    use crate::tui::plugin_browser::BrowserAction;
    // Plugin changes reach the live session (MCP servers, hooks) at once;
    // marketplace changes don't touch it.
    let reload = !matches!(
        action,
        BrowserAction::AddMarket(_)
            | BrowserAction::AddDefaultMarkets(_)
            | BrowserAction::UpdateMarket(_)
            | BrowserAction::RemoveMarket(_)
    );
    if let Ok(mut g) = state.lock()
        && let Some(b) = g.plugin_browser.as_mut()
    {
        b.busy = Some(action.progress());
        b.status = None;
    }
    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || action.run())
            .await
            .unwrap_or_else(|e| Err(format!("plugin task failed: {e}")));
        if reload
            && result.is_ok()
            && let Some(tx) = &reload_tx
        {
            let _ = tx.send(TuiCmd::ReloadExtensions);
        }
        if let Ok(mut g) = state.lock() {
            g.reload_slash_entries = true;
            match g.plugin_browser.as_mut() {
                Some(b) => b.finish(result),
                None => {
                    let (msg, kind) = match result {
                        Ok(m) => (m, crate::tui::state::ToastKind::Success),
                        Err(e) => (e, crate::tui::state::ToastKind::Error),
                    };
                    g.show_toast(msg, kind);
                }
            }
        }
    });
}

/// Replace each `` !`cmd` `` in a command-file prompt with the command's
/// output when `allowed` permits it (Claude Code semantics); otherwise
/// leave a note that it was not run.
async fn expand_inline_shell(prompt: &str, allowed: &[String], cwd: &std::path::Path) -> String {
    const MAX_OUTPUT: usize = 8_000;
    let mut out = prompt.to_string();
    for cmd in dcode_ai_core::skills::inline_shell_commands(prompt) {
        let marker = format!("!`{cmd}`");
        // Run what allowed-tools permits, plus commands dcode-ai's classifier
        // rates read-only (`git diff`, `git log`, …), which are always safe.
        let read_only = matches!(
            dcode_ai_core::command_safety::classify_command(&cmd),
            dcode_ai_core::command_safety::CommandSafety::SafeReadOnly
        );
        let replacement = if read_only || dcode_ai_core::skills::bash_allowed(allowed, &cmd) {
            let run = dcode_ai_common::provider_runtime::system_shell_command(&cmd)
                .current_dir(cwd)
                .kill_on_drop(true)
                .output();
            match tokio::time::timeout(std::time::Duration::from_secs(15), run).await {
                Ok(Ok(o)) => {
                    let mut text = String::from_utf8_lossy(&o.stdout).to_string();
                    let err = String::from_utf8_lossy(&o.stderr);
                    if !err.trim().is_empty() {
                        text.push_str(&err);
                    }
                    let text = text.trim_end();
                    let text: String = text.chars().take(MAX_OUTPUT).collect();
                    format!(
                        "\n```\n{}\n```",
                        if text.is_empty() {
                            "(no output)"
                        } else {
                            &text
                        }
                    )
                }
                Ok(Err(e)) => format!("`{cmd}` (failed to run: {e})"),
                Err(_) => format!("`{cmd}` (timed out)"),
            }
        } else {
            format!("`{cmd}` (not run: not permitted by the command's allowed-tools)")
        };
        out = out.replacen(&marker, &replacement, 1);
    }
    out
}

#[cfg(all(test, unix))]
mod inline_shell_tests {
    use super::expand_inline_shell;

    #[tokio::test]
    async fn runs_only_allowed_inline_commands() {
        let dir = tempfile::tempdir().unwrap();
        let allowed = vec!["Bash(echo:*)".to_string()];
        let out = expand_inline_shell(
            "a: !`echo hello` b: !`touch x` c: !`pwd`",
            &allowed,
            dir.path(),
        )
        .await;
        assert!(out.contains("```\nhello\n```"), "{out}");
        assert!(out.contains("`touch x` (not run"), "{out}");
        assert!(!dir.path().join("x").exists());
        // Read-only commands run even when not listed.
        assert!(!out.contains("`pwd` (not run"), "{out}");
    }
}

/// Remember the provider and model across launches, like Claude Code keeps
/// your last `/model`: the global config, the workspace-local copy (when one
/// exists, so it cannot shadow the choice) and the sign-in preference that
/// startup uses to pick a provider. Only these fields change.
fn persist_model_choice(
    workspace: &std::path::Path,
    kind: ProviderKind,
    model: &str,
) -> Result<(), String> {
    use dcode_ai_common::config::DcodeAiConfig;
    let apply = |c: &mut DcodeAiConfig| {
        c.set_default_provider(kind);
        c.apply_model_override(model);
        c.model.track_recent_model(model);
    };
    let mut global = DcodeAiConfig::load_global_file().map_err(|e| e.to_string())?;
    apply(&mut global);
    global.save_global().map_err(|e| e.to_string())?;
    if dcode_ai_common::config::workspace_config_path(workspace).exists() {
        let mut local = DcodeAiConfig::load_workspace_file(workspace).map_err(|e| e.to_string())?;
        apply(&mut local);
        local
            .save_workspace_file(workspace)
            .map_err(|e| e.to_string())?;
    }
    let mut auth = dcode_ai_common::auth::AuthStore::load().unwrap_or_default();
    auth.preferred_provider = Some(match kind {
        ProviderKind::Anthropic => dcode_ai_common::auth::LoggedProvider::Anthropic,
        ProviderKind::Copilot => dcode_ai_common::auth::LoggedProvider::Copilot,
    });
    auth.save().map_err(|e| e.to_string())
}

/// Save the theme in the user's global config (like Claude Code, the theme
/// follows you across projects), leaving every other setting untouched.
fn persist_theme_globally(name: &str) -> Result<(), String> {
    let mut global =
        dcode_ai_common::config::DcodeAiConfig::load_global_file().map_err(|e| e.to_string())?;
    global.ui.theme = Some(name.to_string());
    global.save_global().map_err(|e| e.to_string())?;
    // Older versions saved the theme per workspace; drop that copy so it
    // cannot shadow the global choice.
    if let Ok(cwd) = std::env::current_dir()
        && let Ok(mut local) = dcode_ai_common::config::DcodeAiConfig::load_workspace_file(&cwd)
        && local.ui.theme.is_some()
    {
        local.ui.theme = None;
        local.save_workspace_file(&cwd).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Cheap fingerprint of the working tree's changes (detects edits in a turn).
fn git_change_snapshot(ws: &std::path::Path) -> Option<String> {
    std::process::Command::new("git")
        .args(["diff", "HEAD", "--stat"])
        .current_dir(ws)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                git_changed_files(ws).join("\n")
            )
        })
}

/// The verifier reports `No problems found.` when everything checks out.
/// Prompt for `/agents new`: produce one agent file in Claude Code format.
fn agent_generation_prompt(description: &str) -> String {
    format!(
        "Write a sub-agent definition file for a coding assistant. The user wants an agent that: \
{description}\n\n\
Reply with ONLY the file content, no code fences, in exactly this format:\n\
---\n\
name: <short-kebab-case-name>\n\
description: <one sentence saying what it does and when to use it, starting with \"Use when\" or \"Use for\">\n\
tools: <comma-separated subset of: Read, Grep, Glob, Edit, Write, Bash, WebFetch, WebSearch — only what it needs; omit this line to allow all tools>\n\
model: inherit\n\
---\n\
<the agent's system prompt in second person: its role, step-by-step approach, what to check, \
what to return and in what format, and what it must not do. 10-30 lines.>"
    )
}

/// Validate the model's reply as an agent file and save it as
/// `<dir>/<name>.md` (never overwriting an existing agent).
fn save_generated_agent(
    dir: &std::path::Path,
    reply: &str,
) -> Result<(std::path::PathBuf, dcode_ai_core::agent_defs::AgentDef), String> {
    let text = reply.trim();
    let text = text
        .strip_prefix("```markdown")
        .or_else(|| text.strip_prefix("```md"))
        .or_else(|| text.strip_prefix("```"))
        .map(|t| t.trim_end().trim_end_matches("```").trim())
        .unwrap_or(text);
    if !text.starts_with("---") {
        return Err(format!("the model did not return an agent file:\n{text}"));
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let tmp = dir.join(".new-agent.md.tmp");
    std::fs::write(&tmp, format!("{text}\n")).map_err(|e| e.to_string())?;
    let parsed = dcode_ai_core::agent_defs::parse_agent_file(&tmp);
    let def = match parsed {
        Ok(d) => d,
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!(
                "generated file is not a valid agent ({e}):\n{text}"
            ));
        }
    };
    let name: String = def
        .name
        .to_ascii_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if name.is_empty() {
        let _ = std::fs::remove_file(&tmp);
        return Err("generated agent has no name".into());
    }
    let path = dir.join(format!("{name}.md"));
    if path.exists() {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "{} already exists — not overwritten",
            path.display()
        ));
    }
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    let def = dcode_ai_core::agent_defs::parse_agent_file(&path).unwrap_or(def);
    Ok((path, def))
}

/// Config and label for the `/second-opinion` reviewer: `[session]
/// second_opinion_model` when set, otherwise the strongest available model
/// of a different family than `current_model` (GPT/Gemini on Copilot for a
/// Claude session, Claude for anything else).
async fn second_opinion_config(
    config: &dcode_ai_common::config::DcodeAiConfig,
    current_model: &str,
) -> Result<(dcode_ai_common::config::DcodeAiConfig, String), String> {
    use dcode_ai_common::config::ProviderKind;
    let mut c = config.clone();
    if let Some(spec) = config.session.second_opinion_model.as_deref() {
        let (provider, model) = match spec.split_once(':') {
            Some((p, m)) if ProviderKind::from_cli_name(p).is_some() => {
                (ProviderKind::from_cli_name(p), m)
            }
            _ => (None, spec),
        };
        if let Some(kind) = provider {
            c.set_default_provider(kind);
        }
        c.apply_model_override(model);
        let label = format!("Second opinion ({})", c.model.default_model);
        return Ok((c, label));
    }
    let current_is_claude = current_model.to_ascii_lowercase().contains("claude");
    let copilot_ready = config.provider.api_key_present_for(ProviderKind::Copilot)
        || dcode_ai_common::auth::AuthStore::load()
            .unwrap_or_default()
            .copilot
            .is_some();
    if !current_is_claude
        && dcode_ai_common::provider_runtime::anthropic_signed_in(&config.provider.anthropic)
    {
        c.set_default_provider(ProviderKind::Anthropic);
        let label = format!("Second opinion ({})", c.model.default_model);
        return Ok((c, label));
    }
    if !copilot_ready {
        return Err(
            "no reviewer from another model family is available — sign in to \
             GitHub Copilot or set [session] second_opinion_model"
                .into(),
        );
    }
    c.set_default_provider(ProviderKind::Copilot);
    let ids = dcode_ai_runtime::model_limits_api::fetch_provider_model_ids(&c)
        .await
        .map_err(|e| format!("could not list Copilot models: {e}"))?;
    let model = pick_reviewer_model(current_model, &ids).ok_or_else(|| {
        "Copilot offers no model from another family — set [session] second_opinion_model"
            .to_string()
    })?;
    c.apply_model_override(&model);
    Ok((c, format!("Second opinion ({model})")))
}

/// Strongest model in `available` from a different family than `current`:
/// for Claude, the newest full GPT (then Gemini, then any non-Claude); for
/// others, the newest Claude.
fn pick_reviewer_model(current: &str, available: &[String]) -> Option<String> {
    let version = |id: &str| -> Vec<u32> {
        id.split(|c: char| !c.is_ascii_digit())
            .filter_map(|p| p.parse().ok())
            .collect()
    };
    let small = |id: &str| {
        ["mini", "nano", "flash", "lite", "haiku"]
            .iter()
            .any(|s| id.contains(s))
    };
    let newest = |pred: &dyn Fn(&str) -> bool| -> Option<String> {
        available
            .iter()
            .filter(|id| pred(&id.to_ascii_lowercase()))
            .max_by_key(|id| (!small(&id.to_ascii_lowercase()), version(id)))
            .cloned()
    };
    if current.to_ascii_lowercase().contains("claude") {
        newest(&|id| id.starts_with("gpt-") && !id.contains("codex"))
            .or_else(|| newest(&|id| id.starts_with("gemini")))
            .or_else(|| newest(&|id| !id.contains("claude")))
    } else {
        newest(&|id| id.starts_with("claude"))
    }
}

fn verifier_found_problems(report: &str) -> bool {
    let r = report.trim();
    !r.is_empty() && !r.to_ascii_lowercase().starts_with("no problems found")
}

#[cfg(test)]
mod verifier_tests {
    use super::verifier_found_problems;

    #[test]
    fn generated_agents_are_validated_and_never_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let reply = "```markdown\n---\nname: Test Runner\ndescription: Use when tests fail.\ntools: Read, Bash\nmodel: inherit\n---\nRun the tests and fix failures.\n```";
        let (path, def) = super::save_generated_agent(dir.path(), reply).unwrap();
        assert!(path.ends_with("test-runner.md"), "{}", path.display());
        assert_eq!(def.description, "Use when tests fail.");
        assert!(super::save_generated_agent(dir.path(), reply).is_err());
        assert!(super::save_generated_agent(dir.path(), "no frontmatter").is_err());
    }

    #[test]
    fn reviewer_comes_from_another_family() {
        let ids: Vec<String> = [
            "claude-sonnet-5",
            "gpt-5-mini",
            "gpt-5.2",
            "gpt-5.3-codex",
            "gemini-3.8-flash",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            super::pick_reviewer_model("claude-sonnet-5", &ids).as_deref(),
            Some("gpt-5.2")
        );
        assert_eq!(
            super::pick_reviewer_model("gpt-5.2", &ids).as_deref(),
            Some("claude-sonnet-5")
        );
    }

    #[test]
    fn clean_reports_do_not_trigger_fixes() {
        assert!(!verifier_found_problems(
            "No problems found. Ran cargo test."
        ));
        assert!(!verifier_found_problems("  "));
        assert!(verifier_found_problems("src/a.rs:3 — off by one — use <="));
    }
}
