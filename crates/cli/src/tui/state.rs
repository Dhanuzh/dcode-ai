//! Transcript + status driven by `AgentEvent`.

use crate::tui::app::TuiCmd;
use crate::tui::branch_picker::{branch_picker_enter_command, filtered_branch_indices};
use crate::tui::connect_modal::{
    ConnectAction, build_connect_rows, clamp_selection, provider_at_selection,
    selectable_row_indices,
};
use crate::tui::palette::{
    PaletteRow, filter_palette_rows, palette_command_for_label, palette_selectable_indices,
};
use crate::{activity, tool_ui};
use dcode_ai_common::config::ProviderKind;
use dcode_ai_common::event::{
    AgentEvent, BusyState, InteractiveQuestionPayload, QuestionSelection,
};
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
    pub title: String,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct SessionPickerEntry {
    pub id: String,
    pub label: String,
    pub search_text: String,
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
    pub started: Instant,
    /// Start time per in-flight tool call_id, for duration badges on completion.
    tool_started: HashMap<String, Instant>,
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
    /// Selected action in approval popup: 0=approve, 1=always approve, 2=deny.
    pub approval_option_index: usize,
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
    /// Active mouse text-selection. `Some` while user is dragging or after
    /// release until input/scroll clears it.
    pub mouse_selection: Option<crate::tui::mouse_select::Selection>,
    /// History panel viewport rect captured each frame, used to translate
    /// raw mouse coordinates → buffer-space rows for selection.
    pub history_rect: Option<(u16, u16, u16, u16)>,
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

impl TuiSessionState {
    pub fn new(
        session_id: String,
        model: String,
        agent_profile: String,
        permission_mode: String,
        workspace_root: PathBuf,
        code_line_numbers: bool,
    ) -> Self {
        Self {
            blocks: Vec::new(),
            streaming_assistant: None,
            streaming_thinking: None,
            composer: crate::tui::composer::TextArea::default(),
            input_buffer: String::new(),
            cursor_char_idx: 0,
            scroll_lines: 0,
            transcript_follow_tail: true,
            session_id,
            workspace_root,
            workspace_display: String::new(),
            staged_image_attachments: Vec::new(),
            subagents: Vec::new(),
            model,
            agent_profile,
            permission_mode,
            process_title: "idle".to_string(),
            process_detail: String::new(),
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
            started: Instant::now(),
            tool_started: HashMap::new(),
            busy: false,
            current_busy_state: BusyState::Idle,
            busy_state_since: Instant::now(),
            should_exit: false,
            slash_menu_index: 0,
            command_palette_open: false,
            command_palette_query: String::new(),
            active_approval: None,
            approval_option_index: 0,
            active_question: None,
            answered_questions: HashMap::new(),
            thinking_expanded: false,
            current_branch: String::new(),
            branch_picker_open: false,
            branch_picker_query: String::new(),
            branch_picker_index: 0,
            branch_picker_branches: Vec::new(),
            branch_chip_bounds: None,
            sidebar_open: true,
            sidebar_toggle_bounds: None,
            provider_picker_open: false,
            provider_picker_index: 0,
            provider_picker_for_api_key: false,
            pending_api_key_provider: None,
            at_menu_index: 0,
            connect_modal_open: false,
            connect_search: String::new(),
            connect_menu_index: 0,
            connect_modal_scroll: 0,
            connect_modal_ignore_enter_once: false,
            api_key_modal_open: false,
            api_key_target_provider: None,
            api_key_input: String::new(),
            api_key_target_has_existing: false,
            api_key_connect_after_save: false,
            anthropic_oauth_modal_open: false,
            anthropic_oauth_url: String::new(),
            anthropic_oauth_code_verifier: String::new(),
            anthropic_oauth_code_input: String::new(),
            info_modal_open: false,
            info_modal_title: String::new(),
            info_modal_lines: Vec::new(),
            info_modal_scroll: 0,
            info_modal_hscroll: 0,
            info_modal_view_rows: 16,
            info_modal_view_cols: 80,
            model_picker_open: false,
            model_picker_search: String::new(),
            model_picker_index: 0,
            model_picker_entries: Vec::new(),
            model_picker_scroll: 0,
            leader_pending: false,
            permission_picker_open: false,
            permission_picker_index: 0,
            agent_picker_open: false,
            agent_picker_index: 0,
            question_modal_open: false,
            question_modal_index: 0,
            question_modal_scroll: 0,
            palette_index: 0,
            session_picker_open: false,
            session_picker_search: String::new(),
            session_picker_index: 0,
            session_picker_entries: Vec::new(),
            session_picker_scroll: 0,
            onboarding_mode: false,
            validation_status: None,
            queued_steering: 0,
            queued_followup: 0,
            queue_preview_items: Vec::new(),
            mcp_server_count: 0,
            code_line_numbers,
            transcript_rev: 0,
            paste_store: HashMap::new(),
            paste_counter: 0,
            mouse_capture_on: true,
            theme_picker_open: false,
            theme_picker_index: 0,
            theme_picker_entries: Vec::new(),
            pinned_notes: Vec::new(),
            pins_modal_open: false,
            pins_modal_index: 0,
            subagent_modal_open: false,
            subagent_modal_index: 0,
            transcript_search_open: false,
            transcript_search_query: String::new(),
            transcript_search_index: 0,
            composer_history_search_open: false,
            composer_history_search_query: String::new(),
            composer_history_search_index: 0,
            menu_content: crate::tui::tui_types::MenuContent::None,
            tool_block_collapsed: HashMap::new(),
            all_tools_collapsed: false,
            mouse_selection: None,
            history_rect: None,
        }
    }

    /// Returns true if the tool block with `call_id` should render header-only.
    /// Per-block override wins over the global toggle; default is collapsed for
    /// finished tools when global toggle on, otherwise expanded.
    pub fn is_tool_block_collapsed(&self, call_id: &str) -> bool {
        if let Some(v) = self.tool_block_collapsed.get(call_id) {
            return *v;
        }
        self.all_tools_collapsed
    }

    pub fn sync_legacy_from_composer(&mut self) {
        self.input_buffer = self.composer.text().to_string();
        self.cursor_char_idx = self.composer.cursor_char_idx();
    }

    pub fn set_input_text(&mut self, text: impl Into<String>) {
        self.composer.set_text(text.into());
        self.sync_legacy_from_composer();
    }

    pub fn set_input_text_with_cursor(&mut self, text: impl Into<String>, cursor_char_idx: usize) {
        self.composer
            .set_text_with_cursor(text.into(), cursor_char_idx);
        self.sync_legacy_from_composer();
    }

    /// Filtered history matches for the open reverse-search overlay:
    /// most-recent-first, substring of the current query, capped at 64.
    pub fn history_search_matches(&self, history: &[String]) -> Vec<String> {
        let needle = self.composer_history_search_query.to_ascii_lowercase();
        history
            .iter()
            .rev()
            .filter(|entry| needle.is_empty() || entry.to_ascii_lowercase().contains(&needle))
            .take(64)
            .cloned()
            .collect()
    }

    fn close_history_search(&mut self) {
        self.composer_history_search_open = false;
        self.composer_history_search_query.clear();
        self.composer_history_search_index = 0;
    }

    /// Apply one key to the open history-search overlay, returning `true` (the
    /// key is always consumed while the overlay is open). The crossterm→key
    /// mapping stays in the event loop; the state machine lives here so it can
    /// be unit-tested without a terminal.
    pub fn apply_history_search_key(&mut self, key: HistorySearchKey, history: &[String]) -> bool {
        let matches = self.history_search_matches(history);
        match key {
            HistorySearchKey::Cancel => self.close_history_search(),
            HistorySearchKey::Backspace => {
                self.composer_history_search_query.pop();
                self.composer_history_search_index = 0;
            }
            HistorySearchKey::Up => {
                if !matches.is_empty() {
                    self.composer_history_search_index =
                        self.composer_history_search_index.saturating_sub(1);
                }
            }
            HistorySearchKey::Down => {
                if !matches.is_empty() {
                    self.composer_history_search_index = (self.composer_history_search_index + 1)
                        .min(matches.len().saturating_sub(1));
                }
            }
            HistorySearchKey::Accept => {
                if !matches.is_empty() {
                    let pick = self
                        .composer_history_search_index
                        .min(matches.len().saturating_sub(1));
                    if let Some(entry) = matches.get(pick) {
                        self.set_input_text(entry.clone());
                    }
                }
                self.close_history_search();
            }
            HistorySearchKey::Char(c) => {
                self.composer_history_search_query.push(c);
                self.composer_history_search_index = 0;
            }
        }
        true
    }

    /// Apply one key to the open branch-picker overlay. Returns a [`TuiCmd`] to
    /// dispatch (switch/create) when the selection is accepted, else `None`.
    /// As with history search, the crossterm mapping stays in the event loop so
    /// these transitions are unit-testable without a terminal.
    pub fn apply_branch_picker_key(&mut self, key: BranchPickerKey) -> Option<TuiCmd> {
        match key {
            BranchPickerKey::Cancel => self.close_branch_picker(),
            BranchPickerKey::Up => {
                if !filtered_branch_indices(&self.branch_picker_branches, &self.branch_picker_query)
                    .is_empty()
                {
                    self.branch_picker_index = self.branch_picker_index.saturating_sub(1);
                }
            }
            BranchPickerKey::Down => {
                let n = filtered_branch_indices(
                    &self.branch_picker_branches,
                    &self.branch_picker_query,
                )
                .len();
                if n > 0 {
                    self.branch_picker_index = (self.branch_picker_index + 1).min(n - 1);
                }
            }
            BranchPickerKey::Accept => {
                let cmd = branch_picker_enter_command(
                    &self.branch_picker_branches,
                    &self.branch_picker_query,
                    self.branch_picker_index,
                );
                self.close_branch_picker();
                return cmd;
            }
            BranchPickerKey::Backspace => {
                self.branch_picker_query.pop();
                let filtered = filtered_branch_indices(
                    &self.branch_picker_branches,
                    &self.branch_picker_query,
                );
                self.branch_picker_index = self
                    .branch_picker_index
                    .min(filtered.len().saturating_sub(1));
            }
            BranchPickerKey::Char(c) => {
                self.branch_picker_query.push(c);
                self.branch_picker_index = 0;
            }
        }
        None
    }

    /// Apply one key to the open interactive-question modal. Navigation/cancel
    /// mutate state in place; an accepted option is returned for the loop to
    /// route to the right answer channel.
    pub fn apply_question_modal_key(&mut self, key: QuestionModalKey) -> QuestionModalOutcome {
        let Some(q) = self.active_question.clone() else {
            return QuestionModalOutcome::Stay;
        };
        // Items: suggested (1) + options + optional "chat about this" (allow_custom).
        let total = 1 + q.options.len() + if q.allow_custom { 1 } else { 0 };
        match key {
            QuestionModalKey::Cancel => {
                if q.allow_custom {
                    // Fall back to inline text input; keep the question active.
                    self.close_question_modal();
                    QuestionModalOutcome::CloseKeepActive
                } else {
                    QuestionModalOutcome::Stay
                }
            }
            QuestionModalKey::Up => {
                self.question_modal_index = self.question_modal_index.saturating_sub(1);
                QuestionModalOutcome::Stay
            }
            QuestionModalKey::Down => {
                self.question_modal_index = (self.question_modal_index + 1).min(total - 1);
                QuestionModalOutcome::Stay
            }
            QuestionModalKey::Accept => {
                let idx = self.question_modal_index;
                let selection = if idx == 0 {
                    Some(QuestionSelection::Suggested)
                } else if idx <= q.options.len() {
                    Some(QuestionSelection::Option {
                        option_id: q.options[idx - 1].id.clone(),
                    })
                } else {
                    None // "Chat about this" → inline text input
                };
                self.close_question_modal();
                match selection {
                    Some(selection) => QuestionModalOutcome::Answer {
                        question_id: q.question_id.clone(),
                        selection,
                    },
                    None => QuestionModalOutcome::CloseKeepActive,
                }
            }
        }
    }

    fn close_command_palette(&mut self) {
        self.command_palette_open = false;
        self.command_palette_query.clear();
        self.palette_index = 0;
    }

    fn clamp_palette_index(&mut self) {
        let filtered = filter_palette_rows(&self.command_palette_query);
        let selectable = palette_selectable_indices(&filtered);
        self.palette_index = self.palette_index.min(selectable.len().saturating_sub(1));
    }

    /// Apply one key to the open model picker. On Accept, returns the chosen
    /// [`ModelPickerAction`] for the loop to route; nav/search mutate in place.
    pub fn apply_model_picker_key(&mut self, key: ModelPickerKey) -> Option<ModelPickerAction> {
        let filter = self.model_picker_search.to_ascii_lowercase();
        let is_match = |e: &ModelPickerEntry| {
            !e.is_header
                && (filter.is_empty()
                    || e.label.to_ascii_lowercase().contains(&filter)
                    || e.detail.to_ascii_lowercase().contains(&filter))
        };
        let selectable_count = self
            .model_picker_entries
            .iter()
            .filter(|e| is_match(e))
            .count();
        match key {
            ModelPickerKey::Cancel => self.close_model_picker(),
            ModelPickerKey::Up => {
                if selectable_count > 0 {
                    self.model_picker_index = self
                        .model_picker_index
                        .saturating_sub(1)
                        .min(selectable_count - 1);
                }
            }
            ModelPickerKey::Down => {
                if selectable_count > 0 {
                    self.model_picker_index =
                        (self.model_picker_index + 1).min(selectable_count - 1);
                }
            }
            ModelPickerKey::Accept => {
                let action = {
                    let selectable: Vec<&ModelPickerEntry> = self
                        .model_picker_entries
                        .iter()
                        .filter(|e| is_match(e))
                        .collect();
                    let pick = self
                        .model_picker_index
                        .min(selectable.len().saturating_sub(1));
                    selectable.get(pick).map(|e| e.action.clone())
                };
                if let Some(action) = action {
                    self.close_model_picker();
                    return Some(action);
                }
            }
            ModelPickerKey::Backspace => {
                self.model_picker_search.pop();
                self.model_picker_index = 0;
                self.model_picker_scroll = 0;
            }
            ModelPickerKey::Char(c) => {
                self.model_picker_search.push(c);
                self.model_picker_index = 0;
                self.model_picker_scroll = 0;
            }
        }
        None
    }

    /// Apply one key to the open connect-provider modal. On Accept, returns the
    /// selected [`ConnectAction`] for the loop to route (it needs `AuthStore`
    /// I/O); nav/search and the "ignore the opening Enter" guard mutate in place.
    pub fn apply_connect_modal_key(&mut self, key: ConnectModalKey) -> Option<ConnectAction> {
        let rows = build_connect_rows(&self.connect_search);
        let n_sel = selectable_row_indices(&rows).len();
        match key {
            ConnectModalKey::Cancel => {
                if !self.onboarding_mode {
                    self.close_connect_modal();
                }
            }
            ConnectModalKey::Up => {
                self.connect_modal_ignore_enter_once = false;
                if n_sel > 0 {
                    self.connect_menu_index =
                        self.connect_menu_index.saturating_sub(1).min(n_sel - 1);
                }
            }
            ConnectModalKey::Down => {
                self.connect_modal_ignore_enter_once = false;
                if n_sel > 0 {
                    self.connect_menu_index = (self.connect_menu_index + 1).min(n_sel - 1);
                }
            }
            ConnectModalKey::Accept => {
                if self.connect_modal_ignore_enter_once {
                    // Swallow the same Enter that opened the modal.
                    self.connect_modal_ignore_enter_once = false;
                    return None;
                }
                if let Some((_p, _title, action)) =
                    provider_at_selection(&rows, self.connect_menu_index)
                {
                    self.close_connect_modal();
                    return Some(action);
                }
            }
            ConnectModalKey::Backspace => {
                self.connect_modal_ignore_enter_once = false;
                self.connect_search.pop();
                self.connect_menu_index = 0;
                self.connect_modal_scroll = 0;
                let rows2 = build_connect_rows(&self.connect_search);
                self.connect_menu_index = clamp_selection(self.connect_menu_index, &rows2);
            }
            ConnectModalKey::Char(c) => {
                self.connect_modal_ignore_enter_once = false;
                self.connect_search.push(c);
                self.connect_menu_index = 0;
                self.connect_modal_scroll = 0;
                let rows2 = build_connect_rows(&self.connect_search);
                self.connect_menu_index = clamp_selection(self.connect_menu_index, &rows2);
            }
        }
        None
    }

    /// Apply one key to the open pinned-notes modal. Returns `true` if the user
    /// requested a copy of the selected note — the loop performs the clipboard
    /// write and status message (the only side effect this overlay needs).
    pub fn apply_pins_modal_key(&mut self, key: PinsModalKey) -> bool {
        match key {
            PinsModalKey::Close => self.close_pins_modal(),
            PinsModalKey::Up => {
                self.pins_modal_index = self.pins_modal_index.saturating_sub(1);
            }
            PinsModalKey::Down => {
                if !self.pinned_notes.is_empty() {
                    self.pins_modal_index =
                        (self.pins_modal_index + 1).min(self.pinned_notes.len().saturating_sub(1));
                }
            }
            PinsModalKey::Delete => {
                let idx = self.pins_modal_index;
                if idx < self.pinned_notes.len() {
                    self.pinned_notes.remove(idx);
                    self.pins_modal_index = self
                        .pins_modal_index
                        .min(self.pinned_notes.len().saturating_sub(1));
                }
                if self.pinned_notes.is_empty() {
                    self.close_pins_modal();
                }
            }
            PinsModalKey::Accept => {
                self.scroll_lines = 0;
                self.transcript_follow_tail = false;
                self.close_pins_modal();
            }
            PinsModalKey::Copy => return true,
        }
        false
    }

    /// Apply one key to the open default-provider picker. On Accept, returns the
    /// chosen provider + intent for the loop to route; nav mutates in place.
    pub fn apply_provider_picker_key(
        &mut self,
        key: ProviderPickerKey,
    ) -> Option<ProviderPickerOutcome> {
        let n = ProviderKind::ALL.len();
        match key {
            ProviderPickerKey::Cancel => self.close_provider_picker(),
            ProviderPickerKey::Up => {
                self.provider_picker_index = self.provider_picker_index.saturating_sub(1);
            }
            ProviderPickerKey::Down => {
                if n > 0 {
                    self.provider_picker_index = (self.provider_picker_index + 1) % n;
                }
            }
            ProviderPickerKey::Accept => {
                if n == 0 {
                    self.close_provider_picker();
                    return None;
                }
                let p = ProviderKind::ALL[self.provider_picker_index.min(n - 1)];
                let for_key = self.provider_picker_for_api_key;
                self.close_provider_picker();
                return Some(if for_key {
                    ProviderPickerOutcome::ForApiKey(p)
                } else {
                    ProviderPickerOutcome::Apply(p)
                });
            }
        }
        None
    }

    /// Apply one key to the open session picker. On Accept, returns the session
    /// id to resume; nav/search mutate in place.
    pub fn apply_session_picker_key(&mut self, key: SessionPickerKey) -> Option<String> {
        let filter = self.session_picker_search.to_ascii_lowercase();
        let is_match = |e: &SessionPickerEntry| {
            filter.is_empty() || e.search_text.to_ascii_lowercase().contains(&filter)
        };
        let count = self
            .session_picker_entries
            .iter()
            .filter(|e| is_match(e))
            .count();
        match key {
            SessionPickerKey::Cancel => self.close_session_picker(),
            SessionPickerKey::Up => {
                self.session_picker_index = self.session_picker_index.saturating_sub(1);
            }
            SessionPickerKey::Down => {
                if count > 0 {
                    self.session_picker_index =
                        (self.session_picker_index + 1).min(count.saturating_sub(1));
                }
            }
            SessionPickerKey::Accept => {
                let id = {
                    let filtered: Vec<&SessionPickerEntry> = self
                        .session_picker_entries
                        .iter()
                        .filter(|e| is_match(e))
                        .collect();
                    let pick = self
                        .session_picker_index
                        .min(filtered.len().saturating_sub(1));
                    filtered.get(pick).map(|e| e.id.clone())
                };
                if let Some(id) = id {
                    self.close_session_picker();
                    return Some(id);
                }
            }
            SessionPickerKey::Backspace => {
                self.session_picker_search.pop();
                self.session_picker_index = 0;
                self.session_picker_scroll = 0;
            }
            SessionPickerKey::Char(c) => {
                self.session_picker_search.push(c);
                self.session_picker_index = 0;
                self.session_picker_scroll = 0;
            }
        }
        None
    }

    /// Apply one key to the open read-only info modal (scroll/close only).
    pub fn apply_info_modal_key(&mut self, key: InfoModalKey) {
        match key {
            InfoModalKey::Close => self.close_info_modal(),
            InfoModalKey::ScrollUp => {
                self.info_modal_scroll = self.info_modal_scroll.saturating_sub(1);
            }
            InfoModalKey::ScrollDown => {
                let max_vis = self.info_modal_view_rows.max(1);
                let max_scroll = self.info_modal_lines.len().saturating_sub(max_vis);
                self.info_modal_scroll = (self.info_modal_scroll + 1).min(max_scroll);
            }
            InfoModalKey::ScrollLeft => {
                self.info_modal_hscroll = self.info_modal_hscroll.saturating_sub(4);
            }
            InfoModalKey::ScrollRight => {
                self.info_modal_hscroll = self.info_modal_hscroll.saturating_add(4);
            }
            InfoModalKey::Home => {
                self.info_modal_scroll = 0;
                self.info_modal_hscroll = 0;
            }
            InfoModalKey::End => {
                let max_vis = self.info_modal_view_rows.max(1);
                self.info_modal_scroll = self.info_modal_lines.len().saturating_sub(max_vis);
                let max_line_chars = self
                    .info_modal_lines
                    .iter()
                    .map(|line| line.chars().count())
                    .max()
                    .unwrap_or(0);
                self.info_modal_hscroll =
                    max_line_chars.saturating_sub(self.info_modal_view_cols.max(1));
            }
        }
    }

    /// Apply one key to the open command palette. On Accept, the selected
    /// command's slash text is loaded into the composer. All effects are on
    /// state, so this is unit-testable without a terminal.
    pub fn apply_command_palette_key(&mut self, key: CommandPaletteKey) {
        match key {
            CommandPaletteKey::Cancel => self.close_command_palette(),
            CommandPaletteKey::Up => {
                if self.palette_index > 0 {
                    self.palette_index -= 1;
                }
            }
            CommandPaletteKey::Down => {
                let filtered = filter_palette_rows(&self.command_palette_query);
                let selectable = palette_selectable_indices(&filtered);
                if !selectable.is_empty() {
                    self.palette_index =
                        (self.palette_index + 1).min(selectable.len().saturating_sub(1));
                }
            }
            CommandPaletteKey::Accept => {
                let filtered = filter_palette_rows(&self.command_palette_query);
                let selectable = palette_selectable_indices(&filtered);
                let pick = self.palette_index.min(selectable.len().saturating_sub(1));
                if let Some(&abs_idx) = selectable.get(pick)
                    && let PaletteRow::Entry { label, .. } = filtered[abs_idx]
                {
                    let cmd = palette_command_for_label(label);
                    self.set_input_text(cmd.to_string());
                }
                self.close_command_palette();
            }
            CommandPaletteKey::Backspace => {
                self.command_palette_query.pop();
                self.clamp_palette_index();
            }
            CommandPaletteKey::Char(c) => {
                self.command_palette_query.push(c);
                self.clamp_palette_index();
            }
        }
    }

    pub fn clear_input(&mut self) {
        self.composer.clear();
        self.sync_legacy_from_composer();
    }

    pub fn take_input_text(&mut self) -> String {
        let out = self.composer.take_text();
        self.sync_legacy_from_composer();
        out
    }

    pub fn insert_input_char(&mut self, ch: char) {
        self.composer.insert_char(ch);
        self.sync_legacy_from_composer();
    }

    pub fn insert_input_str(&mut self, s: &str) {
        self.composer.insert_str(s);
        self.sync_legacy_from_composer();
    }

    pub fn move_input_left(&mut self) {
        self.composer.move_left();
        self.sync_legacy_from_composer();
    }

    pub fn move_input_right(&mut self) {
        self.composer.move_right();
        self.sync_legacy_from_composer();
    }

    pub fn move_input_home(&mut self) {
        self.composer.move_home();
        self.sync_legacy_from_composer();
    }

    pub fn move_input_end(&mut self) {
        self.composer.move_end();
        self.sync_legacy_from_composer();
    }

    pub fn backspace_input(&mut self) {
        self.composer.backspace();
        self.sync_legacy_from_composer();
    }

    pub fn delete_input(&mut self) {
        self.composer.delete();
        self.sync_legacy_from_composer();
    }

    /// Toggle the most recent tool block (ToolRunning or ToolDone) by call_id.
    /// Returns true if a block was toggled.
    pub fn toggle_last_tool_block(&mut self) -> bool {
        let last_id = self.blocks.iter().rev().find_map(|b| match b {
            DisplayBlock::ToolRunning { call_id, .. } => Some(call_id.clone()),
            DisplayBlock::ToolDone { call_id, .. } => Some(call_id.clone()),
            _ => None,
        });
        if let Some(id) = last_id {
            let cur = self.is_tool_block_collapsed(&id);
            self.tool_block_collapsed.insert(id, !cur);
            self.transcript_rev = self.transcript_rev.wrapping_add(1);
            true
        } else {
            false
        }
    }

    /// Toggle the global all-tools-collapsed flag and clear per-block overrides
    /// so the global state takes effect uniformly.
    pub fn toggle_all_tool_blocks(&mut self) {
        self.all_tools_collapsed = !self.all_tools_collapsed;
        self.tool_block_collapsed.clear();
        self.transcript_rev = self.transcript_rev.wrapping_add(1);
    }

    pub fn open_theme_picker(&mut self, entries: Vec<String>, current_index: usize) {
        self.theme_picker_open = true;
        self.theme_picker_entries = entries;
        self.theme_picker_index = current_index;
    }

    pub fn close_theme_picker(&mut self) {
        self.theme_picker_open = false;
        self.theme_picker_index = 0;
    }

    pub fn open_pins_modal(&mut self) {
        self.pins_modal_open = true;
        self.pins_modal_index = self
            .pins_modal_index
            .min(self.pinned_notes.len().saturating_sub(1));
    }

    pub fn close_pins_modal(&mut self) {
        self.pins_modal_open = false;
        self.pins_modal_index = 0;
    }

    pub fn open_subagent_modal(&mut self) {
        self.subagent_modal_open = true;
        self.subagent_modal_index = self
            .subagent_modal_index
            .min(self.subagents.len().saturating_sub(1));
    }

    pub fn close_subagent_modal(&mut self) {
        self.subagent_modal_open = false;
        self.subagent_modal_index = 0;
    }

    pub fn open_transcript_search(&mut self) {
        self.transcript_search_open = true;
        self.transcript_search_index = 0;
    }

    pub fn close_transcript_search(&mut self) {
        self.transcript_search_open = false;
        self.transcript_search_index = 0;
    }

    pub fn open_connect_modal(&mut self) {
        self.connect_modal_open = true;
        self.connect_search.clear();
        self.connect_menu_index = 0;
        self.connect_modal_scroll = 0;
        self.connect_modal_ignore_enter_once = false;
    }

    pub fn close_connect_modal(&mut self) {
        self.connect_modal_open = false;
        self.connect_search.clear();
        self.connect_menu_index = 0;
        self.connect_modal_scroll = 0;
        self.connect_modal_ignore_enter_once = false;
    }

    pub fn open_api_key_modal(
        &mut self,
        provider: ProviderKind,
        has_existing: bool,
        connect_after_save: bool,
    ) {
        self.api_key_modal_open = true;
        self.api_key_target_provider = Some(provider);
        self.api_key_input.clear();
        self.api_key_target_has_existing = has_existing;
        self.api_key_connect_after_save = connect_after_save;
        self.validation_status = None;
    }

    pub fn close_api_key_modal(&mut self) {
        self.api_key_modal_open = false;
        self.api_key_target_provider = None;
        self.api_key_input.clear();
        self.api_key_target_has_existing = false;
        self.api_key_connect_after_save = false;
        self.validation_status = None;
    }

    pub fn open_anthropic_oauth_modal(&mut self, url: String, code_verifier: String) {
        self.anthropic_oauth_modal_open = true;
        self.anthropic_oauth_url = url;
        self.anthropic_oauth_code_verifier = code_verifier;
        self.anthropic_oauth_code_input.clear();
    }

    pub fn close_anthropic_oauth_modal(&mut self) {
        self.anthropic_oauth_modal_open = false;
        self.anthropic_oauth_url.clear();
        self.anthropic_oauth_code_verifier.clear();
        self.anthropic_oauth_code_input.clear();
    }

    pub fn open_info_modal(&mut self, title: impl Into<String>, lines: Vec<String>) {
        self.info_modal_open = true;
        self.info_modal_title = title.into();
        self.info_modal_lines = lines;
        self.info_modal_scroll = 0;
        self.info_modal_hscroll = 0;
        self.info_modal_view_rows = 16;
        self.info_modal_view_cols = 80;
    }

    pub fn close_info_modal(&mut self) {
        self.info_modal_open = false;
        self.info_modal_title.clear();
        self.info_modal_lines.clear();
        self.info_modal_scroll = 0;
        self.info_modal_hscroll = 0;
        self.info_modal_view_rows = 16;
        self.info_modal_view_cols = 80;
    }

    pub fn open_model_picker(&mut self, entries: Vec<ModelPickerEntry>) {
        self.model_picker_open = true;
        self.model_picker_search.clear();
        self.model_picker_index = 0;
        self.model_picker_scroll = 0;
        self.model_picker_entries = entries;
    }

    pub fn close_model_picker(&mut self) {
        self.model_picker_open = false;
        self.model_picker_search.clear();
        self.model_picker_index = 0;
        self.model_picker_scroll = 0;
        self.model_picker_entries.clear();
    }

    pub fn open_permission_picker(&mut self, current_index: usize) {
        self.permission_picker_open = true;
        self.permission_picker_index = current_index;
    }

    pub fn close_permission_picker(&mut self) {
        self.permission_picker_open = false;
        self.permission_picker_index = 0;
    }

    pub fn open_agent_picker(&mut self, current_index: usize) {
        self.agent_picker_open = true;
        self.agent_picker_index = current_index;
    }

    pub fn close_agent_picker(&mut self) {
        self.agent_picker_open = false;
        self.agent_picker_index = 0;
    }

    pub fn open_question_modal(&mut self) {
        self.question_modal_open = true;
        self.question_modal_index = 0;
        self.question_modal_scroll = 0;
        self.touch_transcript();
    }

    pub fn close_question_modal(&mut self) {
        self.question_modal_open = false;
        self.question_modal_index = 0;
        self.question_modal_scroll = 0;
        self.touch_transcript();
    }

    pub fn open_session_picker(&mut self, entries: Vec<SessionPickerEntry>, current: &str) {
        self.session_picker_open = true;
        self.session_picker_search.clear();
        self.session_picker_index = entries.iter().position(|e| e.id == current).unwrap_or(0);
        self.session_picker_entries = entries;
        self.session_picker_scroll = 0;
    }

    pub fn close_session_picker(&mut self) {
        self.session_picker_open = false;
        self.session_picker_search.clear();
        self.session_picker_index = 0;
        self.session_picker_entries.clear();
        self.session_picker_scroll = 0;
    }

    pub fn open_provider_picker(&mut self, current: ProviderKind, for_api_key: bool) {
        self.provider_picker_open = true;
        self.provider_picker_for_api_key = for_api_key;
        self.provider_picker_index = ProviderKind::ALL
            .iter()
            .position(|p| *p == current)
            .unwrap_or(0);
    }

    pub fn close_provider_picker(&mut self) {
        self.provider_picker_open = false;
        self.provider_picker_for_api_key = false;
    }

    pub fn set_busy(&mut self, busy: bool) {
        self.busy = busy;
    }

    pub fn set_busy_state(&mut self, state: BusyState) {
        if self.current_busy_state != state {
            self.current_busy_state = state;
            self.busy_state_since = Instant::now();
        }
    }

    pub fn push_error(&mut self, msg: String) {
        self.blocks.push(DisplayBlock::ErrorLine(msg));
        self.touch_transcript();
    }

    pub fn touch_transcript(&mut self) {
        self.transcript_rev = self.transcript_rev.wrapping_add(1);
    }

    /// Approval/question prompts from replayed history are transcript only.
    /// The live pending channels are not restored on resume, so these must not
    /// keep the input box in approval/answer mode.
    pub fn clear_replayed_interaction_state(&mut self) {
        self.active_approval = None;
        self.approval_option_index = 0;
        self.active_question = None;
        self.close_question_modal();
    }

    pub fn clear_active_approval_if_matches(&mut self, call_id: &str) {
        let mut cleared = false;
        if self
            .active_approval
            .as_ref()
            .is_some_and(|req| req.call_id == call_id)
        {
            self.active_approval = None;
            self.approval_option_index = 0;
            cleared = true;
        }
        let before = self.blocks.len();
        self.blocks
            .retain(|b| !matches!(b, DisplayBlock::ApprovalPending(req) if req.call_id == call_id));
        if cleared || self.blocks.len() != before {
            self.touch_transcript();
        }
    }

    pub fn set_agent_profile(&mut self, label: &str) {
        self.agent_profile = label.to_string();
    }

    pub fn set_current_branch(&mut self, branch: &str) {
        self.current_branch = branch.to_string();
    }

    pub fn open_branch_picker(&mut self, branches: Vec<String>, current: &str) {
        self.branch_picker_branches = branches;
        self.branch_picker_query.clear();
        self.branch_picker_index = self
            .branch_picker_branches
            .iter()
            .position(|b| b == current)
            .unwrap_or(0);
        self.branch_picker_open = true;
    }

    pub fn close_branch_picker(&mut self) {
        self.branch_picker_open = false;
        self.branch_picker_query.clear();
        self.branch_picker_branches.clear();
        self.branch_picker_index = 0;
    }

    pub fn set_permission_mode(&mut self, mode: &str) {
        self.permission_mode = mode.to_string();
    }

    fn set_process(&mut self, title: impl Into<String>, detail: impl Into<String>) {
        self.process_title = title.into();
        self.process_detail = detail.into();
    }

    fn flush_stream_before_tool(&mut self) {
        if let Some(t) = self.streaming_thinking.take()
            && !t.trim().is_empty()
        {
            self.blocks.push(DisplayBlock::Thinking(t));
        }
        if let Some(s) = self.streaming_assistant.take()
            && !s.trim().is_empty()
        {
            self.blocks.push(DisplayBlock::Assistant(s));
        }
    }

    pub fn apply_event(&mut self, e: &AgentEvent) {
        let mut transcript_dirty = false;
        match e {
            AgentEvent::SessionStarted {
                session_id,
                model,
                workspace,
            } => {
                self.session_id = session_id.clone();
                self.model = model.clone();
                self.workspace_root = workspace.clone();
                self.workspace_display = workspace.display().to_string();
                self.set_process("session ready", model.clone());
            }
            AgentEvent::MessageReceived { role, content } => {
                if role == "user" {
                    self.streaming_assistant = None;
                    self.streaming_thinking = None;
                    self.blocks.push(DisplayBlock::User(content.clone()));
                    self.set_process("queued prompt", truncate(content, 96));
                    self.set_busy_state(BusyState::Thinking);
                    transcript_dirty = true;
                } else if role == "assistant" {
                    self.streaming_assistant = None;
                    if let Some(t) = self.streaming_thinking.take()
                        && !t.trim().is_empty()
                    {
                        self.blocks.push(DisplayBlock::Thinking(t));
                    }
                    self.blocks.push(DisplayBlock::Assistant(content.clone()));
                    self.set_process("response ready", truncate(content, 96));
                    self.set_busy_state(BusyState::Idle);
                    transcript_dirty = true;
                }
            }
            AgentEvent::ThinkingDelta { delta } => {
                self.streaming_thinking
                    .get_or_insert_with(String::new)
                    .push_str(delta);
                self.set_process("thinking", "analyzing and planning");
                transcript_dirty = true;
            }
            AgentEvent::TokensStreamed { delta } => {
                self.streaming_assistant
                    .get_or_insert_with(String::new)
                    .push_str(delta);
                self.set_process("writing response", truncate(delta, 96));
                self.set_busy_state(BusyState::Streaming);
                transcript_dirty = true;
            }
            AgentEvent::ToolCallStarted {
                call_id,
                tool,
                input,
            } => {
                self.flush_stream_before_tool();
                self.tool_started.insert(call_id.clone(), Instant::now());
                self.blocks.push(DisplayBlock::ToolRunning {
                    name: tool.clone(),
                    call_id: call_id.clone(),
                    input: tool_ui::format_input_for_display(tool, input),
                });
                if let Some(message) = activity::started(tool, input) {
                    self.blocks.push(DisplayBlock::System(message));
                }
                let ui = tool_ui::metadata(tool);
                let preview = tool_ui::preview_from_value(tool, input);
                self.set_process(
                    format!("running {}", ui.label.to_ascii_lowercase()),
                    if preview.is_empty() {
                        ui.family.to_string()
                    } else {
                        truncate(&preview, 96)
                    },
                );
                self.set_busy_state(BusyState::ToolRunning);
                transcript_dirty = true;
            }
            AgentEvent::ToolCallCompleted { call_id, output } => {
                let ok = output.success;
                let duration_ms = self
                    .tool_started
                    .remove(call_id)
                    .map(|t| t.elapsed().as_millis() as u64);
                self.active_approval = self
                    .active_approval
                    .take()
                    .filter(|req| req.call_id != *call_id);
                self.set_busy_state(BusyState::Thinking);
                let detail = if ok {
                    output.output.clone()
                } else {
                    output.error.clone().unwrap_or_else(|| "failed".into())
                };
                if let Some(idx) = self.blocks.iter().rposition(
                    |b| {
                        matches!(b, DisplayBlock::ToolRunning { call_id: id, .. } if id == call_id)
                            || matches!(b, DisplayBlock::ApprovalPending(req) if req.call_id == *call_id)
                    },
                ) {
                    let name = match &self.blocks[idx] {
                        DisplayBlock::ToolRunning { name, .. } => name.clone(),
                        DisplayBlock::ApprovalPending(req) => req.tool.clone(),
                        _ => "?".into(),
                    };
                    if let Some(message) = activity::completed(
                        &name,
                        ok,
                        &output.output,
                        Some(self.workspace_root.as_path()),
                    ) {
                        self.blocks.push(DisplayBlock::System(message));
                    }
                    self.blocks[idx] = DisplayBlock::ToolDone {
                        name,
                        call_id: call_id.clone(),
                        ok,
                        detail,
                        duration_ms,
                    };
                } else {
                    self.blocks.push(DisplayBlock::ToolDone {
                        name: "?".into(),
                        call_id: call_id.clone(),
                        ok,
                        detail,
                        duration_ms,
                    });
                }
                self.set_process(
                    if ok { "tool completed" } else { "tool failed" },
                    truncate(
                        if ok {
                            &output.output
                        } else {
                            output.error.as_deref().unwrap_or("failed")
                        },
                        96,
                    ),
                );
                transcript_dirty = true;
            }
            AgentEvent::ApprovalRequested {
                call_id,
                tool,
                description,
            } => {
                // Idempotency: if we receive duplicate approval events for the same call,
                // keep only one pending prompt row in transcript state.
                self.blocks.retain(
                    |b| !matches!(b, DisplayBlock::ApprovalPending(req) if req.call_id == *call_id),
                );
                let input = self
                    .blocks
                    .iter()
                    .rev()
                    .find_map(|block| match block {
                        DisplayBlock::ToolRunning {
                            call_id: id, input, ..
                        } if id == call_id => Some(input.clone()),
                        _ => None,
                    })
                    .unwrap_or_else(|| "{}".into());
                let req = ApprovalRequest {
                    call_id: call_id.clone(),
                    tool: tool.clone(),
                    description: description.clone(),
                    input,
                };
                self.active_approval = Some(req.clone());
                self.approval_option_index = 0;
                self.set_busy_state(BusyState::ApprovalPending);
                if let Some(idx) = self.blocks.iter().rposition(
                    |b| matches!(b, DisplayBlock::ToolRunning { call_id: id, .. } if id == call_id),
                ) {
                    self.blocks[idx] = DisplayBlock::ApprovalPending(req);
                } else {
                    self.blocks.push(DisplayBlock::ApprovalPending(req));
                }
                self.set_process("waiting approval", truncate(description, 96));
                transcript_dirty = true;
            }
            AgentEvent::ApprovalResolved { call_id, approved } => {
                let tool = self
                    .active_approval
                    .as_ref()
                    .filter(|req| req.call_id == *call_id)
                    .map(|req| req.tool.clone())
                    .or_else(|| {
                        self.blocks.iter().rev().find_map(|block| match block {
                            DisplayBlock::ApprovalPending(req) if req.call_id == *call_id => {
                                Some(req.tool.clone())
                            }
                            _ => None,
                        })
                    })
                    .unwrap_or_else(|| "tool".into());
                self.active_approval = self
                    .active_approval
                    .take()
                    .filter(|req| req.call_id != *call_id);
                self.approval_option_index = 0;
                self.blocks.retain(
                    |b| !matches!(b, DisplayBlock::ApprovalPending(req) if req.call_id == *call_id),
                );
                self.blocks.push(DisplayBlock::ApprovalResolved {
                    tool,
                    approved: *approved,
                });
                self.set_process(
                    if *approved {
                        "approval granted"
                    } else {
                        "approval denied"
                    },
                    "",
                );
                transcript_dirty = true;
            }
            AgentEvent::QuestionRequested { question } => {
                self.active_question = Some(question.clone());
                self.blocks.push(DisplayBlock::Question(question.clone()));
                // Bring the prompt into view when follow-tail is on (default).
                self.transcript_follow_tail = true;
                self.open_question_modal();
                self.set_process("waiting input", truncate(&question.prompt, 96));
                transcript_dirty = true;
            }
            AgentEvent::QuestionResolved {
                question_id,
                selection,
            } => {
                self.answered_questions
                    .insert(question_id.clone(), selection.clone());
                self.active_question = None;
                self.close_question_modal();
                self.blocks.push(DisplayBlock::System(format!(
                    "Answered question {question_id}: {selection:?}"
                )));
                self.set_process("input received", format!("{selection:?}"));
                transcript_dirty = true;
            }
            AgentEvent::CostUpdated {
                input_tokens,
                output_tokens,
                estimated_cost_usd,
            } => {
                self.input_tokens = *input_tokens;
                self.output_tokens = *output_tokens;
                self.cost_usd = *estimated_cost_usd;
            }
            AgentEvent::SessionEnded { .. } => {
                self.set_process("session ended", "");
            }
            AgentEvent::Error { message } => {
                self.blocks.push(DisplayBlock::ErrorLine(message.clone()));
                self.set_process("error", truncate(message, 96));
                if message.to_ascii_lowercase().contains("run cancelled") {
                    self.set_busy_state(BusyState::Idle);
                } else {
                    self.set_busy_state(BusyState::Error);
                }
                transcript_dirty = true;
            }
            AgentEvent::Checkpoint { .. } => {}
            AgentEvent::ChildSessionSpawned {
                child_session_id,
                task,
                ..
            } => {
                let short = short_session_prefix(child_session_id);
                let task_s = truncate(task, 200);
                if let Some(row) = self
                    .subagents
                    .iter_mut()
                    .find(|r| r.id == *child_session_id)
                {
                    row.task = task_s.clone();
                    row.running = true;
                } else {
                    self.subagents.push(SubagentRow {
                        id: child_session_id.clone(),
                        task: task_s.clone(),
                        phase: String::new(),
                        detail: String::new(),
                        running: true,
                        skill: None,
                    });
                }
                self.blocks.push(DisplayBlock::System(format!(
                    "Sub-agent {short}… — {}",
                    truncate(task, 80)
                )));
                transcript_dirty = true;
            }
            AgentEvent::ChildSessionActivity {
                child_session_id,
                phase,
                detail,
            } => {
                let short = short_session_prefix(child_session_id);
                let d = truncate(detail, 120);
                if let Some(row) = self
                    .subagents
                    .iter_mut()
                    .find(|r| r.id == *child_session_id)
                {
                    row.phase = phase.clone();
                    row.detail = d.clone();
                    row.running = true;
                    if phase == "skill" || phase == "invoke_skill" {
                        row.skill = Some(detail.clone());
                    }
                } else {
                    self.subagents.push(SubagentRow {
                        id: child_session_id.clone(),
                        task: "(sub-agent)".into(),
                        phase: phase.clone(),
                        detail: d.clone(),
                        running: true,
                        skill: if phase == "skill" || phase == "invoke_skill" {
                            Some(detail.clone())
                        } else {
                            None
                        },
                    });
                }
                self.blocks
                    .push(DisplayBlock::System(format!("↳ {short}… · {phase} · {d}")));
                transcript_dirty = true;
            }
            AgentEvent::ChildSessionCompleted {
                child_session_id,
                status,
                ..
            } => {
                let short = short_session_prefix(child_session_id);
                if let Some(row) = self
                    .subagents
                    .iter_mut()
                    .find(|r| r.id == *child_session_id)
                {
                    row.running = false;
                    row.phase = "done".into();
                    row.detail = status.clone();
                }
                self.blocks.push(DisplayBlock::System(format!(
                    "Sub-agent {short}… done: {status}"
                )));
                transcript_dirty = true;
            }
            AgentEvent::BusyStateChanged { state } => {
                self.set_busy_state(*state);
                if matches!(state, BusyState::Idle) && self.process_title == "writing response" {
                    self.set_process("idle", "");
                }
            }
            _ => {}
        }
        if transcript_dirty {
            self.touch_transcript();
        }
    }
}

fn short_session_prefix(id: &str) -> &str {
    if id.len() > 8 { &id[..8] } else { id }
}

fn truncate(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max {
        t.to_string()
    } else {
        format!(
            "{}…",
            t.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dcode_ai_common::event::{
        AgentEvent, InteractiveQuestionPayload, QuestionOption, QuestionSelection,
    };

    fn test_state() -> TuiSessionState {
        TuiSessionState::new(
            "s".into(),
            "m".into(),
            "@build".into(),
            "default".into(),
            PathBuf::from("/tmp"),
            false,
        )
    }

    #[test]
    fn history_search_matches_filters_recent_first() {
        let mut st = test_state();
        st.composer_history_search_open = true;
        st.composer_history_search_query = "fix".into();
        let history = vec![
            "fix the bug".into(),
            "add feature".into(),
            "fix tests".into(),
        ];
        let matches = st.history_search_matches(&history);
        // Most-recent (last) first, only entries containing "fix".
        assert_eq!(matches, vec!["fix tests", "fix the bug"]);
    }

    #[test]
    fn history_search_typing_and_backspace_update_query() {
        let mut st = test_state();
        st.composer_history_search_open = true;
        st.composer_history_search_index = 3;
        st.apply_history_search_key(HistorySearchKey::Char('a'), &[]);
        st.apply_history_search_key(HistorySearchKey::Char('b'), &[]);
        assert_eq!(st.composer_history_search_query, "ab");
        assert_eq!(st.composer_history_search_index, 0); // reset on edit
        st.apply_history_search_key(HistorySearchKey::Backspace, &[]);
        assert_eq!(st.composer_history_search_query, "a");
    }

    #[test]
    fn history_search_down_clamps_to_matches() {
        let mut st = test_state();
        st.composer_history_search_open = true;
        let history = vec!["one".into(), "two".into()];
        // Two matches → index clamps at 1.
        for _ in 0..5 {
            st.apply_history_search_key(HistorySearchKey::Down, &history);
        }
        assert_eq!(st.composer_history_search_index, 1);
        st.apply_history_search_key(HistorySearchKey::Up, &history);
        assert_eq!(st.composer_history_search_index, 0);
    }

    #[test]
    fn history_search_accept_sets_input_and_closes() {
        let mut st = test_state();
        st.composer_history_search_open = true;
        let history = vec!["cargo build".into(), "cargo test".into()];
        // index 0 → most recent match ("cargo test").
        st.apply_history_search_key(HistorySearchKey::Accept, &history);
        assert_eq!(st.input_buffer, "cargo test");
        assert!(!st.composer_history_search_open);
        assert!(st.composer_history_search_query.is_empty());
    }

    #[test]
    fn history_search_cancel_closes_without_setting_input() {
        let mut st = test_state();
        st.composer_history_search_open = true;
        st.composer_history_search_query = "x".into();
        st.apply_history_search_key(HistorySearchKey::Cancel, &["x cmd".into()]);
        assert!(!st.composer_history_search_open);
        assert!(st.input_buffer.is_empty());
    }

    fn branch_picker_state() -> TuiSessionState {
        let mut st = test_state();
        st.branch_picker_open = true;
        st.branch_picker_branches = vec!["main".into(), "feature-login".into(), "release".into()];
        st
    }

    #[test]
    fn branch_picker_typing_filters_and_resets_index() {
        let mut st = branch_picker_state();
        st.branch_picker_index = 2;
        assert!(
            st.apply_branch_picker_key(BranchPickerKey::Char('f'))
                .is_none()
        );
        assert_eq!(st.branch_picker_query, "f");
        assert_eq!(st.branch_picker_index, 0);
        st.apply_branch_picker_key(BranchPickerKey::Backspace);
        assert_eq!(st.branch_picker_query, "");
    }

    #[test]
    fn branch_picker_down_clamps_to_filtered_len() {
        let mut st = branch_picker_state();
        for _ in 0..10 {
            st.apply_branch_picker_key(BranchPickerKey::Down);
        }
        assert_eq!(st.branch_picker_index, 2); // 3 branches → max index 2
        st.apply_branch_picker_key(BranchPickerKey::Up);
        assert_eq!(st.branch_picker_index, 1);
    }

    #[test]
    fn branch_picker_accept_existing_returns_switch_and_closes() {
        let mut st = branch_picker_state();
        st.branch_picker_query = "feature-login".into();
        let cmd = st.apply_branch_picker_key(BranchPickerKey::Accept);
        assert!(matches!(cmd, Some(TuiCmd::SwitchBranch(b)) if b == "feature-login"));
        assert!(!st.branch_picker_open);
    }

    #[test]
    fn branch_picker_accept_slash_query_returns_create() {
        let mut st = branch_picker_state();
        st.branch_picker_query = "/new-branch".into();
        let cmd = st.apply_branch_picker_key(BranchPickerKey::Accept);
        assert!(matches!(cmd, Some(TuiCmd::CreateBranch(b)) if b == "new-branch"));
    }

    #[test]
    fn branch_picker_cancel_closes_with_no_command() {
        let mut st = branch_picker_state();
        let cmd = st.apply_branch_picker_key(BranchPickerKey::Cancel);
        assert!(cmd.is_none());
        assert!(!st.branch_picker_open);
    }

    fn question_state(allow_custom: bool) -> TuiSessionState {
        let mut st = test_state();
        st.active_question = Some(InteractiveQuestionPayload {
            question_id: "q1".into(),
            call_id: "c1".into(),
            prompt: "Pick one".into(),
            options: vec![
                QuestionOption {
                    id: "opt-a".into(),
                    label: "A".into(),
                },
                QuestionOption {
                    id: "opt-b".into(),
                    label: "B".into(),
                },
            ],
            allow_custom,
            suggested_answer: String::new(),
        });
        st.question_modal_open = true;
        st.question_modal_index = 0;
        st
    }

    #[test]
    fn question_modal_accept_suggested_and_option() {
        let mut st = question_state(false);
        // index 0 → suggested
        assert_eq!(
            st.apply_question_modal_key(QuestionModalKey::Accept),
            QuestionModalOutcome::Answer {
                question_id: "q1".into(),
                selection: QuestionSelection::Suggested,
            }
        );

        let mut st = question_state(false);
        st.question_modal_index = 1; // first option
        assert_eq!(
            st.apply_question_modal_key(QuestionModalKey::Accept),
            QuestionModalOutcome::Answer {
                question_id: "q1".into(),
                selection: QuestionSelection::Option {
                    option_id: "opt-a".into()
                },
            }
        );
    }

    #[test]
    fn question_modal_down_clamps_to_total() {
        let mut st = question_state(true); // total = 1 + 2 + 1 = 4 → max idx 3
        for _ in 0..10 {
            st.apply_question_modal_key(QuestionModalKey::Down);
        }
        assert_eq!(st.question_modal_index, 3);
    }

    #[test]
    fn question_modal_chat_about_this_keeps_active() {
        let mut st = question_state(true);
        st.question_modal_index = 3; // the "chat about this" row
        assert_eq!(
            st.apply_question_modal_key(QuestionModalKey::Accept),
            QuestionModalOutcome::CloseKeepActive
        );
        assert!(!st.question_modal_open);
        assert!(st.active_question.is_some());
    }

    #[test]
    fn connect_modal_ignores_opening_enter_then_accepts() {
        let mut st = test_state();
        st.connect_modal_open = true;
        st.connect_modal_ignore_enter_once = true;
        // First Enter is swallowed (the one that opened the modal).
        assert!(
            st.apply_connect_modal_key(ConnectModalKey::Accept)
                .is_none()
        );
        assert!(!st.connect_modal_ignore_enter_once);
        assert!(st.connect_modal_open); // still open
        // A real Enter now selects an action (the catalog always has entries).
        let action = st.apply_connect_modal_key(ConnectModalKey::Accept);
        assert!(action.is_some());
        assert!(!st.connect_modal_open);
    }

    #[test]
    fn connect_modal_typing_updates_search() {
        let mut st = test_state();
        st.connect_modal_open = true;
        st.apply_connect_modal_key(ConnectModalKey::Char('o'));
        st.apply_connect_modal_key(ConnectModalKey::Char('p'));
        assert_eq!(st.connect_search, "op");
        st.apply_connect_modal_key(ConnectModalKey::Backspace);
        assert_eq!(st.connect_search, "o");
    }

    #[test]
    fn pins_modal_delete_removes_and_closes_when_empty() {
        let mut st = test_state();
        st.pins_modal_open = true;
        st.pinned_notes = vec![PinnedNote {
            title: "t".into(),
            body: "b".into(),
        }];
        st.pins_modal_index = 0;
        assert!(!st.apply_pins_modal_key(PinsModalKey::Delete));
        assert!(st.pinned_notes.is_empty());
        assert!(!st.pins_modal_open); // auto-closes when last note removed
    }

    #[test]
    fn pins_modal_copy_requests_clipboard() {
        let mut st = test_state();
        st.pins_modal_open = true;
        st.pinned_notes = vec![PinnedNote {
            title: "t".into(),
            body: "b".into(),
        }];
        assert!(st.apply_pins_modal_key(PinsModalKey::Copy)); // signals copy to the loop
        assert!(st.pins_modal_open); // copy doesn't close
    }

    #[test]
    fn provider_picker_accept_routes_by_for_api_key_flag() {
        use dcode_ai_common::config::ProviderKind;
        let mut st = test_state();
        st.provider_picker_open = true;
        st.provider_picker_index = 0;
        st.provider_picker_for_api_key = false;
        let out = st.apply_provider_picker_key(ProviderPickerKey::Accept);
        assert_eq!(
            out,
            Some(ProviderPickerOutcome::Apply(ProviderKind::ALL[0]))
        );
        assert!(!st.provider_picker_open);

        let mut st = test_state();
        st.provider_picker_open = true;
        st.provider_picker_for_api_key = true;
        let out = st.apply_provider_picker_key(ProviderPickerKey::Accept);
        assert_eq!(
            out,
            Some(ProviderPickerOutcome::ForApiKey(ProviderKind::ALL[0]))
        );
    }

    #[test]
    fn provider_picker_down_wraps() {
        let mut st = test_state();
        st.provider_picker_open = true;
        let n = dcode_ai_common::config::ProviderKind::ALL.len();
        st.provider_picker_index = n - 1;
        st.apply_provider_picker_key(ProviderPickerKey::Down);
        assert_eq!(st.provider_picker_index, 0); // wraps
    }

    #[test]
    fn session_picker_accept_returns_filtered_id() {
        let mut st = test_state();
        st.session_picker_open = true;
        st.session_picker_entries = vec![
            SessionPickerEntry {
                id: "sess-1".into(),
                label: "fix auth".into(),
                search_text: "fix auth".into(),
            },
            SessionPickerEntry {
                id: "sess-2".into(),
                label: "add tests".into(),
                search_text: "add tests".into(),
            },
        ];
        st.session_picker_search = "tests".into();
        st.session_picker_index = 0; // only one match → sess-2
        let id = st.apply_session_picker_key(SessionPickerKey::Accept);
        assert_eq!(id.as_deref(), Some("sess-2"));
        assert!(!st.session_picker_open);
    }

    #[test]
    fn session_picker_cancel_closes_no_id() {
        let mut st = test_state();
        st.session_picker_open = true;
        assert!(
            st.apply_session_picker_key(SessionPickerKey::Cancel)
                .is_none()
        );
        assert!(!st.session_picker_open);
    }

    #[test]
    fn model_picker_accept_returns_action_and_skips_headers() {
        let mut st = test_state();
        st.model_picker_open = true;
        st.model_picker_entries = vec![
            ModelPickerEntry {
                label: "Anthropic".into(),
                detail: "".into(),
                action: ModelPickerAction::SwitchCopilot,
                is_header: true, // header → not selectable
            },
            ModelPickerEntry {
                label: "claude-sonnet-4-6".into(),
                detail: "anthropic".into(),
                action: ModelPickerAction::ApplyModel("claude-sonnet-4-6".into()),
                is_header: false,
            },
        ];
        st.model_picker_index = 0; // first *selectable* = the non-header entry
        let action = st.apply_model_picker_key(ModelPickerKey::Accept);
        assert!(
            matches!(action, Some(ModelPickerAction::ApplyModel(m)) if m == "claude-sonnet-4-6")
        );
        assert!(!st.model_picker_open);
    }

    #[test]
    fn model_picker_search_filters_and_resets_index() {
        let mut st = test_state();
        st.model_picker_open = true;
        st.model_picker_index = 5;
        st.model_picker_entries = vec![ModelPickerEntry {
            label: "gpt-4o".into(),
            detail: "openai".into(),
            action: ModelPickerAction::ApplyModel("gpt-4o".into()),
            is_header: false,
        }];
        assert!(
            st.apply_model_picker_key(ModelPickerKey::Char('x'))
                .is_none()
        );
        assert_eq!(st.model_picker_search, "x");
        assert_eq!(st.model_picker_index, 0);
    }

    #[test]
    fn info_modal_scroll_clamps_and_home_resets() {
        let mut st = test_state();
        st.info_modal_open = true;
        st.info_modal_lines = (0..10).map(|i| format!("line {i}")).collect();
        st.info_modal_view_rows = 4; // max_scroll = 10 - 4 = 6
        for _ in 0..50 {
            st.apply_info_modal_key(InfoModalKey::ScrollDown);
        }
        assert_eq!(st.info_modal_scroll, 6);
        st.apply_info_modal_key(InfoModalKey::ScrollUp);
        assert_eq!(st.info_modal_scroll, 5);
        st.apply_info_modal_key(InfoModalKey::Home);
        assert_eq!(st.info_modal_scroll, 0);
        assert_eq!(st.info_modal_hscroll, 0);
    }

    #[test]
    fn info_modal_close_dismisses() {
        let mut st = test_state();
        st.info_modal_open = true;
        st.apply_info_modal_key(InfoModalKey::Close);
        assert!(!st.info_modal_open);
    }

    #[test]
    fn command_palette_accept_loads_slash_command() {
        let mut st = test_state();
        st.command_palette_open = true;
        st.command_palette_query = "switch model".into();
        st.palette_index = 0;
        st.apply_command_palette_key(CommandPaletteKey::Accept);
        assert_eq!(st.input_buffer, "/models");
        assert!(!st.command_palette_open);
        assert!(st.command_palette_query.is_empty());
    }

    #[test]
    fn command_palette_cancel_closes_without_input() {
        let mut st = test_state();
        st.command_palette_open = true;
        st.command_palette_query = "abc".into();
        st.palette_index = 2;
        st.apply_command_palette_key(CommandPaletteKey::Cancel);
        assert!(!st.command_palette_open);
        assert!(st.command_palette_query.is_empty());
        assert_eq!(st.palette_index, 0);
        assert!(st.input_buffer.is_empty());
    }

    #[test]
    fn command_palette_typing_appends_and_clamps_index() {
        let mut st = test_state();
        st.command_palette_open = true;
        st.palette_index = 99;
        st.apply_command_palette_key(CommandPaletteKey::Char('z'));
        assert_eq!(st.command_palette_query, "z");
        // index clamped to the (possibly empty) filtered selectable range.
        assert!(st.palette_index < 50);
    }

    #[test]
    fn approval_allow_pattern_falls_back_on_invalid_input() {
        let req = ApprovalRequest {
            call_id: "c1".into(),
            tool: "execute_bash".into(),
            description: "run".into(),
            input: "not valid json".into(),
        };
        // Invalid JSON → empty input → bare tool wildcard (no panic).
        assert_eq!(req.allow_pattern(), "execute_bash:*");
    }

    #[test]
    fn question_modal_esc_depends_on_allow_custom() {
        let mut st = question_state(false);
        assert_eq!(
            st.apply_question_modal_key(QuestionModalKey::Cancel),
            QuestionModalOutcome::Stay
        );
        assert!(st.question_modal_open); // no-op when custom not allowed

        let mut st = question_state(true);
        assert_eq!(
            st.apply_question_modal_key(QuestionModalKey::Cancel),
            QuestionModalOutcome::CloseKeepActive
        );
        assert!(!st.question_modal_open);
    }

    #[test]
    fn question_requested_sets_active_question() {
        let mut st = TuiSessionState::new(
            "session-x".into(),
            "m".into(),
            "@build".into(),
            "default".into(),
            PathBuf::from("/tmp"),
            false,
        );
        let q = InteractiveQuestionPayload {
            question_id: "q-1".into(),
            call_id: "c1".into(),
            prompt: "Pick".into(),
            options: vec![QuestionOption {
                id: "a".into(),
                label: "A".into(),
            }],
            allow_custom: true,
            suggested_answer: "A".into(),
        };
        st.apply_event(&AgentEvent::QuestionRequested {
            question: q.clone(),
        });
        assert_eq!(
            st.active_question.as_ref().map(|x| x.question_id.as_str()),
            Some("q-1")
        );
        assert!(matches!(st.blocks.last(), Some(DisplayBlock::Question(_))));

        st.apply_event(&AgentEvent::QuestionResolved {
            question_id: "q-1".into(),
            selection: QuestionSelection::Suggested,
        });
        assert!(st.active_question.is_none());
    }

    #[test]
    fn child_session_activity_updates_subagent_row() {
        let mut st = TuiSessionState::new(
            "session-x".into(),
            "m".into(),
            "@build".into(),
            "default".into(),
            PathBuf::from("/tmp"),
            false,
        );
        st.apply_event(&AgentEvent::ChildSessionSpawned {
            parent_session_id: "session-x".into(),
            child_session_id: "child-abc".into(),
            task: "do the thing".into(),
            workspace: std::path::PathBuf::from("/tmp"),
            branch: None,
        });
        assert_eq!(st.subagents.len(), 1);
        st.apply_event(&AgentEvent::ChildSessionActivity {
            child_session_id: "child-abc".into(),
            phase: "read_file".into(),
            detail: "src/lib.rs".into(),
        });
        assert_eq!(st.subagents[0].phase, "read_file");
        assert_eq!(st.subagents[0].detail, "src/lib.rs");
    }

    #[test]
    fn tui_state_defaults_mouse_capture_on() {
        let st = TuiSessionState::new(
            "session-x".into(),
            "m".into(),
            "@build".into(),
            "default".into(),
            PathBuf::from("/tmp"),
            false,
        );
        assert!(st.mouse_capture_on);
    }

    #[test]
    fn approval_requested_promotes_running_tool_with_input() {
        let mut st = TuiSessionState::new(
            "session-x".into(),
            "m".into(),
            "@build".into(),
            "default".into(),
            PathBuf::from("/tmp"),
            false,
        );
        st.apply_event(&AgentEvent::ToolCallStarted {
            call_id: "call-1".into(),
            tool: "execute_bash".into(),
            input: serde_json::json!({"command":"ls -la"}),
        });
        st.apply_event(&AgentEvent::ApprovalRequested {
            call_id: "call-1".into(),
            tool: "execute_bash".into(),
            description: "Tool `execute_bash` requires approval".into(),
        });

        assert!(st.active_approval.is_some());
        let req = st
            .blocks
            .iter()
            .rev()
            .find_map(|block| match block {
                DisplayBlock::ApprovalPending(req) if req.call_id == "call-1" => Some(req),
                _ => None,
            })
            .expect("expected approval block for call-1");
        assert_eq!(req.tool, "execute_bash");
        assert!(req.input.contains("command"));
        assert!(req.input.contains("ls -la"));
    }

    #[test]
    fn tool_events_append_codex_style_activity_lines() {
        let mut st = TuiSessionState::new(
            "session-x".into(),
            "m".into(),
            "@build".into(),
            "default".into(),
            PathBuf::from("/tmp/repo"),
            false,
        );
        st.apply_event(&AgentEvent::ToolCallStarted {
            call_id: "call-1".into(),
            tool: "web_search".into(),
            input: serde_json::json!({"query":"tokio tutorial"}),
        });
        st.apply_event(&AgentEvent::ToolCallCompleted {
            call_id: "call-1".into(),
            output: dcode_ai_common::tool::ToolResult {
                call_id: "call-1".into(),
                success: true,
                output: "- result".into(),
                error: None,
            },
        });

        assert!(st.blocks.iter().any(
            |block| matches!(block, DisplayBlock::System(s) if s == "Using web context: tokio tutorial")
        ));
        assert!(
            st.blocks
                .iter()
                .any(|block| matches!(block, DisplayBlock::System(s) if s == "Web context ready"))
        );
    }

    #[test]
    fn clear_replayed_interaction_state_drops_stale_prompts() {
        let mut st = TuiSessionState::new(
            "session-x".into(),
            "m".into(),
            "@build".into(),
            "default".into(),
            PathBuf::from("/tmp"),
            false,
        );
        st.active_approval = Some(ApprovalRequest {
            call_id: "call-1".into(),
            tool: "execute_bash".into(),
            description: "approve".into(),
            input: "{}".into(),
        });
        st.active_question = Some(InteractiveQuestionPayload {
            question_id: "q-1".into(),
            call_id: "call-2".into(),
            prompt: "Pick".into(),
            options: vec![],
            allow_custom: true,
            suggested_answer: String::new(),
        });

        st.clear_replayed_interaction_state();

        assert!(st.active_approval.is_none());
        assert!(st.active_question.is_none());
    }

    #[test]
    fn open_close_question_modal() {
        let mut st = TuiSessionState::new(
            "s".into(),
            "m".into(),
            "@build".into(),
            "default".into(),
            PathBuf::from("/tmp"),
            false,
        );
        assert!(!st.question_modal_open);
        assert_eq!(st.question_modal_index, 0);

        st.open_question_modal();
        assert!(st.question_modal_open);
        assert_eq!(st.question_modal_index, 0);
        assert_eq!(st.question_modal_scroll, 0);

        st.question_modal_index = 3;
        st.close_question_modal();
        assert!(!st.question_modal_open);
        assert_eq!(st.question_modal_index, 0);
        assert_eq!(st.question_modal_scroll, 0);
    }

    #[test]
    fn question_requested_opens_modal() {
        let mut st = TuiSessionState::new(
            "s".into(),
            "m".into(),
            "@build".into(),
            "default".into(),
            PathBuf::from("/tmp"),
            false,
        );
        let q = InteractiveQuestionPayload {
            question_id: "q-1".into(),
            call_id: "c1".into(),
            prompt: "Pick".into(),
            options: vec![QuestionOption {
                id: "a".into(),
                label: "A".into(),
            }],
            allow_custom: true,
            suggested_answer: "A".into(),
        };
        st.apply_event(&AgentEvent::QuestionRequested {
            question: q.clone(),
        });
        assert!(st.question_modal_open);
        assert_eq!(st.question_modal_index, 0);
        assert!(st.active_question.is_some());
    }

    #[test]
    fn question_resolved_closes_modal() {
        let mut st = TuiSessionState::new(
            "s".into(),
            "m".into(),
            "@build".into(),
            "default".into(),
            PathBuf::from("/tmp"),
            false,
        );
        st.question_modal_open = true;
        st.question_modal_index = 2;
        st.apply_event(&AgentEvent::QuestionResolved {
            question_id: "q-1".into(),
            selection: QuestionSelection::Suggested,
        });
        assert!(st.active_question.is_none());
        assert!(!st.question_modal_open);
        assert_eq!(st.question_modal_index, 0);
    }
}
