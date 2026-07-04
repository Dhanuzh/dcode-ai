//! Transcript + status driven by `AgentEvent`.

use dcode_ai_common::config::ProviderKind;
use dcode_ai_common::event::{BusyState, InteractiveQuestionPayload, QuestionSelection};
use dcode_ai_common::message::ImageAttachment;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

#[derive(Debug, Clone)]
pub enum DisplayBlock {
    User(String),
    Assistant(String),
    ToolRunning {
        name: String,
        call_id: String,
        input: String,
    },
    ApprovalPending(ApprovalRequest),
    ApprovalResolved {
        tool: String,
        approved: bool,
    },
    ToolDone {
        name: String,
        call_id: String,
        ok: bool,
        detail: String,
        /// Wall-clock duration of the call, if its start was observed.
        duration_ms: Option<u64>,
    },
    /// Model thinking/reasoning content (shown collapsed before assistant reply).
    Thinking(String),
    /// Interactive `ask_question` prompt (options + suggested answer).
    Question(InteractiveQuestionPayload),
    System(String),
    ErrorLine(String),
}

#[derive(Debug, Clone)]
pub struct ApprovalRequest {
    pub call_id: String,
    pub tool: String,
    pub description: String,
    pub input: String,
}

impl ApprovalRequest {
    /// Suggested "always allow" glob for this tool call, derived from the tool
    /// name and parsed input (best-effort: invalid JSON → empty input).
    pub fn allow_pattern(&self) -> String {
        let input_json: serde_json::Value = serde_json::from_str(&self.input).unwrap_or_default();
        dcode_ai_core::approval::suggest_allow_pattern(&self.tool, &input_json)
    }
}

/// One row in the sidebar for a child / sub-agent session.
#[derive(Debug, Clone)]
pub struct SubagentRow {
    pub id: String,
    pub task: String,
    pub phase: String,
    pub detail: String,
    pub running: bool,
    pub skill: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PinnedNote {
    #[allow(dead_code)]
    pub title: String,
    pub body: String,
}

/// A brief floating notification that auto-dismisses after a timeout.
#[derive(Debug, Clone)]
pub struct Toast {
    #[allow(dead_code)]
    pub message: String,
    #[allow(dead_code)]
    pub kind: ToastKind,
    pub expires_at: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

impl Toast {
    pub fn is_expired(&self) -> bool {
        Instant::now() >= self.expires_at
    }
}

pub struct SessionPickerEntry {
    pub id: String,
    pub label: String,
    pub search_text: String,
    /// Last few messages as a preview (populated from event log).
    #[allow(dead_code)]
    pub preview: String,
}

/// Status of an API key validation during onboarding.
#[derive(Debug, Clone)]
pub enum OnboardingValidation {
    Validating,
    Valid,
    Failed(String),
}

pub struct TuiSessionState {
    pub blocks: Vec<DisplayBlock>,
    /// In-progress assistant text (shown below committed blocks until finalized).
    pub streaming_assistant: Option<String>,
    /// In-progress thinking/reasoning tokens from the model.
    pub streaming_thinking: Option<String>,
    /// Composer input engine (Koda-style textarea state object).
    pub composer: crate::tui::composer::TextArea,
    /// Compatibility mirror for existing render/search code paths.
    pub input_buffer: String,
    /// Compatibility mirror for existing render/search code paths.
    pub cursor_char_idx: usize,
    /// Scroll offset in *lines* (flattened transcript).
    pub scroll_lines: usize,
    /// When true, transcript stays pinned to the bottom as new output arrives.
    pub transcript_follow_tail: bool,
    pub session_id: String,
    /// Workspace root for resolving attachment paths and clipboard import.
    pub workspace_root: PathBuf,
    /// Workspace root (from `SessionStarted`), for sidebar context.
    pub workspace_display: String,
    /// Images to send on the next user message (TUI only).
    pub staged_image_attachments: Vec<ImageAttachment>,
    /// Extra context blocks (from `/web` or `/run`) to prepend to the next
    /// outgoing user message and then clear.
    pub pending_context: Vec<String>,
    /// Live view of spawned sub-agents (updated from child activity events).
    pub subagents: Vec<SubagentRow>,
    pub model: String,
    pub agent_profile: String,
    pub permission_mode: String,
    /// Compact current process title for the live status row.
    pub process_title: String,
    /// Optional process detail for the live status row.
    pub process_detail: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
    /// Estimated current context-window occupancy (from the latest CostUpdated),
    /// used to render the status-bar ctx gauge.
    pub context_tokens: u64,
    /// Output tokens streamed so far **in the current turn** (reset each time a
    /// new busy turn begins). Shown live in the status bar while the agent is
    /// streaming, so the user can see tokens accumulating in real-time.
    pub turn_output_tokens: u64,
    pub started: Instant,
    /// Start time per in-flight tool call_id, for duration badges on completion.
    pub tool_started: HashMap<String, Instant>,
    pub busy: bool,
    /// Current busy state (for animated indicator).
    pub current_busy_state: BusyState,
    /// When the current busy state started (for animation frame selection).
    pub busy_state_since: Instant,
    pub should_exit: bool,
    /// Selected row in slash-command popup (↑↓ or click).
    pub slash_menu_index: usize,
    /// Centered command palette opened via Ctrl+P.
    pub command_palette_open: bool,
    /// Filter text for the command palette.
    pub command_palette_query: String,
    /// Approval request currently waiting for a local TUI answer.
    pub active_approval: Option<ApprovalRequest>,
    /// Allow-pattern of the most recent approval request (persists after the
    /// request resolves) so `/approve` can re-allow it for the session.
    pub last_approval_pattern: Option<String>,
    /// Selected action in approval popup: 0=approve, 1=always approve, 2=deny.
    pub approval_option_index: usize,
    /// Per-hunk accept/reject selection for the active approval (git add -p style).
    /// `true` = hunk accepted, `false` = rejected. Empty when not in hunk mode.
    pub approval_hunk_selection: Vec<bool>,
    /// Currently focused hunk index (for ↑/↓ navigation in hunk mode).
    pub approval_hunk_cursor: usize,
    /// When true, the approval popup is in hunk-selection mode instead of
    /// the simple approve/deny radio.
    pub approval_hunk_mode: bool,
    /// When set, the composer answers this question (see status hint).
    pub active_question: Option<InteractiveQuestionPayload>,
    /// Resolved answers keyed by `question_id`, used to render selected choice
    /// highlight in transcript question blocks.
    pub answered_questions: HashMap<String, QuestionSelection>,
    /// When true, thinking blocks render in full instead of capping at a
    /// preview; toggled by clicking a thinking header or its "+N more" line.
    pub thinking_expanded: bool,
    /// Current git branch name (updated on branch switch).
    pub current_branch: String,
    /// Branch picker popup state.
    pub branch_picker_open: bool,
    /// Filter text in the branch picker.
    pub branch_picker_query: String,
    /// Selected index in the branch picker list.
    pub branch_picker_index: usize,
    /// List of branches for the picker (refreshed on open).
    pub branch_picker_branches: Vec<String>,
    /// Bounding rect of the branch chip in the status bar (for click hit-testing).
    pub branch_chip_bounds: Option<ratatui::layout::Rect>,
    /// Right sidebar visibility toggle (true = expanded, false = collapsed).
    pub sidebar_open: bool,
    /// Bounding rect of the sidebar toggle chip in the status bar.
    pub sidebar_toggle_bounds: Option<ratatui::layout::Rect>,
    /// Pick default LLM provider (or provider for API key) — TUI overlay.
    pub provider_picker_open: bool,
    pub provider_picker_index: usize,
    /// When true, picking a row sets `pending_api_key_provider` instead of applying provider.
    pub provider_picker_for_api_key: bool,
    /// After choosing a provider for API key, next non-command line is the secret.
    pub pending_api_key_provider: Option<ProviderKind>,
    /// Selected row when `@` file completion panel is visible.
    pub at_menu_index: usize,
    /// OpenCode-style "Connect a provider" modal (`/connect`).
    pub connect_modal_open: bool,
    pub connect_search: String,
    /// Index among selectable provider rows (not section headers).
    pub connect_menu_index: usize,
    /// Scroll offset for the connect modal viewport.
    pub connect_modal_scroll: usize,
    /// Guard against immediately consuming the same Enter key that opened the modal.
    pub connect_modal_ignore_enter_once: bool,
    /// API key entry modal (used by `/connect` and `/apikey` TUI flows).
    pub api_key_modal_open: bool,
    pub api_key_target_provider: Option<ProviderKind>,
    pub api_key_input: String,
    pub api_key_target_has_existing: bool,
    /// When true, Enter should connect to this provider after saving/confirming the key.
    pub api_key_connect_after_save: bool,
    /// Anthropic OAuth code entry modal (URL + pasted authorization code).
    pub anthropic_oauth_modal_open: bool,
    pub anthropic_oauth_url: String,
    pub anthropic_oauth_code_verifier: String,
    pub anthropic_oauth_code_input: String,
    /// Generic info popup (read-only scrollable lines).
    pub info_modal_open: bool,
    pub info_modal_title: String,
    pub info_modal_lines: Vec<String>,
    pub info_modal_scroll: usize,
    pub info_modal_hscroll: usize,
    pub info_modal_view_rows: usize,
    pub info_modal_view_cols: usize,
    /// Model picker popup (searchable model/provider list).
    pub model_picker_open: bool,
    pub model_picker_search: String,
    pub model_picker_index: usize,
    pub model_picker_entries: Vec<ModelPickerEntry>,
    /// Scroll offset (first visible row) in the model picker viewport.
    pub model_picker_scroll: usize,
    /// Ctrl+X leader key pending (next keypress is dispatched as shortcut).
    pub leader_pending: bool,
    /// Permission mode picker popup.
    pub permission_picker_open: bool,
    pub permission_picker_index: usize,
    /// Agent profile picker popup.
    pub agent_picker_open: bool,
    pub agent_picker_index: usize,
    /// Question modal popup (arrow-key option picker).
    pub question_modal_open: bool,
    pub question_modal_index: usize,
    pub question_modal_scroll: usize,
    /// Command palette selection index (separate from slash_menu_index).
    pub palette_index: usize,
    /// Session picker popup (interactive list with resume).
    pub session_picker_open: bool,
    pub session_picker_search: String,
    pub session_picker_index: usize,
    pub session_picker_entries: Vec<SessionPickerEntry>,
    /// Scroll offset for the session picker viewport.
    pub session_picker_scroll: usize,
    /// When true, the onboarding gate is active — connect modal is locked open.
    pub onboarding_mode: bool,
    /// Result of the most recent API key validation attempt (None = no attempt yet).
    pub validation_status: Option<OnboardingValidation>,
    /// Queued steering messages (Enter while busy).
    pub queued_steering: usize,
    /// Queued follow-up messages (Alt+Enter while busy).
    pub queued_followup: usize,
    /// Preview rows for queued messages rendered above the status bar.
    pub queue_preview_items: Vec<String>,
    /// Number of enabled MCP servers (status bar segment).
    pub mcp_server_count: usize,
    /// Render fenced code blocks with line numbers in assistant markdown.
    pub code_line_numbers: bool,
    /// Suppress transient startup notices (MCP server status, permissions
    /// approval). Seeded from `ui.quiet_startup`; toggled by `/quiet`.
    pub quiet_startup: bool,
    /// Monotonic revision for transcript render caching.
    pub transcript_rev: u64,
    /// Maps paste placeholder tokens (e.g. `[pasted 5 lines #1]`) to their real content.
    /// Cleared when the input buffer is submitted or cleared.
    pub paste_store: HashMap<String, String>,
    /// Counter for generating unique paste tokens.
    pub paste_counter: u32,
    /// Runtime mouse-capture state for fullscreen TUI mouse handling.
    /// Kept true for koda-style wheel + drag-select behavior.
    pub mouse_capture_on: bool,
    /// Theme picker popup state.
    pub theme_picker_open: bool,
    pub theme_picker_index: usize,
    pub theme_picker_entries: Vec<String>,
    /// Pinned notes shown at transcript top.
    pub pinned_notes: Vec<PinnedNote>,
    /// Pins modal popup (`Ctrl+O`).
    pub pins_modal_open: bool,
    pub pins_modal_index: usize,
    /// Sub-agent details modal popup (`Ctrl+G`).
    pub subagent_modal_open: bool,
    pub subagent_modal_index: usize,
    /// Transcript search popup (`Ctrl+F`).
    pub transcript_search_open: bool,
    pub transcript_search_query: String,
    /// Selected match index within the current filtered match list.
    pub transcript_search_index: usize,
    /// Composer history search overlay (`Ctrl+R` in fullscreen TUI).
    pub composer_history_search_open: bool,
    pub composer_history_search_query: String,
    pub composer_history_search_index: usize,
    /// Typed menu state used by viewport rendering.
    pub menu_content: crate::tui::tui_types::MenuContent,
    /// Per-tool-block collapse override keyed by `call_id`.
    /// `Some(true)` = collapsed, `Some(false)` = expanded, absent = use default.
    pub tool_block_collapsed: HashMap<String, bool>,
    /// Global "collapse all tool blocks" toggle (z key). When true, `ToolDone`/
    /// `ToolRunning` blocks render header only unless overridden in `tool_block_collapsed`.
    pub all_tools_collapsed: bool,
    /// Status-bar items the user has hidden via `/statusline` (keys: "agent",
    /// "effort", "time", "context", "model"). Populated from config at startup.
    pub statusline_hidden: Vec<String>,
    /// Latest `update_plan` checklist (goal tracking) — the current task plan,
    /// recalled anytime via `/goals`. `None` until the agent sets a plan.
    pub current_plan: Option<String>,
    /// Resolved custom keybindings (default + effective key per action) from
    /// config, applied before key dispatch. Empty = built-in keys only.
    pub key_bindings: Vec<crate::tui::app::KeyBinding>,
    /// Active mouse text-selection. `Some` while user is dragging or after
    /// release until input/scroll clears it.
    pub mouse_selection: Option<crate::tui::mouse_select::Selection>,
    /// History panel viewport rect captured each frame, used to translate
    /// raw mouse coordinates → buffer-space rows for selection.
    pub history_rect: Option<(u16, u16, u16, u16)>,
    /// Wall-clock timestamps (Unix seconds) parallel to `blocks`, recorded when
    /// each block is first pushed. Used to render `HH:MM` dim labels in the
    /// transcript header rows.
    pub block_timestamps: Vec<u64>,
    /// Wall-clock timestamp of when the most recent assistant turn completed
    /// (BusyState → Idle transition). Used to compute provider latency.
    pub last_turn_latency_ms: Option<u64>,
    /// Wall-clock time (Unix ms) when the current turn started (first non-idle state).
    pub(crate) turn_started_at: Option<u64>,
    /// User-adjustable transcript column width offset (from default terminal width).
    /// Positive = narrower (zoom in / larger text density), negative = wider margin.
    /// Clamped to [-20, +40] at render time so layout never breaks.
    pub transcript_zoom_offset: i32,
    /// True after the context manager has compacted at least once this session.
    pub context_compacted: bool,
    /// True while context compaction is actively running.
    pub compaction_in_progress: bool,
    /// Notification counter for events that arrived while the user was typing
    /// or scrolled away. Cleared when transcript scrolls to bottom.
    pub notification_count: u16,
    /// When true, the next `MessageReceived { role: "user" }` event will NOT
    /// create a User display block. Used by `!` auto-response to hide the
    /// synthetic prompt from the transcript.
    pub suppress_next_user_block: bool,
    /// Toast notification: a brief floating message that auto-dismisses.
    /// Rendered as an overlay in the bottom-right of the transcript area.
    pub toast: Option<Toast>,
    /// Block indices of collapsed assistant responses (click header to fold).
    pub collapsed_assistant_blocks: std::collections::HashSet<usize>,
    /// Whether extended thinking is currently enabled (for effort badge).
    pub thinking_enabled: bool,
    /// Current thinking budget (for effort level detection).
    pub thinking_budget: u32,
    /// Connected project directories for multi-project switching.
    pub connected_projects: Vec<ConnectedProject>,
    /// Project picker popup state.
    pub project_picker_open: bool,
    pub project_picker_index: usize,
    /// Number of blocks already flushed to terminal scrollback via insert_before.
    pub flushed_block_count: usize,
    /// When set, the draw loop purges the terminal scrollback + screen and
    /// re-flushes the banner (used by `/clear` and Ctrl+L in inline mode).
    pub request_clear: bool,
    /// When set, the main loop enters the full-screen transcript overlay
    /// (expand/collapse, scroll, reflow, raw copy mode).
    pub transcript_overlay_open: bool,
    /// Scroll offset (from top, in lines) for the transcript overlay.
    pub transcript_overlay_scroll: usize,
    /// When true, the overlay renders plain unstyled text for copy-friendly
    /// terminal selection.
    pub transcript_overlay_raw: bool,
    /// Live startup status per MCP server.
    pub mcp_server_statuses: HashMap<String, dcode_ai_common::event::McpStartupStatus>,
}

#[derive(Debug, Clone)]
pub struct ConnectedProject {
    pub name: String,
    pub path: std::path::PathBuf,
    pub active: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectPickerKey {
    Cancel,
    Up,
    Down,
    Accept,
    #[allow(dead_code)]
    Add,
    Remove,
}

#[derive(Debug, Clone)]
pub enum ModelPickerAction {
    SwitchProvider(ProviderKind),
    SwitchCopilot,
    ApplyModel(String),
}

#[derive(Debug, Clone)]
pub struct ModelPickerEntry {
    pub label: String,
    pub detail: String,
    pub action: ModelPickerAction,
    pub is_header: bool,
}

/// A key event for the composer history-search overlay, decoupled from
/// crossterm so the state transitions can be unit-tested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistorySearchKey {
    Cancel,
    Backspace,
    Up,
    Down,
    Accept,
    Char(char),
}

/// A key event for the branch-picker overlay (crossterm-decoupled, testable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchPickerKey {
    Cancel,
    Up,
    Down,
    Accept,
    Backspace,
    Char(char),
}

/// A key event for the interactive-question modal (crossterm-decoupled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuestionModalKey {
    Cancel,
    Up,
    Down,
    Accept,
}

/// A key event for the command palette overlay (crossterm-decoupled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandPaletteKey {
    Cancel,
    Up,
    Down,
    Accept,
    Backspace,
    Char(char),
}

/// A key event for the read-only info modal (crossterm-decoupled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InfoModalKey {
    Close,
    ScrollUp,
    ScrollDown,
    ScrollLeft,
    ScrollRight,
    Home,
    End,
}

/// A key event for the model picker overlay (crossterm-decoupled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelPickerKey {
    Cancel,
    Up,
    Down,
    Accept,
    Backspace,
    Char(char),
}

/// A key event for the session picker overlay (crossterm-decoupled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPickerKey {
    Cancel,
    Up,
    Down,
    Accept,
    Backspace,
    Char(char),
}

/// A key event for the default-provider picker overlay (crossterm-decoupled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderPickerKey {
    Cancel,
    Up,
    Down,
    Accept,
}

/// A key event for the pinned-notes modal (crossterm-decoupled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PinsModalKey {
    Close,
    Up,
    Down,
    Delete,
    Accept,
    Copy,
}

/// A key event for the connect-provider modal (crossterm-decoupled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectModalKey {
    Cancel,
    Up,
    Down,
    Accept,
    Backspace,
    Char(char),
}

/// What an accepted provider-picker selection means; the loop routes it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ProviderPickerOutcome {
    /// Set this provider as the default.
    Apply(ProviderKind),
    /// The pick was for an API-key/login flow; the loop chooses login vs prompt.
    ForApiKey(ProviderKind),
}

/// Result of applying a key to the question modal. Channel routing of an
/// accepted answer stays in the event loop; this only decides *what* happened.
#[derive(Debug, Clone, PartialEq)]
pub enum QuestionModalOutcome {
    /// Modal stays open (navigation, or a no-op Esc).
    Stay,
    /// Modal closed but the question stays active for inline text answering.
    CloseKeepActive,
    /// An option was chosen; the loop routes it to the right channel.
    Answer {
        question_id: String,
        selection: QuestionSelection,
    },
}
