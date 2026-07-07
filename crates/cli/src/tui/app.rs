//! Full-screen session TUI: transcript, streaming assistant, composer.

#![allow(clippy::collapsible_match, clippy::explicit_into_iter_loop)]

use std::collections::HashSet;
use std::path::PathBuf;

use crate::file_mentions;
use crate::tool_ui;
use crate::tui::answer_parse::{parse_approval_verdict, parse_tui_question_answer};
use crate::tui::composer_input::*;
use crate::tui::connect_modal::{ConnectAction, build_connect_rows, selectable_row_indices};
use crate::tui::layout::{centered_rect, layout_chunks, layout_with_sidebar};
use crate::tui::mouse::{is_click_jitter, mouse_left_activated, mouse_scroll_step, rect_contains};
use crate::tui::oauth_status::{
    oauth_logged_in_for_slug, oauth_login_provider_slug, oauth_switch_command_for_slug,
};
use crate::tui::paste::{expand_paste_tokens, pasted_lines_token};
use crate::tui::path_parse::{extract_embedded_path_fragments, parse_candidate_image_path};
use crate::tui::render::modals::{
    render_api_key_modal, render_approval_popup, render_at_panel, render_command_palette,
    render_connect_modal, render_info_modal, render_question_modal, render_slash_panel,
};
use crate::tui::render_helpers::{popup_block, truncate_chars};
use crate::tui::slash_entries::{
    SLASH_PANEL_MAX_ROWS, SlashEntry, filter_slash_entries, load_slash_entries, slash_panel_height,
    slash_panel_visible,
};
use crate::tui::state::{
    BacktrackKey, BranchPickerKey, CommandPaletteKey, ConnectModalKey, DisplayBlock,
    HistorySearchKey, InfoModalKey, ModelPickerAction, ModelPickerKey, PinnedNote, PinsModalKey,
    ProviderPickerKey, ProviderPickerOutcome, QuestionModalKey, QuestionModalOutcome,
    SessionPickerKey, TuiSessionState,
};
use crate::tui::terminal::{restore_terminal, setup_terminal};
use crate::tui::transcript::transcript_lines_and_hits;
use arboard::Clipboard;
use crossterm::{
    cursor::MoveToColumn,
    event::{
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind, poll,
        read,
    },
    execute,
};
use dcode_ai_common::auth::AuthStore;
use dcode_ai_common::config::ProviderKind;
use dcode_ai_common::event::{BusyState, QuestionSelection};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear as ClearWidget, Padding, Paragraph, Widget},
};
use std::io::stdout;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

const STARTUP_APPROVE_ALL_QUESTION_ID: &str = "startup-approve-all";

/// Message from TUI to the approval dispatch task.
#[derive(Debug)]
pub enum ApprovalAnswer {
    Verdict {
        call_id: String,
        approved: bool,
    },
    AllowPattern {
        call_id: String,
        pattern: String,
    },
    /// Approve with modified tool input (partial hunk selection).
    ModifiedApproval {
        call_id: String,
        modified_input: serde_json::Value,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum LineClickHit {
    Question(QuestionSelection),
    CopyText(String),
    /// Toggle full vs preview rendering of thinking blocks.
    ToggleThinking,
    /// Open a URL in the browser or a file path in the editor.
    OpenLink(String),
    /// Toggle collapse/expand of an assistant response block.
    #[allow(dead_code)]
    ToggleAssistant(usize),
}

/// Per flattened transcript line: click action (same indices as `transcript_lines`).
pub(crate) type LineAnswerHit = Option<LineClickHit>;

#[derive(Debug)]
pub enum TuiCmd {
    Submit(String),
    /// Rewind the conversation to just before a past user message (picked in
    /// the backtrack overlay). Index counts user messages from the end; text
    /// is verified runtime-side before truncating.
    Backtrack {
        user_index_from_end: usize,
        text: String,
    },
    /// Queue a steering message while a turn is in progress (`Enter` when busy).
    QueueSteering(String),
    /// Queue a follow-up message for later (`Alt+Enter` when busy).
    QueueFollowUp(String),
    /// Answer for the current `ask_question` (from question mode or `/auto-answer`).
    QuestionAnswer(dcode_ai_common::event::QuestionSelection),
    CycleAgent,
    CancelTurn,
    Exit,
    /// Open the branch picker popup.
    OpenBranchPicker,
    /// Switch to the given branch name.
    SwitchBranch(String),
    /// Create a new branch with the given name and switch to it.
    CreateBranch(String),
    /// Apply workspace default provider (from TUI picker).
    ApplyDefaultProvider(ProviderKind),
    /// Open API key modal for provider; bool indicates whether to connect after save/confirm.
    PromptApiKey(ProviderKind, bool),
    /// Apply a model name (from the model picker).
    ApplyModel(String),
    /// Switch provider (from the model picker).
    ApplyModelProvider(ProviderKind),
    /// Apply permission mode (from the permission picker).
    ApplyPermission(usize),
    /// Switch agent profile (from the agent picker).
    SwitchAgent(usize),
    /// Open external editor via leader key.
    OpenEditor,
    /// Start a new session.
    NewSession,
    /// Run compact.
    RunCompact,
    /// Open model picker (triggered by leader key or command palette).
    OpenModelPicker,
    /// Open status info modal.
    OpenStatus,
    /// Open help info modal.
    OpenHelp,
    /// Open agent picker.
    OpenAgentPicker,
    /// Open permission picker (reserved for future shortcut).
    #[allow(dead_code)]
    OpenPermissionPicker,
    /// Open sessions picker/info.
    OpenSessions,
    /// Cycle to the next recent model (F2 forward, Shift+F2 backward).
    CycleModel(bool),
    /// Validate an API key for onboarding (provider, api_key).
    /// The repl handler looks up base_url from config.
    ValidateApiKey(ProviderKind, String),
    /// Complete Anthropic OAuth with pasted authorization code from TUI modal.
    CompleteAnthropicOAuth {
        code_verifier: String,
        authorization_code: String,
    },
    /// Mark onboarding as complete and persist the flag.
    #[allow(dead_code)]
    CompleteOnboarding,
    /// Resume a different session by ID.
    ResumeSession(String),
    /// Apply a theme by name (from the theme picker).
    ApplyTheme(String),
    /// Switch to a connected project by index.
    SwitchProject(usize),
    /// Add a project directory to the connected projects list.
    #[allow(dead_code)]
    AddProject(std::path::PathBuf),
    /// Open the project picker modal.
    OpenProjectPicker,
}

use super::theme;

/// Gentle hint when the configured model is a clearly-superseded Anthropic
/// generation (claude-2 / claude-3 / instant). Returns `None` for current 4.x /
/// Fable models and for any non-Anthropic id (custom / OpenRouter), so it never
/// false-positives on a valid model the user deliberately chose.
fn outdated_model_hint(model: &str) -> Option<&'static str> {
    let m = model.to_ascii_lowercase();
    let outdated =
        m.starts_with("claude-2") || m.starts_with("claude-instant") || m.starts_with("claude-3");
    outdated.then_some(
        "older Anthropic model — newer ones: opus / sonnet / haiku / fable (see /models)",
    )
}

/// Startup banner: the dcode robot-face logo + session info, flushed once to
/// the terminal scrollback at session start. Returns styled lines.
pub fn banner_lines(model: &str, workspace: &str, width: u16) -> Vec<Line<'static>> {
    let version = env!("CARGO_PKG_VERSION");
    // Original dcode robot-face logo (6 rows).
    let logo: [&str; 6] = [
        "   ___   ",
        "  /   \\  ",
        " | x x | ",
        " |  ^  | ",
        " |_____| ",
        "  |   |  ",
    ];
    let logo_style = Style::default()
        .fg(theme::accent())
        .add_modifier(Modifier::BOLD);

    let ws = if workspace.chars().count() > 52 {
        let tail: String = workspace
            .chars()
            .rev()
            .take(49)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        format!("…{tail}")
    } else {
        workspace.to_string()
    };
    // Info shown beside the logo (rows 1-4); rows 0 and 5 are logo-only.
    let info: [Option<(String, Style)>; 6] = [
        None,
        Some((
            format!("dcode-ai v{version}"),
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        )),
        Some((
            "Rust-native coding agent".to_string(),
            Style::default().fg(theme::muted()),
        )),
        Some((model.to_string(), Style::default().fg(theme::text()))),
        Some((ws, Style::default().fg(theme::muted()))),
        None,
    ];

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::default());
    for i in 0..6 {
        let mut spans = vec![Span::styled(logo[i].to_string(), logo_style)];
        if let Some((text, style)) = &info[i] {
            spans.push(Span::raw("   "));
            spans.push(Span::styled(text.clone(), *style));
        }
        lines.push(Line::from(spans));
    }
    lines.push(Line::default());
    let sep_w = (width as usize).clamp(20, 100);
    lines.push(Line::from(Span::styled(
        "─".repeat(sep_w),
        Style::default().fg(theme::border()),
    )));
    // "Update available" hint (Codex parity), from the cached version check.
    if let Some(latest) = crate::update_check::pending_upgrade() {
        lines.push(Line::from(vec![
            Span::styled("  ⬆ update available: ", Style::default().fg(theme::warn())),
            Span::styled(
                format!("v{version} → v{latest}"),
                Style::default()
                    .fg(theme::warn())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  ·  see ", Style::default().add_modifier(Modifier::DIM)),
            Span::styled(
                "github.com/Dhanuzh/dcode-ai/releases",
                Style::default()
                    .fg(theme::muted())
                    .add_modifier(Modifier::UNDERLINED),
            ),
        ]));
    }
    // Gentle model-deprecation hint (Codex parity for model migration).
    if let Some(hint) = outdated_model_hint(model) {
        lines.push(Line::from(vec![
            Span::styled("  ⚠ ", Style::default().fg(theme::warn())),
            Span::styled(hint, Style::default().fg(theme::muted())),
        ]));
    }
    lines.push(Line::default());
    lines
}

fn copy_to_clipboard(text: &str) -> Result<String, String> {
    crate::tui::clipboard::copy_to_clipboard(text)
}

fn paste_text_from_clipboard() -> Result<String, arboard::Error> {
    let mut cb = Clipboard::new()?;
    cb.get_text()
}

/// Matches `PermissionMode` as stored via `format!("{:?}", mode)` (e.g. `BypassPermissions`).
fn toolbar_permission_is_bypass(mode: &str) -> bool {
    mode.contains("BypassPermissions")
}

fn menu_content_for_state(state: &TuiSessionState) -> crate::tui::tui_types::MenuContent {
    use crate::tui::tui_types::MenuContent;
    if state.command_palette_open {
        MenuContent::CommandPalette
    } else if state.info_modal_open {
        MenuContent::Info
    } else if state.model_picker_open {
        MenuContent::ModelPicker
    } else if state.session_picker_open {
        MenuContent::SessionPicker
    } else if state.connect_modal_open {
        MenuContent::Connect
    } else if state.transcript_search_open {
        MenuContent::TranscriptSearch
    } else if state.composer_history_search_open {
        MenuContent::ComposerHistorySearch
    } else if state.question_modal_open {
        MenuContent::Question
    } else if state.active_approval.is_some() {
        MenuContent::Approval
    } else if state.pins_modal_open {
        MenuContent::Pins
    } else if state.subagent_modal_open {
        MenuContent::SubAgents
    } else if slash_panel_visible(&state.input_buffer) {
        MenuContent::Slash
    } else if at_completion_active(&state.input_buffer, state.cursor_char_idx) {
        MenuContent::FileMention
    } else {
        MenuContent::None
    }
}

fn escape_cancels_active_turn(state: &TuiSessionState) -> bool {
    matches!(
        state.current_busy_state,
        BusyState::Thinking
            | BusyState::Streaming
            | BusyState::ToolRunning
            | BusyState::ApprovalPending
    ) || state.active_approval.is_some()
}

fn request_turn_cancel(
    state: &mut TuiSessionState,
    cancel_flag: Option<&Arc<std::sync::atomic::AtomicBool>>,
    cmd_tx: &UnboundedSender<TuiCmd>,
) {
    if !escape_cancels_active_turn(state) {
        return;
    }
    let first_request = if let Some(flag) = cancel_flag {
        !flag.swap(true, std::sync::atomic::Ordering::SeqCst)
    } else {
        true
    };
    if first_request {
        state
            .blocks
            .push(DisplayBlock::System("Cancelling current run...".into()));
        state.set_busy_state(BusyState::Idle);
        let _ = cmd_tx.send(TuiCmd::CancelTurn);
    }
}

fn stage_pasted_image_paths(state: &mut TuiSessionState, pasted: &str) -> Result<usize, String> {
    let normalized = pasted.replace("\r\n", "\n").replace('\r', "\n");
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        return Ok(0);
    }

    let mut staged = 0usize;
    let mut tried_any = false;
    let mut first_error: Option<String> = None;

    let mut seen: HashSet<String> = HashSet::new();
    for line in trimmed.lines() {
        for fragment in extract_embedded_path_fragments(line) {
            let Some(src) = parse_candidate_image_path(&fragment) else {
                continue;
            };
            let key = src.to_string_lossy().into_owned();
            if !seen.insert(key) {
                continue;
            }

            tried_any = true;
            match crate::image_attach::import_image_file(
                &state.workspace_root,
                &state.session_id,
                &src,
            ) {
                Ok(att) => {
                    staged += 1;
                    state.staged_image_attachments.push(att);
                }
                Err(e) if first_error.is_none() => {
                    first_error = Some(e);
                }
                Err(_) => {}
            }
        }
    }

    if staged > 0 {
        state.push_block(DisplayBlock::System(format!(
            "[image] staged {staged} image(s) — Enter to send"
        )));
        state.touch_transcript();
        return Ok(staged);
    }

    if tried_any {
        return Err(first_error.unwrap_or_else(|| "failed to import pasted image path".into()));
    }

    Ok(0)
}

fn insert_pasted_text(state: &mut TuiSessionState, slash_entries: &[SlashEntry], pasted: &str) {
    state.paste_counter += 1;
    let token = pasted_lines_token(pasted, state.paste_counter);
    let insert_text = if let Some(ref tok) = token {
        state.paste_store.insert(tok.clone(), pasted.to_string());
        tok.clone()
    } else {
        pasted.to_string()
    };
    if insert_text.is_empty() {
        return;
    }
    state.insert_input_str(&insert_text);
    if slash_panel_visible(&state.input_buffer) {
        let f = filter_slash_entries(slash_entries, &state.input_buffer);
        if !f.is_empty() {
            state.slash_menu_index = state.slash_menu_index.min(f.len().saturating_sub(1));
        } else {
            state.slash_menu_index = 0;
        }
    }
}

#[inline]
pub(crate) fn push_transcript_line(
    lines: &mut Vec<Line<'static>>,
    hits: &mut Vec<LineAnswerHit>,
    line: Line<'static>,
    hit: LineAnswerHit,
) {
    lines.push(line);
    hits.push(hit);
}

pub(crate) fn prefixed_line(prefix: Span<'static>, mut line: Line<'static>) -> Line<'static> {
    let mut spans = Vec::with_capacity(line.spans.len() + 1);
    spans.push(prefix);
    spans.append(&mut line.spans);
    Line::from(spans)
}

pub(crate) fn line_has_text(line: &Line<'_>) -> bool {
    line.spans.iter().any(|s| !s.content.trim().is_empty())
}

#[derive(Default)]
struct TranscriptRenderCache {
    width: u16,
    revision: u64,
    code_line_numbers: bool,
    zoom_offset: i32,
    lines: Vec<Line<'static>>,
    hits: Vec<LineAnswerHit>,
}

fn line_plain_text(line: &Line<'_>) -> String {
    line.spans.iter().map(|s| s.content.as_ref()).collect()
}

fn transcript_search_matches(lines: &[Line<'_>], query: &str) -> Vec<usize> {
    let needle = query.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return Vec::new();
    }
    lines
        .iter()
        .enumerate()
        .filter_map(|(i, line)| {
            let hay = line_plain_text(line).to_ascii_lowercase();
            hay.contains(&needle).then_some(i)
        })
        .collect()
}

fn latest_copyable_text(state: &TuiSessionState) -> Option<String> {
    if let Some(stream) = state.streaming_assistant.as_ref()
        && !stream.trim().is_empty()
    {
        return Some(stream.clone());
    }
    state.blocks.iter().rev().find_map(|block| match block {
        DisplayBlock::Assistant(text) if !text.trim().is_empty() => Some(text.clone()),
        _ => None,
    })
}

fn latest_pinnable_note(state: &TuiSessionState) -> Option<PinnedNote> {
    if let Some(stream) = state.streaming_assistant.as_ref()
        && !stream.trim().is_empty()
    {
        return Some(PinnedNote {
            title: "assistant (streaming)".into(),
            body: stream.clone(),
        });
    }
    state.blocks.iter().rev().find_map(|block| match block {
        DisplayBlock::Assistant(text) if !text.trim().is_empty() => Some(PinnedNote {
            title: "assistant".into(),
            body: text.clone(),
        }),
        DisplayBlock::User(text) if !text.trim().is_empty() => Some(PinnedNote {
            title: "user".into(),
            body: text.clone(),
        }),
        DisplayBlock::ToolDone { name, detail, .. } => Some(PinnedNote {
            title: format!("tool: {name}"),
            body: detail.clone(),
        }),
        _ => None,
    })
}

impl TranscriptRenderCache {
    /// Returns `(lines, hits, was_rebuilt)`. When `was_rebuilt` is false the
    /// caller can skip expensive work like copying lines into the scroll buffer.
    fn get_or_rebuild(
        &mut self,
        state: &TuiSessionState,
        width: u16,
    ) -> (&[Line<'static>], &[LineAnswerHit], bool) {
        let zoomed_width = {
            let w = width as i32 - state.transcript_zoom_offset;
            w.max(20) as u16
        };
        let normalized_width = zoomed_width;
        let stale = self.width != normalized_width
            || self.revision != state.transcript_rev
            || self.code_line_numbers != state.code_line_numbers
            || self.zoom_offset != state.transcript_zoom_offset;
        if stale {
            let (lines, hits) = transcript_lines_and_hits(state, normalized_width);
            self.width = normalized_width;
            self.revision = state.transcript_rev;
            self.code_line_numbers = state.code_line_numbers;
            self.zoom_offset = state.transcript_zoom_offset;
            self.lines = lines;
            self.hits = hits;
        }
        (&self.lines, &self.hits, stale)
    }

    fn invalidate(&mut self) {
        self.revision = self.revision.wrapping_sub(1);
    }
}

pub(crate) fn tool_header_detail_spans(name: &str, input: &str) -> Vec<Span<'static>> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(input) else {
        let preview = tool_input_preview(name, input);
        return if preview.is_empty() {
            Vec::new()
        } else {
            vec![Span::styled(
                truncate_chars(&preview, 100),
                Style::default().fg(theme::muted()),
            )]
        };
    };

    use crate::tui::tool_summary::{ToolCallKind, ToolCallSummary};
    let summary = ToolCallSummary::from_call(name, &value);
    match summary.kind {
        ToolCallKind::Bash { command } => {
            if command.is_empty() {
                Vec::new()
            } else {
                vec![Span::styled(
                    truncate_chars(&command, 100),
                    Style::default().fg(theme::warn()),
                )]
            }
        }
        ToolCallKind::Path { path } => {
            if path.is_empty() {
                Vec::new()
            } else {
                vec![Span::styled(
                    truncate_chars(&path, 100),
                    Style::default()
                        .fg(theme::assistant())
                        .add_modifier(Modifier::UNDERLINED),
                )]
            }
        }
        ToolCallKind::Grep { pattern, dir } => vec![
            Span::styled(
                format!("\"{}\"", truncate_chars(&pattern, 48)),
                Style::default()
                    .fg(theme::warn())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" in ", Style::default().fg(theme::muted())),
            Span::styled(
                truncate_chars(&dir, 44),
                Style::default()
                    .fg(theme::assistant())
                    .add_modifier(Modifier::UNDERLINED),
            ),
        ],
        ToolCallKind::Glob { pattern, base } => {
            let mut spans = vec![Span::styled(
                truncate_chars(&pattern, 56),
                Style::default().fg(theme::assistant()),
            )];
            if let Some(base) = base {
                spans.push(Span::styled(" in ", Style::default().fg(theme::muted())));
                spans.push(Span::styled(
                    truncate_chars(&base, 40),
                    Style::default()
                        .fg(theme::assistant())
                        .add_modifier(Modifier::UNDERLINED),
                ));
            }
            spans
        }
        ToolCallKind::List { dir } => vec![Span::styled(
            truncate_chars(&dir, 100),
            Style::default()
                .fg(theme::assistant())
                .add_modifier(Modifier::UNDERLINED),
        )],
        ToolCallKind::WebFetch { url } => vec![Span::styled(
            truncate_chars(&url, 100),
            Style::default()
                .fg(theme::assistant())
                .add_modifier(Modifier::UNDERLINED),
        )],
        ToolCallKind::Generic { value } => value
            .filter(|v| !v.trim().is_empty())
            .map(|v| {
                vec![Span::styled(
                    truncate_chars(&v, 100),
                    Style::default().fg(theme::muted()),
                )]
            })
            .unwrap_or_default(),
    }
}

/// Extract a compact single-line preview of a tool call input for the transcript.
/// Tries common argument keys (path/file_path/command/pattern) from JSON first.
pub(crate) fn tool_input_preview(name: &str, input: &str) -> String {
    let trimmed = input.trim();
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        use crate::tui::tool_summary::{ToolCallKind, ToolCallSummary};
        let summary = ToolCallSummary::from_call(name, &value);
        let out = match summary.kind {
            ToolCallKind::Bash { command } => {
                if command.is_empty() {
                    String::new()
                } else {
                    format!("cmd: {}", truncate_chars(&command, 110))
                }
            }
            ToolCallKind::Path { path } => {
                if path.is_empty() {
                    String::new()
                } else {
                    format!("path: {path}")
                }
            }
            ToolCallKind::Grep { pattern, dir } => {
                format!(
                    "grep `{}` in {}",
                    truncate_chars(&pattern, 60),
                    truncate_chars(&dir, 40)
                )
            }
            ToolCallKind::Glob { pattern, base } => {
                if let Some(base) = base {
                    format!(
                        "glob `{}` in {}",
                        truncate_chars(&pattern, 60),
                        truncate_chars(&base, 40)
                    )
                } else {
                    format!("glob `{}`", truncate_chars(&pattern, 60))
                }
            }
            ToolCallKind::List { dir } => format!("list {dir}"),
            ToolCallKind::WebFetch { url } => format!("url: {}", truncate_chars(&url, 110)),
            ToolCallKind::Generic { value } => value.unwrap_or_default(),
        };
        if !out.trim().is_empty() {
            return out;
        }
    }
    tool_ui::preview_from_display_input(name, input)
}

/// Count added/deleted lines in a unified diff (ignoring `+++`/`---` headers).
pub(crate) fn diff_change_counts(diff: &str) -> (usize, usize) {
    let mut adds = 0usize;
    let mut dels = 0usize;
    for l in diff.lines() {
        if l.starts_with('+') && !l.starts_with("+++") {
            adds += 1;
        } else if l.starts_with('-') && !l.starts_with("---") {
            dels += 1;
        }
    }
    (adds, dels)
}

// Diff hunk logic extracted to tui/diff_hunk.rs
use crate::tui::diff_hunk::{build_hunk_modified_input, extract_approval_hunks};

/// Build a compact colorized diff preview for file-write/edit tool approvals.
/// Returns up to ~6 colored `Line`s showing +/- changes, or empty if not applicable.
pub(crate) fn approval_diff_preview(
    tool: &str,
    input_json: &str,
    max_cols: usize,
) -> Vec<Line<'static>> {
    let lower = tool.to_ascii_lowercase();
    if !lower.contains("write")
        && !lower.contains("edit")
        && !lower.contains("patch")
        && !lower.contains("replace")
    {
        return Vec::new();
    }
    let Ok(val) = serde_json::from_str::<serde_json::Value>(input_json) else {
        return Vec::new();
    };

    let path = val
        .get("path")
        .or_else(|| val.get("file"))
        .or_else(|| val.get("file_path"))
        .or_else(|| val.get("filename"))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    // For edit_file / replace_match: compute diff from old_string → new_string.
    let (old_str_owned, new_str_owned);
    let (old_str, new_str) = if lower.contains("edit") || lower.contains("replace") {
        let old_s = val
            .get("old_string")
            .or_else(|| val.get("old_text"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let new_s = val
            .get("new_string")
            .or_else(|| val.get("new_text"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if old_s.is_empty() && new_s.is_empty() {
            return Vec::new();
        }
        (old_s, new_s)
    } else {
        // write_file: diff between current file and proposed content.
        let new_content = val
            .get("content")
            .or_else(|| val.get("new_content"))
            .or_else(|| val.get("text"))
            .and_then(|v| v.as_str());
        let old_content = if !path.is_empty() {
            std::fs::read_to_string(path).ok()
        } else {
            None
        };
        match (old_content, new_content) {
            (Some(old), Some(new)) => {
                old_str_owned = old;
                new_str_owned = new.to_string();
                (old_str_owned.as_str(), new_str_owned.as_str())
            }
            _ => return Vec::new(),
        }
    };

    use similar::{ChangeTag, TextDiff};
    let diff = TextDiff::from_lines(old_str, new_str);
    let mut out: Vec<Line<'static>> = Vec::new();

    // File path header.
    if !path.is_empty() {
        let (adds, dels) = {
            let mut a = 0usize;
            let mut d = 0usize;
            for change in diff.iter_all_changes() {
                match change.tag() {
                    ChangeTag::Insert => a += 1,
                    ChangeTag::Delete => d += 1,
                    ChangeTag::Equal => {}
                }
            }
            (a, d)
        };
        out.push(Line::from(vec![
            Span::styled(
                truncate_chars(path, max_cols.saturating_sub(16)),
                Style::default()
                    .fg(theme::text())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" | ", Style::default().fg(theme::muted())),
            Span::styled(format!("+{adds}"), Style::default().fg(theme::success())),
            Span::styled(" ", Style::default()),
            Span::styled(format!("-{dels}"), Style::default().fg(theme::error())),
        ]));
    }

    // Show up to 12 changed lines with 1 line of context around each change.
    let max_lines = 12_usize;
    let mut prev_was_context = false;
    for change in diff.iter_all_changes().take(80) {
        if out.len() > max_lines {
            out.push(Line::from(Span::styled(
                "  …",
                Style::default().fg(theme::muted()),
            )));
            break;
        }
        let (sigil, color) = match change.tag() {
            ChangeTag::Insert => ("+", theme::success()),
            ChangeTag::Delete => ("-", theme::error()),
            ChangeTag::Equal => {
                if !prev_was_context && out.len() > 1 {
                    prev_was_context = true;
                    let text = format!(
                        "  {}",
                        truncate_chars(
                            change.value().trim_end_matches('\n'),
                            max_cols.saturating_sub(4)
                        )
                    );
                    out.push(Line::from(Span::styled(
                        text,
                        Style::default().fg(theme::muted()),
                    )));
                }
                continue;
            }
        };
        prev_was_context = false;
        let text = format!(
            "{sigil} {}",
            truncate_chars(
                change.value().trim_end_matches('\n'),
                max_cols.saturating_sub(2)
            )
        );
        out.push(Line::from(Span::styled(text, Style::default().fg(color))));
    }
    out
}

// ── Codex-style popup/panel renderers ──────────────────────────────────

// (Renderers moved to tui/render/modals.rs)

// (Renderers moved to tui/render/modals.rs)

/// Generic centered list picker (model/theme/agent/project/session).
fn render_list_picker(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    title: &str,
    items: &[String],
    selected: usize,
    search: &str,
) {
    let has_search = !search.is_empty();
    let rows = (items.len() as u16)
        .saturating_add(if has_search { 3 } else { 2 })
        .clamp(6, 22);
    let popup = centered_rect(area, 56, rows);
    frame.render_widget(ClearWidget, popup);
    let mut lines: Vec<Line> = Vec::new();

    // Search/filter line at the top so the user sees what they typed.
    lines.push(Line::from(vec![
        Span::styled(
            "› ",
            Style::default()
                .fg(theme::accent())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if search.is_empty() {
                "type to filter…".to_string()
            } else {
                search.to_string()
            },
            if search.is_empty() {
                Style::default().fg(theme::muted())
            } else {
                Style::default().fg(theme::text())
            },
        ),
    ]));

    let inner_h = popup.height.saturating_sub(3) as usize;
    let start = selected
        .saturating_sub(inner_h.saturating_sub(1))
        .min(items.len().saturating_sub(inner_h));
    if items.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no matches)",
            Style::default().fg(theme::muted()),
        )));
    }
    for (i, item) in items.iter().enumerate().skip(start).take(inner_h) {
        let sel = i == selected;
        let style = if sel {
            Style::default()
                .fg(theme::on_accent())
                .bg(theme::accent())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::text())
        };
        let marker = if sel { "› " } else { "  " };
        lines.push(Line::from(Span::styled(
            format!(
                "{marker}{}",
                truncate_chars(item, popup.width.saturating_sub(3) as usize)
            ),
            style,
        )));
    }
    frame.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().bg(theme::surface()))
            .block(popup_block(title)),
        popup,
    );
}

fn model_picker_labels(g: &TuiSessionState) -> Vec<String> {
    let filter = g.model_picker_search.to_ascii_lowercase();
    g.model_picker_entries
        .iter()
        .filter(|e| {
            !e.is_header
                && (filter.is_empty()
                    || e.label.to_ascii_lowercase().contains(&filter)
                    || e.detail.to_ascii_lowercase().contains(&filter))
        })
        .map(|e| {
            if e.detail.is_empty() {
                e.label.clone()
            } else {
                format!("{}  ·  {}", e.label, e.detail)
            }
        })
        .collect()
}

fn agent_picker_labels() -> Vec<String> {
    vec![
        "@build   — full-access development".into(),
        "@plan    — read-only analysis".into(),
        "@review  — focused code review".into(),
        "@fix     — bug diagnosis & fixes".into(),
        "@test    — testing & validation".into(),
    ]
}

fn project_picker_labels(g: &TuiSessionState) -> Vec<String> {
    g.connected_projects
        .iter()
        .map(|p| {
            let dot = if p.active { "● " } else { "○ " };
            format!("{dot}{}  ·  {}", p.name, p.path.display())
        })
        .collect()
}

fn session_picker_labels(g: &TuiSessionState) -> Vec<String> {
    let filter = g.session_picker_search.to_ascii_lowercase();
    g.session_picker_entries
        .iter()
        .filter(|e| filter.is_empty() || e.search_text.to_ascii_lowercase().contains(&filter))
        .map(|e| e.label.clone())
        .collect()
}

// (Renderers moved to tui/render/modals.rs)

// (Renderers moved to tui/render/modals.rs)

// (Renderers moved to tui/render/modals.rs)

/// Full-screen transcript overlay (Codex `/transcript`). Renders ALL blocks at
/// the current terminal width — so it expands/collapses and reflows freely,
/// unlike the frozen native scrollback. Scrollable; supports a raw copy mode.
/// Enters the alternate screen, runs its own input loop, then restores.
pub(crate) fn run_transcript_overlay(state: &Arc<Mutex<TuiSessionState>>) -> anyhow::Result<()> {
    use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen};
    let mut out = stdout();
    execute!(out, EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout());
    let mut term = ratatui::Terminal::new(backend)?;

    let res: anyhow::Result<()> = (|| {
        loop {
            let size = term.size()?;
            let width = size.width.saturating_sub(2).max(20);
            let view_h = size.height.saturating_sub(2).max(1) as usize;

            // Render all blocks at the current width (reflows on resize).
            let (mut lines, raw, msg_count) = {
                let g = state.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
                if !g.transcript_overlay_open {
                    return Ok(());
                }
                let n = g.blocks.len();
                let mut ls = crate::tui::transcript::render_blocks_range(&g, 0, n, width);
                if g.transcript_overlay_raw {
                    // Flatten to plain text for copy-friendly selection.
                    ls = ls
                        .into_iter()
                        .map(|l| {
                            let text: String = l.spans.iter().map(|s| s.content.as_ref()).collect();
                            Line::from(text)
                        })
                        .collect();
                }
                (ls, g.transcript_overlay_raw, n)
            };

            let total = lines.len();
            let max_scroll = total.saturating_sub(view_h);
            let scroll = {
                let mut g = state.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
                g.transcript_overlay_scroll = g.transcript_overlay_scroll.min(max_scroll);
                g.transcript_overlay_scroll
            };
            let end = (scroll + view_h).min(total);
            let visible: Vec<Line> = if scroll < end {
                lines.drain(scroll..end).collect()
            } else {
                Vec::new()
            };

            term.draw(|frame| {
                let area = frame.area();
                if !raw {
                    frame.render_widget(
                        Block::default().style(Style::default().bg(theme::bg())),
                        area,
                    );
                }
                let rows = Layout::vertical([
                    Constraint::Length(1),
                    Constraint::Min(1),
                    Constraint::Length(1),
                ])
                .split(area);

                // Header
                let header = Line::from(vec![
                    Span::styled(
                        " transcript ",
                        Style::default()
                            .fg(theme::on_accent())
                            .bg(theme::accent())
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {msg_count} blocks"),
                        Style::default().fg(theme::muted()),
                    ),
                ]);
                frame.render_widget(
                    Paragraph::new(header).style(Style::default().bg(theme::surface())),
                    rows[0],
                );

                frame.render_widget(
                    Paragraph::new(Text::from(visible))
                        .block(Block::default().padding(Padding::horizontal(1))),
                    rows[1],
                );

                // Footer with keybinds + scroll position.
                let pct = (scroll * 100).checked_div(max_scroll).unwrap_or(100).min(100);
                let footer = Line::from(vec![
                    Span::styled(
                        " ↑↓/PgUp/PgDn scroll · t thinking · o tools · r raw · g/G top/bottom · q close ",
                        Style::default().fg(theme::muted()),
                    ),
                    Span::styled(
                        format!("  {pct}%"),
                        Style::default().fg(theme::muted()),
                    ),
                ]);
                frame.render_widget(
                    Paragraph::new(footer).style(Style::default().bg(theme::surface())),
                    rows[2],
                );
            })?;

            if poll(Duration::from_millis(200))?
                && let Event::Key(key) = read()?
                && !matches!(key.kind, KeyEventKind::Release)
            {
                let mut g = state.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
                match (key.code, key.modifiers) {
                    (KeyCode::Esc, _)
                    | (KeyCode::Char('q'), _)
                    | (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                        g.transcript_overlay_open = false;
                        return Ok(());
                    }
                    (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                        g.transcript_overlay_scroll = g.transcript_overlay_scroll.saturating_sub(1);
                    }
                    (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                        g.transcript_overlay_scroll =
                            (g.transcript_overlay_scroll + 1).min(max_scroll);
                    }
                    (KeyCode::PageUp, _) => {
                        g.transcript_overlay_scroll =
                            g.transcript_overlay_scroll.saturating_sub(view_h);
                    }
                    (KeyCode::PageDown, _) | (KeyCode::Char(' '), _) => {
                        g.transcript_overlay_scroll =
                            (g.transcript_overlay_scroll + view_h).min(max_scroll);
                    }
                    (KeyCode::Home, _) | (KeyCode::Char('g'), KeyModifiers::NONE) => {
                        g.transcript_overlay_scroll = 0;
                    }
                    (KeyCode::End, _) | (KeyCode::Char('G'), _) => {
                        g.transcript_overlay_scroll = max_scroll;
                    }
                    (KeyCode::Char('t'), KeyModifiers::NONE) => {
                        g.thinking_expanded = !g.thinking_expanded;
                    }
                    (KeyCode::Char('o'), KeyModifiers::NONE) => {
                        g.toggle_all_tool_blocks();
                    }
                    (KeyCode::Char('r'), KeyModifiers::NONE) => {
                        g.transcript_overlay_raw = !g.transcript_overlay_raw;
                    }
                    _ => {}
                }
            }
        }
    })();

    let _ = execute!(stdout(), LeaveAlternateScreen);
    res
}

/// A key combination: a key code plus active modifiers. Used by the
/// customizable-keybinding layer to match incoming keys against user aliases.
pub type KeyCombo = (KeyCode, KeyModifiers);

/// Parse a key string like `"ctrl+b"`, `"alt+enter"`, or `"f5"` into a
/// [`KeyCombo`]. Returns `None` for unrecognized syntax.
pub fn parse_key_combo(s: &str) -> Option<KeyCombo> {
    let parts: Vec<&str> = s
        .split('+')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        return None;
    }
    let (mod_parts, key_part) = parts.split_at(parts.len() - 1);
    let mut mods = KeyModifiers::NONE;
    for m in mod_parts {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
            "alt" | "option" | "meta" => mods |= KeyModifiers::ALT,
            "shift" => mods |= KeyModifiers::SHIFT,
            _ => return None,
        }
    }
    let k = key_part[0].to_ascii_lowercase();
    let code = match k.as_str() {
        "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "esc" | "escape" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        s if s.chars().count() == 1 => KeyCode::Char(s.chars().next()?),
        s if s.len() >= 2 && s.starts_with('f') => KeyCode::F(s[1..].parse::<u8>().ok()?),
        _ => return None,
    };
    Some((code, mods))
}

/// Default key for a remappable global action, or `None` for unknown actions.
pub fn default_action_key(action: &str) -> Option<KeyCombo> {
    use KeyCode::Char;
    Some(match action.to_ascii_lowercase().as_str() {
        "palette" => (Char('p'), KeyModifiers::CONTROL),
        "search" | "transcript_search" => (Char('f'), KeyModifiers::CONTROL),
        "history" => (Char('r'), KeyModifiers::CONTROL),
        "pin" => (Char('k'), KeyModifiers::CONTROL),
        "expand" => (Char('o'), KeyModifiers::CONTROL),
        "subagents" | "subagent" => (Char('g'), KeyModifiers::CONTROL),
        "thinking" => (Char('t'), KeyModifiers::CONTROL),
        "clear" => (Char('l'), KeyModifiers::CONTROL),
        _ => return None,
    })
}

/// Global actions whose keys can be remapped via `/keymap`.
const REMAPPABLE_ACTIONS: &[&str] = &[
    "palette",
    "search",
    "history",
    "pin",
    "expand",
    "subagents",
    "thinking",
    "clear",
];

/// A remappable action's built-in default key and its effective (possibly
/// user-overridden) key.
#[derive(Clone, Copy)]
pub struct KeyBinding {
    pub default: KeyCombo,
    pub effective: KeyCombo,
}

/// Resolve config key overrides into per-action bindings. Returns empty when no
/// overrides are configured, so key handling is completely untouched in the
/// common case.
pub fn build_key_bindings(keymap: &std::collections::BTreeMap<String, String>) -> Vec<KeyBinding> {
    if keymap.is_empty() {
        return Vec::new();
    }
    REMAPPABLE_ACTIONS
        .iter()
        .filter_map(|action| {
            let default = default_action_key(action)?;
            let effective = keymap
                .get(*action)
                .and_then(|s| parse_key_combo(s))
                .unwrap_or(default);
            Some(KeyBinding { default, effective })
        })
        .collect()
}

/// Translate an incoming key through the user's remap:
/// - if it's an action's *effective* key, rewrite it to that action's default
///   key so the (unchanged) key handler fires the action;
/// - if it's the default key of an action that was remapped *away*, suppress it
///   (`None`) so the old default no longer triggers the action;
/// - otherwise pass it through unchanged.
fn translate_key(key: KeyEvent, bindings: &[KeyBinding]) -> Option<KeyEvent> {
    let pressed: KeyCombo = (key.code, key.modifiers);
    for b in bindings {
        if b.effective == pressed {
            return Some(KeyEvent::new(b.default.0, b.default.1));
        }
    }
    for b in bindings {
        if b.default == pressed && b.effective != pressed {
            return None;
        }
    }
    Some(key)
}

/// Parse user approval input (flexible: punctuation, synonyms, `/approve` style).
/// `question_answer_tx`: when `Some`, answers are sent there so they unblock `ask_question` while
/// the async loop is stuck in `run_turn` (that task does not poll `cmd_rx` until the turn ends).
/// `mouse_capture`: retained for API compatibility; fullscreen TUI always enables mouse capture.
/// `scroll_speed`: lines per scroll event.
#[allow(clippy::too_many_arguments)]
pub fn run_blocking(
    state: Arc<Mutex<TuiSessionState>>,
    cmd_tx: UnboundedSender<TuiCmd>,
    question_answer_tx: Option<UnboundedSender<(String, QuestionSelection)>>,
    approval_answer_tx: Option<UnboundedSender<ApprovalAnswer>>,
    show_run_banner: bool,
    cancel_flag: Option<Arc<std::sync::atomic::AtomicBool>>,
    _mouse_capture: bool,
    scroll_speed: u16,
) -> anyhow::Result<()> {
    let mouse_capture = true;
    let mut terminal = setup_terminal(mouse_capture)?;

    // Set terminal window title for tab identification.
    {
        let title = if let Ok(g) = state.lock() {
            let ws = g
                .workspace_root
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("workspace");
            format!("dcode-ai: {ws}")
        } else {
            "dcode-ai".to_string()
        };
        let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::SetTitle(&title));
    }

    // Load slash entries once: hardcoded commands + discovered skills
    let skill_dirs = vec![PathBuf::from(".dcode-ai/skills")];
    let workspace_root = {
        let g = state.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        g.workspace_root.clone()
    };
    let slash_entries = load_slash_entries(&workspace_root, &skill_dirs);
    // Discover workspace files in a background thread so the TUI is responsive
    // immediately on large repos. Completion uses an empty list until ready.
    let workspace_files_rx = {
        let root = workspace_root.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let files = file_mentions::discover_workspace_files(&root);
            let _ = tx.send(files);
        });
        rx
    };
    let mut workspace_files: Vec<String> = Vec::new();
    let mut transcript_cache = TranscriptRenderCache::default();
    let mut scroll_buffer = crate::tui::scroll_buffer::ScrollBuffer::default();
    let mut composer_history: Vec<String> = Vec::new();
    let mut composer_history_index: Option<usize> = None;
    let mut composer_history_draft = String::new();
    let mut ctrl_c_armed_at: Option<std::time::Instant> = None;
    // Debounce terminal resizes: reflow once after the drag settles (~120ms of
    // quiet) instead of purge+reflushing on every intermediate resize event.
    let mut pending_resize_at: Option<std::time::Instant> = None;
    // Last terminal width we reflowed at. A resize only needs a full purge +
    // re-flush when the *width* changes (wrapped scrollback becomes stale);
    // height-only changes keep the same wrapping, so we skip the flicker.
    let mut last_term_width: u16 = crossterm::terminal::size().map(|(w, _)| w).unwrap_or(80);
    // Desktop-notification state: emit an OSC 9 notice + bell when a turn
    // finishes while the terminal is unfocused (Codex parity). Focus defaults to
    // true; terminals without focus reporting simply never suppress.
    let mut terminal_focused = true;
    let mut was_busy = false;
    let mut had_active_approval = false;

    // Flush the Antigravity-style startup banner once into the terminal's
    // native scrollback (it scrolls away as the conversation grows). Shown on
    // fresh sessions and explicit run mode; skipped when resuming.
    {
        let (model, ws_path, show) = if let Ok(g) = state.lock() {
            let ws = if g.workspace_display.is_empty() {
                g.workspace_root.display().to_string()
            } else {
                g.workspace_display.clone()
            };
            (g.model.clone(), ws, show_run_banner || g.blocks.is_empty())
        } else {
            (String::new(), String::new(), show_run_banner)
        };
        if show {
            let term_w = crossterm::terminal::size().map(|(w, _)| w).unwrap_or(80);
            let blines = banner_lines(&model, &ws_path, term_w);
            let n = blines.len() as u16;
            let _ = terminal.insert_before(n, |buf| {
                let area = buf.area;
                Block::default()
                    .style(Style::default().bg(theme::bg()))
                    .render(area, buf);
                Paragraph::new(Text::from(blines)).render(area, buf);
            });
        }
    }

    loop {
        // Pick up async workspace file discovery when it completes.
        if workspace_files.is_empty()
            && let Ok(files) = workspace_files_rx.try_recv()
        {
            workspace_files = files;
        }

        // Debounced resize: once the drag has been quiet for ~120ms, do a single
        // reflow (purge + re-flush at the new width) instead of one per event.
        if let Some(t) = pending_resize_at
            && t.elapsed() >= Duration::from_millis(120)
        {
            pending_resize_at = None;
            let cur_w = crossterm::terminal::size()
                .map(|(w, _)| w)
                .unwrap_or(last_term_width);
            if let Ok(mut g) = state.lock() {
                if cur_w != last_term_width {
                    // Width changed → wrapped scrollback is stale; full reflow.
                    last_term_width = cur_w;
                    g.request_clear = true;
                }
                // Height-only changes fall through to a normal live-pane repaint
                // (touch_transcript) without purging/re-flushing scrollback.
                g.touch_transcript();
            }
        }

        // Full-screen transcript overlay (Codex `/transcript`). Runs its own
        // alt-screen loop; on return we force a full redraw of the inline pane.
        let overlay_requested = state
            .lock()
            .map(|g| g.transcript_overlay_open)
            .unwrap_or(false);
        if overlay_requested {
            let _ = run_transcript_overlay(&state);
            let _ = terminal.clear();
            continue;
        }

        {
            let mut g = state.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
            if g.should_exit {
                break;
            }

            // Turn-completion desktop notification: fire once on the busy→idle
            // edge, only when the terminal is unfocused (Codex parity). OSC 9
            // (`ESC ] 9 ; <msg> BEL`) shows a desktop notification on supporting
            // terminals; terminals without focus reporting keep focused=true and
            // simply never trigger it.
            if g.notifications_enabled && !terminal_focused {
                let msg = if was_busy && !g.busy {
                    Some("dcode-ai finished responding")
                } else if !had_active_approval && g.active_approval.is_some() {
                    Some("dcode-ai needs approval")
                } else {
                    None
                };
                if let Some(msg) = msg {
                    use std::io::Write;
                    let mut so = std::io::stdout();
                    // OSC 9 desktop notification + audible bell for terminals
                    // that only support the latter.
                    let _ = write!(so, "\x1b]9;{msg}\x07\x07");
                    let _ = so.flush();
                }
            }
            was_busy = g.busy;
            had_active_approval = g.active_approval.is_some();

            // Width-drift safety net (Codex's `note_width`/`reflow_needed_for_width`
            // model): the terminal can widen without us receiving a clean
            // `Event::Resize` (coalesced/dropped events, some terminals). When
            // that happens, already-flushed scrollback keeps its old, narrower
            // width and the newly-exposed columns show the terminal wallpaper
            // with text clipped at the old edge. Observing the width every draw
            // and scheduling a debounced reflow whenever it differs from the last
            // reflowed width guarantees stale scrollback gets repainted, even if
            // the resize event never arrived.
            let cur_w = crossterm::terminal::size()
                .map(|(w, _)| w)
                .unwrap_or(last_term_width);
            if cur_w != last_term_width && pending_resize_at.is_none() {
                pending_resize_at = Some(std::time::Instant::now());
            }

            let slash_filtered = filter_slash_entries(&slash_entries, &g.input_buffer);
            let at_matches =
                at_completion_matches(&workspace_files, &g.input_buffer, g.cursor_char_idx);
            let chrome_h = composer_chrome_height(
                &slash_entries,
                &workspace_files,
                &g.input_buffer,
                g.cursor_char_idx,
            );
            g.menu_content = menu_content_for_state(&g);

            // ── Handle /clear: purge terminal scrollback + screen ──
            if g.request_clear {
                g.request_clear = false;
                // Row-capped reflow (Codex parity): when re-emitting kept history
                // after a width resize / thinking toggle, restore only the last
                // REFLOW_MAX_ROWS worth of blocks to scrollback so an enormous
                // transcript doesn't stall the reflush. Older blocks stay in
                // memory (transcript overlay) but drop out of scrollback. When
                // blocks were cleared (/clear) this resolves to 0.
                const REFLOW_MAX_ROWS: usize = 5000;
                let reflow_w = crossterm::terminal::size().map(|(w, _)| w).unwrap_or(80);
                g.flushed_block_count =
                    crate::tui::transcript::reflow_start_block(&g, reflow_w, REFLOW_MAX_ROWS);
                use crossterm::terminal::{Clear, ClearType};
                let _ = crossterm::execute!(
                    std::io::stdout(),
                    Clear(ClearType::Purge),
                    Clear(ClearType::All),
                    crossterm::cursor::MoveTo(0, 0),
                );
                let _ = terminal.clear();
                // Re-flush the banner so the cleared screen still shows context.
                let (model, ws_path) = {
                    let ws = if g.workspace_display.is_empty() {
                        g.workspace_root.display().to_string()
                    } else {
                        g.workspace_display.clone()
                    };
                    (g.model.clone(), ws)
                };
                let term_w = crossterm::terminal::size().map(|(w, _)| w).unwrap_or(80);
                let blines = banner_lines(&model, &ws_path, term_w);
                let n = blines.len() as u16;
                let _ = terminal.insert_before(n, |buf| {
                    let area = buf.area;
                    Block::default()
                        .style(Style::default().bg(theme::bg()))
                        .render(area, buf);
                    Paragraph::new(Text::from(blines)).render(area, buf);
                });
            }

            // ── Flush completed blocks into the terminal's native scrollback ──
            // (Codex model). Stable blocks scroll up out of the live viewport
            // and become permanent terminal history. Tool calls still running
            // and pending approvals stay in the live pane until they finalize.
            {
                // If blocks shrank (a /clear or /new happened), reset the flush
                // cursor so new content renders and flushes from the start.
                if g.flushed_block_count > g.blocks.len() {
                    g.flushed_block_count = 0;
                }
                let flushed = g.flushed_block_count;
                let mut flush_target = g.blocks.len();
                for (i, b) in g.blocks.iter().enumerate().skip(flushed) {
                    if matches!(
                        b,
                        DisplayBlock::ToolRunning { .. } | DisplayBlock::ApprovalPending(_)
                    ) {
                        flush_target = i;
                        break;
                    }
                }
                if flush_target > flushed {
                    let term_w = crossterm::terminal::size().map(|(w, _)| w).unwrap_or(80);
                    let flush_lines = crate::tui::transcript::render_blocks_range(
                        &g,
                        flushed,
                        flush_target,
                        term_w,
                    );
                    let n = flush_lines.len() as u16;
                    if n > 0 {
                        let _ = terminal.insert_before(n, |buf| {
                            let area = buf.area;
                            // Fill with solid theme bg so terminal wallpaper
                            // doesn't bleed into scrollback history.
                            Block::default()
                                .style(Style::default().bg(theme::bg()))
                                .render(area, buf);
                            Paragraph::new(Text::from(flush_lines)).render(area, buf);
                        });
                    }
                    g.flushed_block_count = flush_target;
                    transcript_cache.invalidate();
                }
            }

            terminal.draw(|frame| {
                let area = frame.area();

                // Clear the whole viewport each frame so moving content (the
                // input box shifts as live content grows/shrinks) never leaves
                // ghost copies behind. `Clear` resets cells to default — it
                // wipes stale rows without painting a dark block over the
                // terminal wallpaper. Content below paints its own background.
                frame.render_widget(ClearWidget, area);

                // ── Layout: transcript · separator · input ──
                // Tight input height: 1 line, growing with multiline content
                // (no border chrome — clean Codex look).
                let input_h = {
                    // Use the SAME char-wrap as the render so the box height
                    // exactly fits the wrapped input (no clipped cursor row).
                    let inner_w = area.width.saturating_sub(4).max(1) as usize;
                    let line = composer_line(&g.input_buffer, g.cursor_char_idx);
                    let rows = wrap_composer_line(&line, inner_w).len();
                    (rows.max(1) as u16).min(8)
                };
                let slash_h = if slash_panel_visible(&g.input_buffer) && !slash_filtered.is_empty()
                {
                    slash_panel_height(slash_filtered.len())
                } else if !at_matches.is_empty() {
                    (at_matches.len().min(8) as u16) + 2
                } else {
                    0
                };
                let _ = chrome_h;

                // ── Live content (unflushed blocks + streaming) ──
                // Follows the tail by default; respects user scroll position
                // when `transcript_follow_tail` is false.
                let inner_w = area.width.saturating_sub(2).max(10);
                let (lines, _hits, _rebuilt) = transcript_cache.get_or_rebuild(&g, inner_w);

                // Input box height = input lines + 2 border rows.
                let box_h = input_h + 2;
                // Rows available for live content = viewport − spacer − slash
                // − input box − status.
                let avail_for_live = area.height.saturating_sub(1 + slash_h + box_h + 1) as usize;
                let total = lines.len();
                // Top-align when it fits; show the tail when at default scroll
                // state, or preserve user's manual scroll position.
                let start = if g.transcript_follow_tail {
                    total.saturating_sub(avail_for_live)
                } else {
                    g.scroll_lines.min(total.saturating_sub(avail_for_live))
                };
                g.scroll_lines = start;
                let visible: Vec<Line> = lines[start..].to_vec();
                let live_h = visible.len() as u16;

                // Contiguous block from the top:
                // [live][spacer][slash][input-box][status][bg].
                let chunks = Layout::vertical([
                    Constraint::Length(live_h),
                    Constraint::Length(1),
                    Constraint::Length(slash_h),
                    Constraint::Length(box_h),
                    Constraint::Length(1),
                    Constraint::Min(0),
                ])
                .split(area);
                let tr = chunks[0];
                // chunks[1] = blank spacer (breathing room)
                let panel_r = chunks[2];
                let inp_r = chunks[3];
                let status_r = chunks[4];

                if !visible.is_empty() {
                    frame.render_widget(
                        Paragraph::new(Text::from(visible))
                            .style(Style::default().bg(theme::bg()))
                            .block(Block::default().padding(Padding::horizontal(1))),
                        tr,
                    );
                }

                // ── Slash command panel / @ mention panel ──
                if panel_r.height > 0 {
                    if slash_panel_visible(&g.input_buffer) && !slash_filtered.is_empty() {
                        render_slash_panel(frame, panel_r, &slash_filtered, g.slash_menu_index);
                    } else if !at_matches.is_empty() {
                        render_at_panel(frame, panel_r, &at_matches, g.at_menu_index);
                    }
                }

                // ── Input box (rounded border) ──
                let border_color = if toolbar_permission_is_bypass(&g.permission_mode) {
                    theme::error()
                } else {
                    theme::border()
                };
                let input_line = composer_line(&g.input_buffer, g.cursor_char_idx);
                // Char-wrap at the box's inner width (borders 2 + h-padding 2 = 4)
                // so long unbroken tokens wrap instead of overflowing the box.
                // This matches the height math (`input_h`, which counts wrapped
                // rows) so the box grows to fit. We pre-wrap rather than using
                // ratatui's word `Wrap`, which won't break a whitespace-free run.
                let box_inner_w = (inp_r.width as usize).saturating_sub(4).max(1);
                let input_lines = wrap_composer_line(&input_line, box_inner_w);
                // When the wrapped input is taller than the box (capped at
                // `input_h` rows), scroll so the cursor's row stays visible —
                // whether typing at the end or navigating up — like Codex's
                // composer. No scroll when everything fits.
                let visible_rows = input_h as usize;
                let total_rows = input_lines.len();
                let scroll_y = if total_rows <= visible_rows {
                    0
                } else {
                    let cursor_row =
                        composer_cursor_row(&g.input_buffer, g.cursor_char_idx, box_inner_w);
                    let max_scroll = total_rows - visible_rows;
                    // Keep the cursor on the last visible row when it would fall
                    // below the window; clamp to the valid scroll range.
                    cursor_row.saturating_sub(visible_rows - 1).min(max_scroll)
                } as u16;
                frame.render_widget(
                    Paragraph::new(Text::from(input_lines))
                        .scroll((scroll_y, 0))
                        .style(Style::default().bg(theme::bg()))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_type(BorderType::Rounded)
                                .border_style(Style::default().fg(border_color))
                                .padding(Padding::horizontal(1))
                                .style(Style::default().bg(theme::bg())),
                        ),
                    inp_r,
                );
                // The cursor is drawn by `composer_line` as a reverse-video cell,
                // which wraps to the correct line automatically — so we don't set
                // a hardware cursor (it can't track word-wrap and would leave a
                // stray block at the wrong spot on long/wrapped input).

                // ── Status bar: brand · agent · effort · state · context ──
                if status_r.height > 0 {
                    let busy = g.busy || g.current_busy_state.is_active();
                    let state_icon = if busy { busy_spinner(g.started) } else { "●" };
                    let state_label = if busy { "working" } else { "idle" };
                    let state_color = if busy {
                        theme::warn()
                    } else {
                        theme::success()
                    };
                    let agent = if g.agent_profile.is_empty() {
                        "@build".to_string()
                    } else {
                        g.agent_profile.clone()
                    };
                    let effort = if g.thinking_enabled {
                        if g.thinking_budget >= 32768 {
                            "xhigh"
                        } else {
                            "high"
                        }
                    } else {
                        "medium"
                    };
                    let ctx_pct = {
                        let win =
                            dcode_ai_runtime::model_limits::detect_context_window(&g.model) as u64;
                        if win > 0 {
                            ((g.context_tokens.min(win) as f64 / win as f64) * 100.0).round() as u64
                        } else {
                            0
                        }
                    };
                    let sep =
                        Span::styled(" · ", Style::default().fg(theme::border()).bg(theme::bg()));
                    let mut left = vec![Span::styled(
                        format!("{state_icon} "),
                        Style::default().fg(state_color).bg(theme::bg()),
                    )];
                    // Shimmer the label while working; plain when idle.
                    if busy {
                        let elapsed = g.busy_state_since.elapsed().as_millis();
                        left.extend(crate::tui::shimmer::shimmer_spans(
                            state_label,
                            elapsed,
                            theme::muted(),
                            theme::warn(),
                            theme::bg(),
                        ));
                    } else {
                        left.push(Span::styled(
                            state_label,
                            Style::default().fg(state_color).bg(theme::bg()),
                        ));
                    }
                    // State + brand always show; the rest are user-toggleable
                    // via `/statusline` (keys persisted in config).
                    let hidden = |k: &str| g.statusline_hidden.iter().any(|h| h == k);
                    left.push(sep.clone());
                    left.push(Span::styled(
                        "dcode-ai",
                        Style::default()
                            .fg(theme::accent())
                            .bg(theme::bg())
                            .add_modifier(Modifier::BOLD),
                    ));
                    if !hidden("agent") {
                        left.push(sep.clone());
                        left.push(Span::styled(
                            agent,
                            Style::default().fg(theme::assistant()).bg(theme::bg()),
                        ));
                    }
                    if !hidden("effort") {
                        left.push(sep.clone());
                        left.push(Span::styled(
                            effort.to_string(),
                            Style::default().fg(theme::warn()).bg(theme::bg()),
                        ));
                    }
                    if !hidden("time") {
                        left.push(sep.clone());
                        left.push(Span::styled(
                            session_time(g.started),
                            Style::default().fg(theme::muted()).bg(theme::bg()),
                        ));
                    }
                    if ctx_pct > 0 && !hidden("context") {
                        left.push(sep.clone());
                        left.push(Span::styled(
                            format!("ctx {ctx_pct}%"),
                            Style::default().fg(theme::muted()).bg(theme::bg()),
                        ));
                    }
                    if g.mcp_server_count > 0 {
                        let ready_count = g
                            .mcp_server_statuses
                            .values()
                            .filter(|s| {
                                matches!(s, dcode_ai_common::event::McpStartupStatus::Ready)
                            })
                            .count();
                        left.push(sep.clone());
                        left.push(Span::styled(
                            format!("mcp {}/{}", ready_count, g.mcp_server_count),
                            Style::default()
                                .fg(if ready_count == g.mcp_server_count {
                                    theme::success()
                                } else {
                                    theme::warn()
                                })
                                .bg(theme::bg()),
                        ));
                    }
                    let right = if hidden("model") {
                        "? help".to_string()
                    } else {
                        format!("{}  ? help", g.model)
                    };
                    let lw: usize = left.iter().map(|s| s.content.chars().count()).sum();
                    let rw = right.chars().count();
                    let total_w = status_r.width.saturating_sub(2) as usize;
                    let gap = total_w.saturating_sub(lw + rw);
                    left.push(Span::styled(
                        " ".repeat(gap),
                        Style::default().bg(theme::bg()),
                    ));
                    left.push(Span::styled(
                        right,
                        Style::default().fg(theme::muted()).bg(theme::bg()),
                    ));
                    frame.render_widget(
                        Paragraph::new(Line::from(left))
                            .style(Style::default().bg(theme::bg()))
                            .block(Block::default().padding(Padding::horizontal(1))),
                        status_r,
                    );
                }

                // ── Centered popups ──
                if g.api_key_modal_open {
                    render_api_key_modal(frame, area, &g);
                } else if g.connect_modal_open {
                    render_connect_modal(frame, area, &g);
                } else if g.command_palette_open {
                    render_command_palette(frame, area, &g);
                } else if g.model_picker_open {
                    render_list_picker(
                        frame,
                        area,
                        "model",
                        &model_picker_labels(&g),
                        g.model_picker_index,
                        &g.model_picker_search,
                    );
                } else if g.theme_picker_open {
                    render_list_picker(
                        frame,
                        area,
                        "theme",
                        &g.theme_picker_entries,
                        g.theme_picker_index,
                        "",
                    );
                } else if g.agent_picker_open {
                    render_list_picker(
                        frame,
                        area,
                        "agent",
                        &agent_picker_labels(),
                        g.agent_picker_index,
                        "",
                    );
                } else if g.project_picker_open {
                    render_list_picker(
                        frame,
                        area,
                        "project",
                        &project_picker_labels(&g),
                        g.project_picker_index,
                        "",
                    );
                } else if g.session_picker_open {
                    render_list_picker(
                        frame,
                        area,
                        "session",
                        &session_picker_labels(&g),
                        g.session_picker_index,
                        &g.session_picker_search,
                    );
                } else if g.backtrack_open {
                    render_list_picker(
                        frame,
                        area,
                        "backtrack — Enter edits & rewinds",
                        &g.backtrack_labels(),
                        g.backtrack_index,
                        "",
                    );
                } else if g.info_modal_open {
                    render_info_modal(frame, area, &g);
                } else if g.question_modal_open && g.active_question.is_some() {
                    render_question_modal(frame, area, &g);
                } else if let Some(req) = g.active_approval.clone() {
                    render_approval_popup(
                        frame,
                        area,
                        &req,
                        g.approval_option_index,
                        g.approval_hunk_mode,
                        &g.approval_hunk_selection,
                        g.approval_hunk_cursor,
                    );
                }

                if g.toast.as_ref().is_some_and(|t| t.is_expired()) {
                    g.toast = None;
                }

                g.branch_chip_bounds = None;
                g.sidebar_toggle_bounds = None;
            })?;
        }

        let poll_timeout = if pending_resize_at.is_some() {
            // Tick fast while a resize is settling so the debounced reflow fires
            // promptly after the drag stops.
            Duration::from_millis(50)
        } else if let Ok(g) = state.lock() {
            if g.busy || g.current_busy_state.is_active() {
                Duration::from_millis(40)
            } else if g.connect_modal_open {
                // Tick fast enough to drive the modal's animation.
                Duration::from_millis(80)
            } else {
                Duration::from_millis(120)
            }
        } else {
            Duration::from_millis(80)
        };

        if poll(poll_timeout)? {
            let ev = match read().ok() {
                Some(ev) => ev,
                None => continue,
            };
            let mut g = match state.lock() {
                Ok(g) => g,
                Err(_) => continue,
            };

            match ev {
                Event::Mouse(_) if g.command_palette_open => continue,
                Event::Mouse(_) if g.info_modal_open => continue,
                Event::Mouse(_) if g.model_picker_open => continue,
                Event::Mouse(m) if g.connect_modal_open => {
                    let rows = build_connect_rows(&g.connect_search);
                    let n_sel = selectable_row_indices(&rows).len();
                    if n_sel > 0 {
                        match m.kind {
                            MouseEventKind::ScrollUp => {
                                g.connect_modal_ignore_enter_once = false;
                                g.connect_menu_index =
                                    g.connect_menu_index.saturating_sub(1).min(n_sel - 1);
                            }
                            MouseEventKind::ScrollDown => {
                                g.connect_modal_ignore_enter_once = false;
                                g.connect_menu_index = (g.connect_menu_index + 1).min(n_sel - 1);
                            }
                            _ => {}
                        }
                    }
                    continue;
                }
                Event::Mouse(_) if g.api_key_modal_open => continue,
                Event::Mouse(_) if g.anthropic_oauth_modal_open => continue,
                Event::Mouse(_) if g.provider_picker_open => continue,
                Event::Mouse(_) if g.permission_picker_open => continue,
                Event::Mouse(_) if g.agent_picker_open => continue,
                Event::Mouse(_) if g.theme_picker_open => continue,
                Event::Mouse(_) if g.project_picker_open => continue,
                Event::Mouse(_) if g.session_picker_open => continue,
                Event::Mouse(_) if g.backtrack_open => continue,
                Event::Mouse(_) if g.question_modal_open => continue,
                Event::Mouse(_) if g.pins_modal_open => continue,
                Event::Mouse(_) if g.subagent_modal_open => continue,
                Event::Mouse(_) if g.transcript_search_open => continue,
                Event::Mouse(m) => {
                    let sz = match terminal.size().ok() {
                        Some(sz) => sz,
                        None => continue,
                    };
                    let area = Rect::new(0, 0, sz.width, sz.height);
                    let (main_area, _) = layout_with_sidebar(area, g.sidebar_open);
                    let slash_filtered = filter_slash_entries(&slash_entries, &g.input_buffer);
                    let at_matches =
                        at_completion_matches(&workspace_files, &g.input_buffer, g.cursor_char_idx);
                    let sh = composer_chrome_height(
                        &slash_entries,
                        &workspace_files,
                        &g.input_buffer,
                        g.cursor_char_idx,
                    );
                    let input_h = if should_hide_composer_when_scrolling(&g) {
                        0
                    } else {
                        composer_input_height(&g, main_area.width)
                    };
                    let (tr, _, slash_r, _) = layout_chunks(
                        main_area,
                        sh,
                        input_h,
                        g.queue_preview_items.len(),
                        g.subagents.len(),
                    );

                    // Mouse scroll works anywhere on the screen, not just inside the transcript.
                    if matches!(
                        m.kind,
                        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                    ) {
                        let inner_w = tr.width.saturating_sub(2);
                        let (lines, _hits, rebuilt) = transcript_cache.get_or_rebuild(&g, inner_w);
                        let th = tr.height.saturating_sub(2) as usize;
                        if rebuilt {
                            scroll_buffer.replace_lines(lines.to_vec());
                        }
                        scroll_buffer.set_from_top(g.scroll_lines, th, inner_w as usize);
                        let step = mouse_scroll_step(m.modifiers, th, scroll_speed as usize);
                        // Scrolling clears any active mouse text selection.
                        g.mouse_selection = None;
                        match m.kind {
                            MouseEventKind::ScrollUp => {
                                scroll_buffer.scroll_up(step, inner_w as usize, th);
                            }
                            MouseEventKind::ScrollDown => {
                                scroll_buffer.scroll_down(step);
                            }
                            _ => {}
                        }
                        let (from_top, _) =
                            scroll_buffer.scroll_position_from_top(th, inner_w as usize);
                        g.scroll_lines = from_top as usize;
                        g.transcript_follow_tail = scroll_buffer.is_sticky();
                        continue;
                    }

                    // ── Mouse text selection within transcript ──────────────
                    // Down(Left) anchors a selection; Drag(Left) extends it;
                    // Up(Left) with a non-empty drag copies to clipboard.
                    // This stays active even if drag/release leaves the panel.
                    let inner_top = tr.y.saturating_add(1);
                    let inner_left = tr.x.saturating_add(1);
                    let inner_w = tr.width.saturating_sub(2) as usize;
                    let inner_h = tr.height.saturating_sub(2) as usize;
                    let in_transcript_inner = m.row >= inner_top
                        && (m.row as usize) < inner_top as usize + inner_h
                        && m.column >= inner_left
                        && (m.column as usize) < inner_left as usize + inner_w;
                    g.history_rect = Some((tr.x, tr.y, tr.width, tr.height));

                    match m.kind {
                        MouseEventKind::Down(MouseButton::Left) if in_transcript_inner => {
                            let row_in_area = (m.row - inner_top) as usize;
                            let col_in_area = (m.column - inner_left) as usize;
                            let buf_row =
                                (g.scroll_lines + row_in_area).min(u16::MAX as usize) as u16;
                            g.mouse_selection = Some(crate::tui::mouse_select::Selection {
                                anchor: crate::tui::mouse_select::VisualPos {
                                    row: buf_row,
                                    col: col_in_area.min(u16::MAX as usize) as u16,
                                },
                                cursor: crate::tui::mouse_select::VisualPos {
                                    row: buf_row,
                                    col: col_in_area.min(u16::MAX as usize) as u16,
                                },
                                scroll_from_top: g.scroll_lines.min(u16::MAX as usize) as u16,
                            });
                        }
                        MouseEventKind::Drag(MouseButton::Left) => {
                            if g.mouse_selection.is_some() && inner_h > 0 {
                                let inner_w_eff = inner_w.max(1);
                                let (lines, _hits, rebuilt) =
                                    transcript_cache.get_or_rebuild(&g, inner_w_eff as u16);
                                if rebuilt {
                                    scroll_buffer.replace_lines(lines.to_vec());
                                }
                                scroll_buffer.set_from_top(g.scroll_lines, inner_h, inner_w_eff);
                                if m.row < inner_top {
                                    scroll_buffer.scroll_up(1, inner_w_eff, inner_h);
                                } else if (m.row as usize) >= inner_top as usize + inner_h {
                                    scroll_buffer.scroll_down(1);
                                }
                                let (from_top, _) =
                                    scroll_buffer.scroll_position_from_top(inner_h, inner_w_eff);
                                g.scroll_lines = from_top as usize;
                                g.transcript_follow_tail = scroll_buffer.is_sticky();

                                let clamped_row = if m.row < inner_top {
                                    0usize
                                } else if (m.row as usize) >= inner_top as usize + inner_h {
                                    inner_h.saturating_sub(1)
                                } else {
                                    (m.row - inner_top) as usize
                                };
                                let clamped_col = if m.column < inner_left {
                                    0usize
                                } else if (m.column as usize) >= inner_left as usize + inner_w_eff {
                                    inner_w_eff.saturating_sub(1)
                                } else {
                                    (m.column - inner_left) as usize
                                };

                                let scroll_from_top = g.scroll_lines.min(u16::MAX as usize) as u16;
                                if let Some(ref mut sel) = g.mouse_selection {
                                    sel.scroll_from_top = scroll_from_top;
                                    let buf_row = (sel.scroll_from_top as usize + clamped_row)
                                        .min(u16::MAX as usize)
                                        as u16;
                                    sel.cursor = crate::tui::mouse_select::VisualPos {
                                        row: buf_row,
                                        col: clamped_col.min(u16::MAX as usize) as u16,
                                    };
                                }
                            }
                        }
                        MouseEventKind::Up(MouseButton::Left) => {
                            if let Some(sel) = g.mouse_selection.take()
                                && sel.anchor != sel.cursor
                                && !is_click_jitter(&sel)
                            {
                                let inner_w_eff = inner_w.max(1);
                                let (lines, _hits, _) =
                                    transcript_cache.get_or_rebuild(&g, inner_w_eff as u16);
                                let gutter_widths = vec![0u16; lines.len()];
                                let (rows, gutters) =
                                    crate::tui::mouse_select::build_all_visual_rows(
                                        lines,
                                        &gutter_widths,
                                        inner_w_eff,
                                    );
                                let text = crate::tui::mouse_select::extract_selected_text(
                                    &rows, &gutters, &sel,
                                );
                                if !text.is_empty() {
                                    match crate::tui::mouse_select::copy_to_clipboard(&text) {
                                        Ok(_msg) => {
                                            g.show_toast(
                                                "✓ Copied to clipboard",
                                                crate::tui::state::ToastKind::Success,
                                            );
                                        }
                                        Err(e) => {
                                            g.push_error(format!("Clipboard copy failed: {e}"));
                                        }
                                    }
                                    // Meaningful drag copy consumed the click.
                                    continue;
                                }
                            }
                            // Pure click (no drag): let click-hit logic run.
                        }
                        _ => {}
                    }

                    // Transcript click targets (left-click on question options / code blocks).
                    if rect_contains(tr, m.column, m.row) {
                        let inner_w = tr.width.saturating_sub(2);
                        let (_lines, hits, _) = transcript_cache.get_or_rebuild(&g, inner_w);
                        let th = tr.height.saturating_sub(2) as usize;
                        match m.kind {
                            k if mouse_left_activated(k) => {
                                // Inner content starts below top border (y+1).
                                let inner_top = tr.y.saturating_add(1);
                                if m.row >= inner_top {
                                    let row_in_area = (m.row - inner_top) as usize;
                                    if row_in_area < th {
                                        let gline = g.scroll_lines + row_in_area;
                                        let picked = if gline < hits.len() {
                                            hits[gline].clone()
                                        } else {
                                            None
                                        };
                                        if let Some(hit) = picked {
                                            match hit {
                                                LineClickHit::Question(sel) => {
                                                    let qid = g
                                                        .active_question
                                                        .as_ref()
                                                        .map(|q| q.question_id.clone());
                                                    drop(g);
                                                    if let Some(qid) = qid {
                                                        if qid == STARTUP_APPROVE_ALL_QUESTION_ID {
                                                            let _ = cmd_tx
                                                                .send(TuiCmd::QuestionAnswer(sel));
                                                        } else if let Some(ref tx) =
                                                            question_answer_tx
                                                        {
                                                            let _ = tx.send((qid, sel));
                                                        } else {
                                                            let _ = cmd_tx
                                                                .send(TuiCmd::QuestionAnswer(sel));
                                                        }
                                                    }
                                                }
                                                LineClickHit::CopyText(text) => {
                                                    match copy_to_clipboard(&text) {
                                                        Ok(_) => g.show_toast(
                                                            "✓ Copied to clipboard",
                                                            crate::tui::state::ToastKind::Success,
                                                        ),
                                                        Err(e) => g.show_toast(
                                                            format!("✗ Copy failed: {e}"),
                                                            crate::tui::state::ToastKind::Error,
                                                        ),
                                                    }
                                                }
                                                LineClickHit::ToggleThinking => {
                                                    g.thinking_expanded = !g.thinking_expanded;
                                                    g.touch_transcript();
                                                }
                                                LineClickHit::ToggleAssistant(block_idx) => {
                                                    if g.collapsed_assistant_blocks
                                                        .contains(&block_idx)
                                                    {
                                                        g.collapsed_assistant_blocks
                                                            .remove(&block_idx);
                                                    } else {
                                                        g.collapsed_assistant_blocks
                                                            .insert(block_idx);
                                                    }
                                                    g.touch_transcript();
                                                }
                                                LineClickHit::OpenLink(target) => {
                                                    let _ = open_link_in_system(&target);
                                                    g.show_toast(
                                                        format!(
                                                            "↗ Opened: {}",
                                                            &target[..target.len().min(40)]
                                                        ),
                                                        crate::tui::state::ToastKind::Info,
                                                    );
                                                }
                                            }
                                            continue;
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }

                    if let Some(sr) = slash_r
                        && rect_contains(sr, m.column, m.row)
                        && mouse_left_activated(m.kind)
                    {
                        let inner_y = m.row.saturating_sub(sr.y).saturating_sub(1);
                        if slash_panel_visible(&g.input_buffer) && !slash_filtered.is_empty() {
                            let n_show = slash_filtered.len().min(SLASH_PANEL_MAX_ROWS);
                            let max_scroll = slash_filtered.len().saturating_sub(n_show);
                            let list_scroll = g
                                .slash_menu_index
                                .saturating_sub(n_show.saturating_sub(1))
                                .min(max_scroll);
                            if (inner_y as usize) < n_show {
                                let idx = list_scroll + inner_y as usize;
                                if idx < slash_filtered.len() {
                                    g.set_input_text(slash_filtered[idx].command_str());
                                    g.slash_menu_index = idx;
                                }
                            }
                        } else if !at_matches.is_empty() {
                            let n_show = at_matches.len().min(SLASH_PANEL_MAX_ROWS);
                            let max_scroll = at_matches.len().saturating_sub(n_show);
                            let pick = g.at_menu_index.min(at_matches.len().saturating_sub(1));
                            let list_scroll = pick
                                .saturating_sub(n_show.saturating_sub(1))
                                .min(max_scroll);
                            if (inner_y as usize) < n_show {
                                let idx = list_scroll + inner_y as usize;
                                if let Some(choice) = at_matches.get(idx) {
                                    let cur = g.cursor_char_idx;
                                    let (buf, cidx) =
                                        apply_at_completion(&g.input_buffer, cur, choice);
                                    g.set_input_text_with_cursor(buf, cidx);
                                }
                            }
                        }
                    }

                    // Check click on branch chip in status bar.
                    if let Some(bounds) = g.branch_chip_bounds
                        && rect_contains(bounds, m.column, m.row)
                        && mouse_left_activated(m.kind)
                    {
                        let _ = cmd_tx.send(TuiCmd::OpenBranchPicker);
                    }
                }
                Event::Paste(pasted) => {
                    if g.api_key_modal_open {
                        let normalized = pasted.replace('\r', "");
                        let value = normalized.trim_end_matches('\n');
                        g.api_key_input.push_str(value);
                        if g.onboarding_mode {
                            g.validation_status = None;
                        }
                    } else if g.anthropic_oauth_modal_open {
                        let normalized = pasted.replace('\r', "");
                        let value = normalized.trim_end_matches('\n');
                        g.anthropic_oauth_code_input.push_str(value);
                    } else {
                        // Image paste: many terminals (e.g. Windows Terminal)
                        // consume Ctrl+V as a bracketed paste, so the raw Ctrl+V
                        // key handler never runs. Bracketed paste is text-only,
                        // so a clipboard *image* arrives here as an empty paste.
                        // When the paste is empty but the clipboard holds an
                        // image, stage the image instead of inserting nothing.
                        let staged_image = if pasted.trim().is_empty() {
                            let ws = g.workspace_root.clone();
                            let sid = g.session_id.clone();
                            match crate::image_attach::paste_clipboard_image(&ws, &sid) {
                                Ok(att) => {
                                    let label = att.path.clone();
                                    g.staged_image_attachments.push(att);
                                    g.push_block(DisplayBlock::System(format!(
                                        "[image] staged {label} — Enter to send"
                                    )));
                                    g.touch_transcript();
                                    true
                                }
                                Err(_) => false,
                            }
                        } else {
                            false
                        };
                        if !staged_image {
                            match stage_pasted_image_paths(&mut g, &pasted) {
                                Ok(0) => {
                                    insert_pasted_text(&mut g, &slash_entries, &pasted);
                                    composer_history_index = None;
                                    composer_history_draft.clear();
                                }
                                Ok(_) => {}
                                Err(e) => g.push_error(format!("[image] {e}")),
                            }
                        }
                    }
                }
                Event::Resize(_, _) => {
                    // Defer the reflow until the resize settles (debounce) so a
                    // drag doesn't trigger a flicker storm of purge+reflushes.
                    // The loop fires one clean reflush once events go quiet.
                    pending_resize_at = Some(std::time::Instant::now());
                }
                Event::FocusGained => {
                    terminal_focused = true;
                }
                Event::FocusLost => {
                    terminal_focused = false;
                }
                Event::Key(key) => {
                    if matches!(key.kind, KeyEventKind::Release) {
                        continue;
                    }
                    // Customizable keybindings: translate the key through the
                    // user's remap (rewrite custom→default, suppress reassigned
                    // defaults). No-op when nothing is remapped.
                    let key = if g.key_bindings.is_empty() {
                        key
                    } else {
                        match translate_key(key, &g.key_bindings) {
                            Some(k) => k,
                            None => continue,
                        }
                    };

                    // Emacs-style list navigation: while a list overlay is
                    // active, Ctrl+N/Ctrl+P act as Down/Up. Exceptions keep
                    // their stronger meanings: Ctrl+P still toggles the
                    // command palette closed, and Ctrl+N still answers "no"
                    // on an approval prompt.
                    let list_nav_active = g.model_picker_open
                        || g.session_picker_open
                        || g.connect_modal_open
                        || g.provider_picker_open
                        || g.permission_picker_open
                        || g.agent_picker_open
                        || g.theme_picker_open
                        || g.project_picker_open
                        || g.branch_picker_open
                        || g.question_modal_open
                        || g.pins_modal_open
                        || g.subagent_modal_open
                        || g.backtrack_open
                        || g.composer_history_search_open
                        || slash_panel_visible(&g.input_buffer)
                        || at_completion_active(&g.input_buffer, g.cursor_char_idx);
                    let key = if (list_nav_active || g.command_palette_open)
                        && key.modifiers == KeyModifiers::CONTROL
                        && g.active_approval.is_none()
                    {
                        match key.code {
                            KeyCode::Char('n') => KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
                            KeyCode::Char('p') if !g.command_palette_open => {
                                KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)
                            }
                            _ => key,
                        }
                    } else {
                        key
                    };

                    // Any keystroke clears active mouse text selection.
                    if g.mouse_selection.is_some() {
                        g.mouse_selection = None;
                    }

                    // Ctrl+C behavior:
                    // 1) If a turn is active, cancel it.
                    // 2) If idle, require a second Ctrl+C within 1.5s to exit.
                    let is_ctrl_c = matches!(key.code, KeyCode::Char('c'))
                        && key.modifiers.contains(KeyModifiers::CONTROL)
                        && !key.modifiers.contains(KeyModifiers::ALT);
                    if is_ctrl_c {
                        if escape_cancels_active_turn(&g) {
                            request_turn_cancel(&mut g, cancel_flag.as_ref(), &cmd_tx);
                            ctrl_c_armed_at = None;
                        } else {
                            let now = std::time::Instant::now();
                            let armed = ctrl_c_armed_at.is_some_and(|t| {
                                now.duration_since(t) <= Duration::from_millis(1500)
                            });
                            if armed {
                                g.should_exit = true;
                                let _ = cmd_tx.send(TuiCmd::Exit);
                                break;
                            }
                            ctrl_c_armed_at = Some(now);
                            g.push_block(DisplayBlock::System(
                                "Press Ctrl+C again within 1.5s to exit.".into(),
                            ));
                            g.touch_transcript();
                            g.transcript_follow_tail = true;
                            g.notification_count = 0;
                        }
                        continue;
                    } else {
                        ctrl_c_armed_at = None;
                    }

                    // Force-close modals that aren't rendered in the inline
                    // viewport, so their key handlers can't trap input. Modals
                    // we DO render (command palette, model/theme/agent/project/
                    // session pickers, info, question, approval) are left alone.
                    g.anthropic_oauth_modal_open = false;
                    g.provider_picker_open = false;
                    g.permission_picker_open = false;
                    g.branch_picker_open = false;
                    g.pins_modal_open = false;
                    g.subagent_modal_open = false;
                    g.transcript_search_open = false;
                    g.composer_history_search_open = false;

                    if g.anthropic_oauth_modal_open {
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => {
                                g.close_anthropic_oauth_modal();
                                if g.onboarding_mode {
                                    g.open_connect_modal();
                                }
                            }
                            (KeyCode::Enter, _) => {
                                let code = g.anthropic_oauth_code_input.trim().to_string();
                                let verifier = g.anthropic_oauth_code_verifier.clone();
                                if code.is_empty() {
                                    g.push_error(
                                        "[login] paste authorization code, then press Enter".into(),
                                    );
                                } else {
                                    g.close_anthropic_oauth_modal();
                                    drop(g);
                                    let _ = cmd_tx.send(TuiCmd::CompleteAnthropicOAuth {
                                        code_verifier: verifier,
                                        authorization_code: code,
                                    });
                                }
                            }
                            (KeyCode::Backspace, _) => {
                                g.anthropic_oauth_code_input.pop();
                            }
                            (KeyCode::Insert, mods) if mods.contains(KeyModifiers::SHIFT) => {
                                match paste_text_from_clipboard() {
                                    Ok(pasted) => {
                                        let normalized = pasted.replace('\r', "");
                                        let value = normalized.trim_end_matches('\n');
                                        g.anthropic_oauth_code_input.push_str(value);
                                    }
                                    Err(e) => {
                                        g.push_error(format!("[login] clipboard paste failed: {e}"))
                                    }
                                }
                            }
                            (KeyCode::Char(c), mods)
                                if c.eq_ignore_ascii_case(&'v')
                                    && mods.contains(KeyModifiers::CONTROL)
                                    && !mods.contains(KeyModifiers::ALT) =>
                            {
                                match paste_text_from_clipboard() {
                                    Ok(pasted) => {
                                        let normalized = pasted.replace('\r', "");
                                        let value = normalized.trim_end_matches('\n');
                                        g.anthropic_oauth_code_input.push_str(value);
                                    }
                                    Err(e) => {
                                        g.push_error(format!("[login] clipboard paste failed: {e}"))
                                    }
                                }
                            }
                            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                                g.anthropic_oauth_code_input.push(c);
                            }
                            _ => {}
                        }
                        continue;
                    }
                    if g.command_palette_open {
                        let mapped = match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) | (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                                Some(CommandPaletteKey::Cancel)
                            }
                            (KeyCode::Up, _) => Some(CommandPaletteKey::Up),
                            (KeyCode::Down, _) => Some(CommandPaletteKey::Down),
                            (KeyCode::Enter, _) => Some(CommandPaletteKey::Accept),
                            (KeyCode::Backspace, _) => Some(CommandPaletteKey::Backspace),
                            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                                Some(CommandPaletteKey::Char(c))
                            }
                            _ => None,
                        };
                        if let Some(k) = mapped {
                            g.apply_command_palette_key(k);
                        }
                        continue;
                    }

                    // Info modal (read-only scrollable popup).
                    if g.info_modal_open {
                        let mapped = match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) | (KeyCode::Char('q'), KeyModifiers::NONE) => {
                                Some(InfoModalKey::Close)
                            }
                            (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                                Some(InfoModalKey::ScrollUp)
                            }
                            (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                                Some(InfoModalKey::ScrollDown)
                            }
                            (KeyCode::Left, _) | (KeyCode::Char('h'), KeyModifiers::NONE) => {
                                Some(InfoModalKey::ScrollLeft)
                            }
                            (KeyCode::Right, _) | (KeyCode::Char('l'), KeyModifiers::NONE) => {
                                Some(InfoModalKey::ScrollRight)
                            }
                            (KeyCode::Home, _) => Some(InfoModalKey::Home),
                            (KeyCode::End, _) => Some(InfoModalKey::End),
                            _ => None,
                        };
                        if let Some(k) = mapped {
                            g.apply_info_modal_key(k);
                        }
                        continue;
                    }

                    // Model picker popup.
                    if g.model_picker_open {
                        let mapped = match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => Some(ModelPickerKey::Cancel),
                            (KeyCode::Up, _) => Some(ModelPickerKey::Up),
                            (KeyCode::Down, _) => Some(ModelPickerKey::Down),
                            (KeyCode::Enter, _) => Some(ModelPickerKey::Accept),
                            (KeyCode::Backspace, _) => Some(ModelPickerKey::Backspace),
                            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                                Some(ModelPickerKey::Char(c))
                            }
                            _ => None,
                        };
                        if let Some(k) = mapped
                            && let Some(action) = g.apply_model_picker_key(k)
                        {
                            drop(g);
                            match action {
                                ModelPickerAction::SwitchProvider(p) => {
                                    let _ = cmd_tx.send(TuiCmd::ApplyModelProvider(p));
                                }
                                ModelPickerAction::SwitchCopilot => {
                                    let _ = cmd_tx
                                        .send(TuiCmd::Submit("/provider copilot".to_string()));
                                }
                                ModelPickerAction::ApplyModel(m) => {
                                    let _ = cmd_tx.send(TuiCmd::ApplyModel(m));
                                }
                            }
                        }
                        continue;
                    }

                    // Connect provider (OpenCode-style `/connect`).
                    if g.connect_modal_open {
                        let mapped = match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => Some(ConnectModalKey::Cancel),
                            (KeyCode::Up, _) => Some(ConnectModalKey::Up),
                            (KeyCode::Down, _) => Some(ConnectModalKey::Down),
                            (KeyCode::Enter, _) => Some(ConnectModalKey::Accept),
                            (KeyCode::Backspace, _) => Some(ConnectModalKey::Backspace),
                            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                                Some(ConnectModalKey::Char(c))
                            }
                            _ => None,
                        };
                        if let Some(k) = mapped
                            && let Some(action) = g.apply_connect_modal_key(k)
                        {
                            drop(g);
                            match action {
                                ConnectAction::OAuthLogin(slug) => {
                                    let store = AuthStore::load().unwrap_or_default();
                                    if oauth_logged_in_for_slug(&store, slug) {
                                        if let Some(cmd) = oauth_switch_command_for_slug(slug) {
                                            let _ = cmd_tx.send(TuiCmd::Submit(cmd.to_string()));
                                        } else {
                                            let _ = cmd_tx
                                                .send(TuiCmd::Submit(format!("/login {slug}")));
                                        }
                                    } else {
                                        let _ =
                                            cmd_tx.send(TuiCmd::Submit(format!("/login {slug}")));
                                    }
                                }
                                ConnectAction::PromptApiKey(p) => {
                                    let _ = cmd_tx.send(TuiCmd::PromptApiKey(p, true));
                                }
                                ConnectAction::Submit(cmd) => {
                                    let _ = cmd_tx.send(TuiCmd::Submit(cmd.to_string()));
                                }
                            }
                        }
                        continue;
                    }

                    if g.api_key_modal_open {
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => {
                                g.close_api_key_modal();
                                if g.onboarding_mode {
                                    // Go back to connect modal instead of closing entirely
                                    g.open_connect_modal();
                                }
                            }
                            (KeyCode::Enter, _) => {
                                if g.onboarding_mode {
                                    // Block input while validation is in flight
                                    if matches!(
                                        g.validation_status,
                                        Some(crate::tui::state::OnboardingValidation::Validating)
                                    ) {
                                        // Already validating — ignore
                                    } else if let Some(provider) = g.api_key_target_provider {
                                        let key = g.api_key_input.trim().to_string();
                                        if key.is_empty() {
                                            // Don't submit empty keys during onboarding
                                        } else {
                                            g.validation_status = Some(
                                                crate::tui::state::OnboardingValidation::Validating,
                                            );
                                            drop(g);
                                            let _ =
                                                cmd_tx.send(TuiCmd::ValidateApiKey(provider, key));
                                        }
                                    }
                                } else {
                                    drop(g);
                                    let _ = cmd_tx.send(TuiCmd::Submit(String::new()));
                                }
                            }
                            (KeyCode::Backspace, _) => {
                                g.api_key_input.pop();
                                if g.onboarding_mode {
                                    g.validation_status = None;
                                }
                            }
                            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                                g.api_key_input.push(c);
                                if g.onboarding_mode {
                                    g.validation_status = None; // Clear stale error on new input
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Provider picker (settings).
                    if g.provider_picker_open {
                        let mapped = match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => Some(ProviderPickerKey::Cancel),
                            (KeyCode::Up, _) => Some(ProviderPickerKey::Up),
                            (KeyCode::Down, _) => Some(ProviderPickerKey::Down),
                            (KeyCode::Enter, _) => Some(ProviderPickerKey::Accept),
                            _ => None,
                        };
                        if let Some(k) = mapped {
                            match g.apply_provider_picker_key(k) {
                                Some(ProviderPickerOutcome::Apply(p)) => {
                                    drop(g);
                                    let _ = cmd_tx.send(TuiCmd::ApplyDefaultProvider(p));
                                }
                                Some(ProviderPickerOutcome::ForApiKey(p)) => {
                                    drop(g);
                                    if let Some(slug) = oauth_login_provider_slug(p) {
                                        let _ =
                                            cmd_tx.send(TuiCmd::Submit(format!("/login {slug}")));
                                    } else {
                                        let _ = cmd_tx.send(TuiCmd::PromptApiKey(p, false));
                                    }
                                }
                                None => {}
                            }
                        }
                        continue;
                    }

                    // Project picker keyboard handling.
                    if g.project_picker_open {
                        use crate::tui::state::ProjectPickerKey;
                        let mapped = match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => Some(ProjectPickerKey::Cancel),
                            (KeyCode::Up, _) => Some(ProjectPickerKey::Up),
                            (KeyCode::Down, _) => Some(ProjectPickerKey::Down),
                            (KeyCode::Enter, _) => Some(ProjectPickerKey::Accept),
                            (KeyCode::Delete, _) | (KeyCode::Backspace, KeyModifiers::CONTROL) => {
                                Some(ProjectPickerKey::Remove)
                            }
                            _ => None,
                        };
                        if let Some(k) = mapped
                            && let Some(idx) = g.apply_project_picker_key(k)
                        {
                            drop(g);
                            let _ = cmd_tx.send(TuiCmd::SwitchProject(idx));
                        }
                        continue;
                    }

                    // Branch picker keyboard handling.
                    if g.branch_picker_open {
                        let mapped = match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => Some(BranchPickerKey::Cancel),
                            (KeyCode::Up, _) => Some(BranchPickerKey::Up),
                            (KeyCode::Down, _) => Some(BranchPickerKey::Down),
                            (KeyCode::Enter, _) => Some(BranchPickerKey::Accept),
                            (KeyCode::Backspace, _) => Some(BranchPickerKey::Backspace),
                            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                                Some(BranchPickerKey::Char(c))
                            }
                            _ => None,
                        };
                        if let Some(k) = mapped
                            && let Some(cmd) = g.apply_branch_picker_key(k)
                        {
                            drop(g);
                            let _ = cmd_tx.send(cmd);
                        }
                        continue;
                    }

                    // Question modal keyboard handling.
                    if g.question_modal_open {
                        let mapped = match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => Some(QuestionModalKey::Cancel),
                            (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                                Some(QuestionModalKey::Up)
                            }
                            (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                                Some(QuestionModalKey::Down)
                            }
                            (KeyCode::Enter, _) => Some(QuestionModalKey::Accept),
                            _ => None,
                        };
                        if let Some(k) = mapped
                            && let QuestionModalOutcome::Answer {
                                question_id,
                                selection,
                            } = g.apply_question_modal_key(k)
                        {
                            // Route the answer. STARTUP_APPROVE_ALL keeps active_question
                            // so its follow-up flow still sees it.
                            if question_id != STARTUP_APPROVE_ALL_QUESTION_ID {
                                g.active_question = None;
                            }
                            drop(g);
                            if question_id == STARTUP_APPROVE_ALL_QUESTION_ID {
                                let _ = cmd_tx.send(TuiCmd::QuestionAnswer(selection));
                            } else if let Some(ref tx) = question_answer_tx {
                                let _ = tx.send((question_id, selection));
                            } else {
                                let _ = cmd_tx.send(TuiCmd::QuestionAnswer(selection));
                            }
                        }
                        continue;
                    }

                    // Permission picker keyboard handling.
                    if g.permission_picker_open {
                        const PERM_COUNT: usize = 5;
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => {
                                g.close_permission_picker();
                            }
                            (KeyCode::Up, _) => {
                                g.permission_picker_index =
                                    g.permission_picker_index.saturating_sub(1);
                            }
                            (KeyCode::Down, _) => {
                                g.permission_picker_index =
                                    (g.permission_picker_index + 1).min(PERM_COUNT - 1);
                            }
                            (KeyCode::Enter, _) => {
                                let idx = g.permission_picker_index;
                                g.close_permission_picker();
                                drop(g);
                                let _ = cmd_tx.send(TuiCmd::ApplyPermission(idx));
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Theme picker keyboard handling.
                    if g.theme_picker_open {
                        let count = g.theme_picker_entries.len();
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => {
                                // Revert live preview: re-apply the original theme from config.
                                let name =
                                    g.theme_picker_entries.get(g.theme_picker_index).cloned();
                                g.close_theme_picker();
                                drop(g);
                                // Ask repl to reapply persisted theme (no-op if same).
                                let _ = name; // index was not persisted — noop is fine
                            }
                            (KeyCode::Up, _) => {
                                if count > 0 {
                                    g.theme_picker_index = g.theme_picker_index.saturating_sub(1);
                                    // Live preview: apply selected theme immediately.
                                    if let Some(name) =
                                        g.theme_picker_entries.get(g.theme_picker_index)
                                    {
                                        theme::set_by_name(Some(name));
                                    }
                                }
                            }
                            (KeyCode::Down, _) => {
                                if count > 0 {
                                    g.theme_picker_index =
                                        (g.theme_picker_index + 1).min(count - 1);
                                    if let Some(name) =
                                        g.theme_picker_entries.get(g.theme_picker_index)
                                    {
                                        theme::set_by_name(Some(name));
                                    }
                                }
                            }
                            (KeyCode::Enter, _) => {
                                let name =
                                    g.theme_picker_entries.get(g.theme_picker_index).cloned();
                                g.close_theme_picker();
                                drop(g);
                                if let Some(n) = name {
                                    let _ = cmd_tx.send(TuiCmd::ApplyTheme(n));
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Agent profile picker keyboard handling.
                    if g.agent_picker_open {
                        const AGENT_COUNT: usize = 5;
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => {
                                g.close_agent_picker();
                            }
                            (KeyCode::Up, _) => {
                                g.agent_picker_index = g.agent_picker_index.saturating_sub(1);
                            }
                            (KeyCode::Down, _) => {
                                g.agent_picker_index =
                                    (g.agent_picker_index + 1).min(AGENT_COUNT - 1);
                            }
                            (KeyCode::Enter, _) => {
                                let idx = g.agent_picker_index;
                                g.close_agent_picker();
                                drop(g);
                                let _ = cmd_tx.send(TuiCmd::SwitchAgent(idx));
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Backtrack picker keyboard handling (Esc while idle).
                    if g.backtrack_open {
                        let mapped = match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => Some(BacktrackKey::Cancel),
                            (KeyCode::Up, _) => Some(BacktrackKey::Up),
                            (KeyCode::Down, _) => Some(BacktrackKey::Down),
                            (KeyCode::Enter, _) => Some(BacktrackKey::Accept),
                            _ => None,
                        };
                        if let Some(k) = mapped
                            && let Some(rewind) = g.apply_backtrack_key(k)
                        {
                            drop(g);
                            let _ = cmd_tx.send(TuiCmd::Backtrack {
                                user_index_from_end: rewind.user_index_from_end,
                                text: rewind.text,
                            });
                        }
                        continue;
                    }

                    // Session picker keyboard handling.
                    if g.session_picker_open {
                        let mapped = match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => Some(SessionPickerKey::Cancel),
                            (KeyCode::Up, _) => Some(SessionPickerKey::Up),
                            (KeyCode::Down, _) => Some(SessionPickerKey::Down),
                            (KeyCode::Enter, _) => Some(SessionPickerKey::Accept),
                            (KeyCode::Backspace, _) => Some(SessionPickerKey::Backspace),
                            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                                Some(SessionPickerKey::Char(c))
                            }
                            _ => None,
                        };
                        if let Some(k) = mapped
                            && let Some(id) = g.apply_session_picker_key(k)
                        {
                            drop(g);
                            let _ = cmd_tx.send(TuiCmd::ResumeSession(id));
                        }
                        continue;
                    }

                    if g.pins_modal_open {
                        let mapped = match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) | (KeyCode::Char('o'), KeyModifiers::CONTROL) => {
                                Some(PinsModalKey::Close)
                            }
                            (KeyCode::Up, _) => Some(PinsModalKey::Up),
                            (KeyCode::Down, _) => Some(PinsModalKey::Down),
                            (KeyCode::Backspace, _) => Some(PinsModalKey::Delete),
                            (KeyCode::Enter, _) => Some(PinsModalKey::Accept),
                            (KeyCode::F(6), _) => Some(PinsModalKey::Copy),
                            _ => None,
                        };
                        if let Some(k) = mapped
                            && g.apply_pins_modal_key(k)
                        {
                            // Copy requested: clipboard write + status line stay in the loop.
                            let msg = if let Some(note) = g.pinned_notes.get(g.pins_modal_index) {
                                match copy_to_clipboard(&note.body) {
                                    Ok(_) => "Copied pinned note".to_string(),
                                    Err(e) => format!("Clipboard copy failed: {e}"),
                                }
                            } else {
                                "No pinned note selected".to_string()
                            };
                            g.push_block(DisplayBlock::System(msg));
                            g.touch_transcript();
                            g.transcript_follow_tail = true;
                            g.notification_count = 0;
                        }
                        continue;
                    }

                    if g.subagent_modal_open {
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) | (KeyCode::Char('g'), KeyModifiers::CONTROL) => {
                                g.close_subagent_modal();
                            }
                            (KeyCode::Up, _) => {
                                g.subagent_modal_index = g.subagent_modal_index.saturating_sub(1);
                            }
                            (KeyCode::Down, _) => {
                                if !g.subagents.is_empty() {
                                    g.subagent_modal_index = (g.subagent_modal_index + 1)
                                        .min(g.subagents.len().saturating_sub(1));
                                }
                            }
                            (KeyCode::Enter, _) => {
                                if let Some(row) = g.subagents.get(g.subagent_modal_index) {
                                    let id = row.id.clone();
                                    g.close_subagent_modal();
                                    drop(g);
                                    let _ = cmd_tx.send(TuiCmd::ResumeSession(id));
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }

                    if g.transcript_search_open {
                        let sz = terminal.size().ok();
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) | (KeyCode::Char('f'), KeyModifiers::CONTROL) => {
                                g.close_transcript_search();
                            }
                            (KeyCode::Backspace, _) => {
                                g.transcript_search_query.pop();
                                g.transcript_search_index = 0;
                            }
                            (KeyCode::Up, _) => {
                                let inner_w = sz.map(|s| s.width.saturating_sub(2)).unwrap_or(80);
                                let (lines, _, _) = transcript_cache.get_or_rebuild(&g, inner_w);
                                let matches =
                                    transcript_search_matches(lines, &g.transcript_search_query);
                                if !matches.is_empty() {
                                    if g.transcript_search_index == 0 {
                                        g.transcript_search_index = matches.len().saturating_sub(1);
                                    } else {
                                        g.transcript_search_index =
                                            g.transcript_search_index.saturating_sub(1);
                                    }
                                }
                            }
                            (KeyCode::Down, _) | (KeyCode::Enter, _) => {
                                let inner_w = sz.map(|s| s.width.saturating_sub(2)).unwrap_or(80);
                                let (lines, _, _) = transcript_cache.get_or_rebuild(&g, inner_w);
                                let matches =
                                    transcript_search_matches(lines, &g.transcript_search_query);
                                if !matches.is_empty() {
                                    g.transcript_search_index =
                                        (g.transcript_search_index + 1) % matches.len();
                                }
                            }
                            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                                g.transcript_search_query.push(c);
                                g.transcript_search_index = 0;
                            }
                            _ => {}
                        }
                        continue;
                    }

                    if g.composer_history_search_open {
                        let mapped = match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) | (KeyCode::Char('r'), KeyModifiers::CONTROL) => {
                                Some(HistorySearchKey::Cancel)
                            }
                            (KeyCode::Backspace, _) => Some(HistorySearchKey::Backspace),
                            (KeyCode::Up, _) => Some(HistorySearchKey::Up),
                            (KeyCode::Down, _) => Some(HistorySearchKey::Down),
                            (KeyCode::Enter, _) => Some(HistorySearchKey::Accept),
                            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                                Some(HistorySearchKey::Char(c))
                            }
                            _ => None,
                        };
                        if let Some(k) = mapped {
                            g.apply_history_search_key(k, &composer_history);
                        }
                        continue;
                    }

                    // Ctrl+X leader key dispatch.
                    if g.leader_pending {
                        g.leader_pending = false;
                        match key.code {
                            KeyCode::Char('m') | KeyCode::Char('M') => {
                                drop(g);
                                let _ = cmd_tx.send(TuiCmd::OpenModelPicker);
                            }
                            KeyCode::Char('e') | KeyCode::Char('E') => {
                                drop(g);
                                let _ = cmd_tx.send(TuiCmd::OpenEditor);
                            }
                            KeyCode::Char('l') | KeyCode::Char('L') => {
                                drop(g);
                                let _ = cmd_tx.send(TuiCmd::OpenSessions);
                            }
                            KeyCode::Char('n') | KeyCode::Char('N') => {
                                drop(g);
                                let _ = cmd_tx.send(TuiCmd::NewSession);
                            }
                            KeyCode::Char('c') | KeyCode::Char('C') => {
                                drop(g);
                                let _ = cmd_tx.send(TuiCmd::RunCompact);
                            }
                            KeyCode::Char('s') | KeyCode::Char('S') => {
                                drop(g);
                                let _ = cmd_tx.send(TuiCmd::OpenStatus);
                            }
                            KeyCode::Char('b') | KeyCode::Char('B') => {
                                drop(g);
                                let _ = cmd_tx.send(TuiCmd::OpenStatus);
                            }
                            KeyCode::Char('a') | KeyCode::Char('A') => {
                                drop(g);
                                let _ = cmd_tx.send(TuiCmd::OpenAgentPicker);
                            }
                            KeyCode::Char('h') | KeyCode::Char('H') => {
                                drop(g);
                                let _ = cmd_tx.send(TuiCmd::OpenHelp);
                            }
                            KeyCode::Char('q') | KeyCode::Char('Q') => {
                                g.should_exit = true;
                                let _ = cmd_tx.send(TuiCmd::Exit);
                                break;
                            }
                            KeyCode::Char('f') => {
                                let toggled = g.toggle_last_tool_block();
                                let msg = if toggled {
                                    "Toggled latest tool block (collapsed/expanded)".to_string()
                                } else {
                                    "No tool blocks to toggle yet".to_string()
                                };
                                g.push_block(DisplayBlock::System(msg));
                                g.touch_transcript();
                            }
                            KeyCode::Char('F') => {
                                g.toggle_all_tool_blocks();
                                let state_msg = if g.all_tools_collapsed {
                                    "Collapsed all tool blocks (Ctrl+X F to expand)"
                                } else {
                                    "Expanded all tool blocks"
                                };
                                g.push_block(DisplayBlock::System(state_msg.into()));
                                g.touch_transcript();
                            }
                            KeyCode::Char('p') | KeyCode::Char('P') => {
                                drop(g);
                                let _ = cmd_tx.send(TuiCmd::OpenProjectPicker);
                            }
                            KeyCode::Char('v') | KeyCode::Char('V') => {
                                g.transcript_overlay_open = true;
                                g.transcript_overlay_scroll = usize::MAX; // start at bottom
                            }
                            _ => {}
                        }
                        continue;
                    }

                    match (key.code, key.modifiers) {
                        (KeyCode::Esc, _) if escape_cancels_active_turn(&g) => {
                            request_turn_cancel(&mut g, cancel_flag.as_ref(), &cmd_tx);
                        }
                        // Idle Esc with an empty composer: open the backtrack
                        // picker (edit a past user message + rewind to it).
                        (KeyCode::Esc, _) if !g.busy && g.input_buffer.is_empty() => {
                            g.open_backtrack();
                        }
                        (KeyCode::Char('q'), KeyModifiers::CONTROL) => {
                            g.should_exit = true;
                            let _ = cmd_tx.send(TuiCmd::Exit);
                            break;
                        }
                        (KeyCode::F(6), _) => {
                            let msg = if let Some(text) = latest_copyable_text(&g) {
                                match copy_to_clipboard(&text) {
                                    Ok(_) => "Copied latest assistant response".to_string(),
                                    Err(e) => format!("Clipboard copy failed: {e}"),
                                }
                            } else {
                                "Nothing to copy yet".to_string()
                            };
                            g.push_block(DisplayBlock::System(msg));
                            g.touch_transcript();
                            g.transcript_follow_tail = true;
                            g.notification_count = 0;
                        }
                        (KeyCode::F(7), _) => {
                            let detail = g.blocks.iter().rev().find_map(|b| {
                                if let DisplayBlock::ToolDone { detail, .. } = b {
                                    if !detail.trim().is_empty() {
                                        Some(detail.clone())
                                    } else {
                                        None
                                    }
                                } else {
                                    None
                                }
                            });
                            let msg = if let Some(text) = detail {
                                match copy_to_clipboard(&text) {
                                    Ok(_) => "Copied last tool output".to_string(),
                                    Err(e) => format!("Clipboard copy failed: {e}"),
                                }
                            } else {
                                "No tool output to copy yet".to_string()
                            };
                            g.push_block(DisplayBlock::System(msg));
                            g.touch_transcript();
                            g.transcript_follow_tail = true;
                            g.notification_count = 0;
                        }
                        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                            // Handled earlier in Event::Key with double-press semantics.
                        }
                        (KeyCode::Char('l'), KeyModifiers::CONTROL) => {
                            g.blocks.clear();
                            g.block_timestamps.clear();
                            g.flushed_block_count = 0;
                            g.streaming_assistant = None;
                            g.streaming_thinking = None;
                            g.request_clear = true;
                            g.touch_transcript();
                            g.scroll_lines = 0;
                            g.transcript_follow_tail = true;
                            g.notification_count = 0;
                            composer_history_index = None;
                            composer_history_draft.clear();
                        }
                        (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                            g.command_palette_open = true;
                            g.command_palette_query.clear();
                            g.palette_index = 0;
                        }
                        (KeyCode::Char('t'), KeyModifiers::CONTROL) => {
                            // Toggle reasoning/thinking, then re-render ALL history
                            // (purge + re-flush) so already-scrolled content reflects
                            // the new state — not just the live pane.
                            g.thinking_expanded = !g.thinking_expanded;
                            g.request_clear = true;
                            g.touch_transcript();
                        }
                        (KeyCode::Char('o'), KeyModifiers::CONTROL) => {
                            // Fold / unfold ALL tool output across the whole
                            // transcript (re-flushes flushed history too).
                            g.toggle_all_tool_blocks();
                            g.request_clear = true;
                            g.touch_transcript();
                        }
                        (KeyCode::Char('g'), KeyModifiers::CONTROL) => {
                            if g.subagents.is_empty() {
                                g.push_block(DisplayBlock::System(
                                    "No sub-agents to focus right now.".into(),
                                ));
                                g.touch_transcript();
                                g.transcript_follow_tail = true;
                                g.notification_count = 0;
                            } else {
                                g.open_subagent_modal();
                            }
                        }
                        (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
                            // When composer has text and no approval is pending,
                            // Ctrl+K kills to end of line (Emacs-style).
                            // Otherwise it pins the latest assistant message.
                            let has_text = !g.input_buffer.trim().is_empty();
                            if has_text && g.active_approval.is_none() {
                                g.kill_input_to_end();
                            } else {
                                let msg = if let Some(note) = latest_pinnable_note(&g) {
                                    let exists = g.pinned_notes.iter().any(|n| n.body == note.body);
                                    if !exists {
                                        g.pinned_notes.insert(0, note);
                                        if g.pinned_notes.len() > 20 {
                                            g.pinned_notes.pop();
                                        }
                                        "Pinned latest message".to_string()
                                    } else {
                                        "Latest message is already pinned".to_string()
                                    }
                                } else {
                                    "Nothing pinnable yet".to_string()
                                };
                                g.push_block(DisplayBlock::System(msg));
                                g.touch_transcript();
                                g.transcript_follow_tail = true;
                                g.notification_count = 0;
                            }
                        }
                        (KeyCode::Char('f'), KeyModifiers::CONTROL) => {
                            g.open_transcript_search();
                        }
                        (KeyCode::Char('r'), KeyModifiers::CONTROL) => {
                            g.composer_history_search_open = true;
                            g.composer_history_search_query.clear();
                            g.composer_history_search_index = 0;
                        }
                        (KeyCode::Char('b'), KeyModifiers::CONTROL) => {
                            drop(g);
                            let _ = cmd_tx.send(TuiCmd::OpenStatus);
                        }
                        (KeyCode::Char('x'), KeyModifiers::CONTROL) => {
                            g.leader_pending = true;
                        }
                        (KeyCode::Char('v'), mods)
                            if mods.contains(KeyModifiers::CONTROL)
                                && !mods.contains(KeyModifiers::ALT) =>
                        {
                            if g.active_approval.is_some() || g.active_question.is_some() {
                                continue;
                            }
                            let ws = g.workspace_root.clone();
                            let sid = g.session_id.clone();
                            match crate::image_attach::paste_clipboard_image(&ws, &sid) {
                                Ok(att) => {
                                    let label = att.path.clone();
                                    g.staged_image_attachments.push(att);
                                    g.push_block(DisplayBlock::System(format!(
                                        "[image] staged {label} — Enter to send"
                                    )));
                                    g.touch_transcript();
                                }
                                Err(image_err) => {
                                    let fallback_text =
                                        Clipboard::new().ok().and_then(|mut cb| cb.get_text().ok());
                                    if let Some(text) = fallback_text {
                                        match stage_pasted_image_paths(&mut g, &text) {
                                            Ok(0) => g.push_error(format!("[image] {image_err}")),
                                            Ok(_) => {}
                                            Err(path_err) => g.push_error(format!(
                                                "[image] {image_err}; clipboard path import failed: {path_err}"
                                            )),
                                        }
                                    } else {
                                        g.push_error(format!("[image] {image_err}"));
                                    }
                                }
                            }
                        }
                        (KeyCode::Char('i'), KeyModifiers::CONTROL) => {
                            composer_history_index = None;
                            composer_history_draft.clear();
                            g.insert_input_char('\n');
                        }
                        (KeyCode::Tab, mods)
                            if mods.contains(KeyModifiers::CONTROL)
                                && !mods.contains(KeyModifiers::ALT) =>
                        {
                            composer_history_index = None;
                            composer_history_draft.clear();
                            g.insert_input_char('\n');
                        }
                        (KeyCode::Tab, _) => {
                            if let Some((buf, cidx)) = apply_selected_at_completion(
                                &workspace_files,
                                &g.input_buffer,
                                g.cursor_char_idx,
                                g.at_menu_index,
                                false,
                            ) {
                                g.set_input_text_with_cursor(buf, cidx);
                            } else {
                                let slash_filtered =
                                    filter_slash_entries(&slash_entries, &g.input_buffer);
                                if !slash_filtered.is_empty()
                                    && slash_panel_visible(&g.input_buffer)
                                {
                                    let pick = g.slash_menu_index % slash_filtered.len();
                                    g.set_input_text(slash_filtered[pick].command_str());
                                } else {
                                    drop(g);
                                    let _ = cmd_tx.send(TuiCmd::CycleAgent);
                                }
                            }
                        }
                        (KeyCode::F(1), _) => {
                            g.open_info_modal("Keybindings", keybindings_cheatsheet());
                        }
                        (KeyCode::F(2), KeyModifiers::NONE) => {
                            drop(g);
                            let _ = cmd_tx.send(TuiCmd::CycleModel(true));
                        }
                        (KeyCode::F(2), KeyModifiers::SHIFT) => {
                            drop(g);
                            let _ = cmd_tx.send(TuiCmd::CycleModel(false));
                        }
                        // Transcript zoom: Ctrl+Plus / Ctrl+Minus / Ctrl+0
                        // to avoid stealing -, +, = from the composer.
                        (KeyCode::Char('+'), KeyModifiers::CONTROL)
                        | (KeyCode::Char('='), KeyModifiers::CONTROL) => {
                            g.transcript_zoom_offset = (g.transcript_zoom_offset + 4).min(40);
                            g.touch_transcript();
                        }
                        (KeyCode::Char('-'), KeyModifiers::CONTROL) => {
                            g.transcript_zoom_offset = (g.transcript_zoom_offset - 4).max(-20);
                            g.touch_transcript();
                        }
                        (KeyCode::Char('0'), KeyModifiers::CONTROL) => {
                            g.transcript_zoom_offset = 0;
                            g.touch_transcript();
                        }
                        (KeyCode::Up, KeyModifiers::NONE) if g.active_approval.is_some() => {
                            g.approval_option_index = g.approval_option_index.saturating_sub(1);
                            g.touch_transcript();
                            continue;
                        }
                        (KeyCode::Down, KeyModifiers::NONE) if g.active_approval.is_some() => {
                            g.approval_option_index = (g.approval_option_index + 1).min(2);
                            g.touch_transcript();
                            continue;
                        }
                        (KeyCode::Char('y'), KeyModifiers::NONE)
                        | (KeyCode::Char('Y'), KeyModifiers::NONE)
                            if g.active_approval.is_some() =>
                        {
                            if let Some(req) = g.active_approval.clone() {
                                let call_id = req.call_id.clone();
                                g.active_approval = None;
                                g.approval_option_index = 0;
                                g.clear_input();
                                drop(g);
                                if let Some(ref tx) = approval_answer_tx {
                                    let _ = tx.send(ApprovalAnswer::Verdict {
                                        call_id,
                                        approved: true,
                                    });
                                }
                                continue;
                            }
                        }
                        (KeyCode::Char('n'), KeyModifiers::NONE)
                        | (KeyCode::Char('N'), KeyModifiers::NONE)
                            if g.active_approval.is_some() =>
                        {
                            if let Some(req) = g.active_approval.clone() {
                                let call_id = req.call_id.clone();
                                g.active_approval = None;
                                g.approval_option_index = 0;
                                g.clear_input();
                                drop(g);
                                if let Some(ref tx) = approval_answer_tx {
                                    let _ = tx.send(ApprovalAnswer::Verdict {
                                        call_id,
                                        approved: false,
                                    });
                                }
                                continue;
                            }
                        }
                        (KeyCode::Char('a'), KeyModifiers::NONE)
                        | (KeyCode::Char('A'), KeyModifiers::NONE)
                            if g.active_approval.is_some() =>
                        {
                            if let Some(req) = g.active_approval.clone() {
                                let pattern = req.allow_pattern();
                                let call_id = req.call_id.clone();
                                g.active_approval = None;
                                g.approval_option_index = 0;
                                g.clear_input();
                                g.push_block(DisplayBlock::System(format!(
                                    "Always allowing: {pattern}"
                                )));
                                g.touch_transcript();
                                drop(g);
                                if let Some(ref tx) = approval_answer_tx {
                                    let _ =
                                        tx.send(ApprovalAnswer::AllowPattern { call_id, pattern });
                                }
                                continue;
                            }
                        }
                        // Toggle hunk-selection mode for file-writing approvals.
                        (KeyCode::Char('h'), KeyModifiers::NONE)
                            if g.active_approval.is_some() && !g.approval_hunk_mode =>
                        {
                            if let Some(req) = g.active_approval.as_ref() {
                                let hunks = extract_approval_hunks(&req.tool, &req.input);
                                if !hunks.is_empty() {
                                    g.approval_hunk_selection = vec![true; hunks.len()];
                                    g.approval_hunk_cursor = 0;
                                    g.approval_hunk_mode = true;
                                    g.touch_transcript();
                                }
                            }
                            continue;
                        }
                        // Hunk mode navigation.
                        (KeyCode::Up, KeyModifiers::NONE) if g.approval_hunk_mode => {
                            g.approval_hunk_cursor = g.approval_hunk_cursor.saturating_sub(1);
                            g.touch_transcript();
                            continue;
                        }
                        (KeyCode::Down, KeyModifiers::NONE) if g.approval_hunk_mode => {
                            let max = g.approval_hunk_selection.len().saturating_sub(1);
                            g.approval_hunk_cursor = (g.approval_hunk_cursor + 1).min(max);
                            g.touch_transcript();
                            continue;
                        }
                        // Toggle current hunk.
                        (KeyCode::Char(' '), KeyModifiers::NONE) if g.approval_hunk_mode => {
                            let idx = g.approval_hunk_cursor;
                            if let Some(val) = g.approval_hunk_selection.get_mut(idx) {
                                *val = !*val;
                            }
                            g.touch_transcript();
                            continue;
                        }
                        // Accept current hunk.
                        (KeyCode::Char('y'), KeyModifiers::NONE) if g.approval_hunk_mode => {
                            let idx = g.approval_hunk_cursor;
                            if let Some(val) = g.approval_hunk_selection.get_mut(idx) {
                                *val = true;
                            }
                            let max = g.approval_hunk_selection.len().saturating_sub(1);
                            g.approval_hunk_cursor = (g.approval_hunk_cursor + 1).min(max);
                            g.touch_transcript();
                            continue;
                        }
                        // Reject current hunk.
                        (KeyCode::Char('n'), KeyModifiers::NONE) if g.approval_hunk_mode => {
                            let idx = g.approval_hunk_cursor;
                            if let Some(val) = g.approval_hunk_selection.get_mut(idx) {
                                *val = false;
                            }
                            let max = g.approval_hunk_selection.len().saturating_sub(1);
                            g.approval_hunk_cursor = (g.approval_hunk_cursor + 1).min(max);
                            g.touch_transcript();
                            continue;
                        }
                        // Exit hunk mode without applying.
                        (KeyCode::Esc, _) if g.approval_hunk_mode => {
                            g.approval_hunk_mode = false;
                            g.approval_hunk_selection.clear();
                            g.touch_transcript();
                            continue;
                        }
                        (KeyCode::Char('y'), KeyModifiers::CONTROL) => {
                            if let Some(req) = g.active_approval.clone() {
                                let call_id = req.call_id.clone();
                                g.active_approval = None;
                                g.approval_option_index = 0;
                                g.clear_input();
                                drop(g);
                                if let Some(ref tx) = approval_answer_tx {
                                    let _ = tx.send(ApprovalAnswer::Verdict {
                                        call_id,
                                        approved: true,
                                    });
                                }
                                continue;
                            } else {
                                // Yank from kill ring when not in an approval prompt.
                                g.yank_input();
                            }
                        }
                        (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
                            if let Some(req) = g.active_approval.clone() {
                                let call_id = req.call_id.clone();
                                g.active_approval = None;
                                g.approval_option_index = 0;
                                g.clear_input();
                                drop(g);
                                if let Some(ref tx) = approval_answer_tx {
                                    let _ = tx.send(ApprovalAnswer::Verdict {
                                        call_id,
                                        approved: false,
                                    });
                                }
                                continue;
                            }
                        }
                        (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                            if let Some(req) = g.active_approval.clone() {
                                let pattern = req.allow_pattern();
                                let call_id = req.call_id.clone();
                                g.active_approval = None;
                                g.approval_option_index = 0;
                                g.clear_input();
                                g.push_block(DisplayBlock::System(format!(
                                    "Always allowing: {pattern}"
                                )));
                                g.touch_transcript();
                                drop(g);
                                if let Some(ref tx) = approval_answer_tx {
                                    let _ =
                                        tx.send(ApprovalAnswer::AllowPattern { call_id, pattern });
                                }
                                continue;
                            }
                        }
                        (KeyCode::Enter, mods) => {
                            if mods.contains(KeyModifiers::SHIFT)
                                && !mods.contains(KeyModifiers::CONTROL)
                                && !mods.contains(KeyModifiers::ALT)
                            {
                                composer_history_index = None;
                                composer_history_draft.clear();
                                g.insert_input_char('\n');
                                continue;
                            }
                            if let Some((buf, cidx)) = apply_selected_at_completion(
                                &workspace_files,
                                &g.input_buffer,
                                g.cursor_char_idx,
                                g.at_menu_index,
                                true,
                            ) {
                                g.set_input_text_with_cursor(buf, cidx);
                                continue;
                            }
                            // Slash completion (Codex-style): if the slash panel is
                            // open and the typed text is a *partial* command, Enter
                            // completes to the highlighted command before running —
                            // so `/age` + Enter runs `/agent`, not an unknown command.
                            if slash_panel_visible(&g.input_buffer) {
                                let typed = g.input_buffer.trim().to_string();
                                let is_exact =
                                    slash_entries.iter().any(|e| e.command_str() == typed);
                                if !is_exact {
                                    let filtered =
                                        filter_slash_entries(&slash_entries, &g.input_buffer);
                                    if !filtered.is_empty() {
                                        let pick = g.slash_menu_index.min(filtered.len() - 1);
                                        let cmd = filtered[pick].command_str();
                                        g.set_input_text(cmd);
                                    }
                                }
                            }
                            let line = g.take_input_text();
                            g.slash_menu_index = 0;
                            let active_approval = g.active_approval.clone();
                            let active_q = g.active_question.clone();
                            if let Some(req) = active_approval {
                                let t = line.trim();
                                if t.is_empty() {
                                    let call_id = req.call_id.clone();

                                    // If in hunk mode, apply partial selection.
                                    if g.approval_hunk_mode && !g.approval_hunk_selection.is_empty()
                                    {
                                        let modified_input = build_hunk_modified_input(
                                            &req.tool,
                                            &req.input,
                                            &g.approval_hunk_selection,
                                        );
                                        let all_selected =
                                            g.approval_hunk_selection.iter().all(|&s| s);
                                        g.approval_hunk_mode = false;
                                        g.approval_hunk_selection.clear();
                                        g.active_approval = None;
                                        g.approval_option_index = 0;
                                        drop(g);
                                        if let Some(ref tx) = approval_answer_tx {
                                            if all_selected {
                                                let _ = tx.send(ApprovalAnswer::Verdict {
                                                    call_id,
                                                    approved: true,
                                                });
                                            } else if let Some(modified) = modified_input {
                                                let _ = tx.send(ApprovalAnswer::ModifiedApproval {
                                                    call_id,
                                                    modified_input: modified,
                                                });
                                            } else {
                                                let _ = tx.send(ApprovalAnswer::Verdict {
                                                    call_id,
                                                    approved: true,
                                                });
                                            }
                                        }
                                        continue;
                                    }

                                    let selection = g.approval_option_index.min(2);
                                    g.active_approval = None;
                                    g.approval_option_index = 0;
                                    drop(g);
                                    if let Some(ref tx) = approval_answer_tx {
                                        match selection {
                                            0 => {
                                                let _ = tx.send(ApprovalAnswer::Verdict {
                                                    call_id,
                                                    approved: true,
                                                });
                                            }
                                            1 => {
                                                let pattern = req.allow_pattern();
                                                let _ = tx.send(ApprovalAnswer::AllowPattern {
                                                    call_id,
                                                    pattern,
                                                });
                                            }
                                            _ => {
                                                let _ = tx.send(ApprovalAnswer::Verdict {
                                                    call_id,
                                                    approved: false,
                                                });
                                            }
                                        }
                                    } else {
                                        let _ = cmd_tx.send(TuiCmd::CancelTurn);
                                    }
                                    continue;
                                }
                                if t.starts_with('/') {
                                    let lower = t.to_lowercase();
                                    let slash_verdict = match lower.as_str() {
                                        "/approve" | "/y" | "/yes" | "/ok" => Some(true),
                                        "/deny" | "/n" | "/no" => Some(false),
                                        _ => None,
                                    };
                                    if let Some(approved) = slash_verdict {
                                        let call_id = req.call_id.clone();
                                        g.active_approval = None;
                                        g.approval_option_index = 0;
                                        drop(g);
                                        if let Some(ref tx) = approval_answer_tx {
                                            let _ = tx.send(ApprovalAnswer::Verdict {
                                                call_id,
                                                approved,
                                            });
                                        } else {
                                            let _ = cmd_tx.send(TuiCmd::CancelTurn);
                                        }
                                        continue;
                                    }
                                    drop(g);
                                    let _ = cmd_tx.send(TuiCmd::Submit(line));
                                    continue;
                                }
                                if let Some(approved) = parse_approval_verdict(t) {
                                    let call_id = req.call_id.clone();
                                    g.active_approval = None;
                                    g.approval_option_index = 0;
                                    drop(g);
                                    if let Some(ref tx) = approval_answer_tx {
                                        let _ =
                                            tx.send(ApprovalAnswer::Verdict { call_id, approved });
                                    } else {
                                        let _ = cmd_tx.send(TuiCmd::CancelTurn);
                                    }
                                    continue;
                                }
                                g.push_block(DisplayBlock::System(
                                    "Could not parse approval — try y, n, yes, no, ok, deny, or Ctrl+Y / Ctrl+N."
                                        .into(),
                                ));
                                g.touch_transcript();
                                continue;
                            }
                            if let Some(ref q) = active_q {
                                let t = line.trim();
                                // `/auto-answer` must go through the side channel: `run_turn` is often
                                // blocked on this question, so `cmd_rx` is not polled for Submit.
                                if t == "/auto-answer" {
                                    let qid = q.question_id.clone();
                                    drop(g);
                                    if qid == STARTUP_APPROVE_ALL_QUESTION_ID {
                                        let _ = cmd_tx.send(TuiCmd::QuestionAnswer(
                                            QuestionSelection::Suggested,
                                        ));
                                    } else if let Some(ref tx) = question_answer_tx {
                                        let _ = tx.send((qid, QuestionSelection::Suggested));
                                    } else {
                                        let _ = cmd_tx.send(TuiCmd::QuestionAnswer(
                                            QuestionSelection::Suggested,
                                        ));
                                    }
                                    continue;
                                }
                                if t.starts_with('/') {
                                    drop(g);
                                    let _ = cmd_tx.send(TuiCmd::Submit(line));
                                    continue;
                                }
                                if let Some(sel) = parse_tui_question_answer(&line, q) {
                                    let qid = q.question_id.clone();
                                    drop(g);
                                    if qid == STARTUP_APPROVE_ALL_QUESTION_ID {
                                        let _ = cmd_tx.send(TuiCmd::QuestionAnswer(sel));
                                    } else if let Some(ref tx) = question_answer_tx {
                                        let _ = tx.send((qid, sel));
                                    } else {
                                        let _ = cmd_tx.send(TuiCmd::QuestionAnswer(sel));
                                    }
                                    continue;
                                }
                                g.push_block(DisplayBlock::System(
                                    "Invalid answer: use Enter/0 for suggested, 1–n for an option, or custom text."
                                        .into(),
                                ));
                                g.touch_transcript();
                                continue;
                            }
                            let expanded = expand_paste_tokens(&line, &g.paste_store);
                            g.paste_store.clear();
                            g.paste_counter = 0;
                            if !expanded.trim().is_empty()
                                && composer_history.last() != Some(&expanded)
                            {
                                composer_history.push(expanded.clone());
                                if composer_history.len() > 200 {
                                    composer_history.remove(0);
                                }
                            }
                            composer_history_index = None;
                            composer_history_draft.clear();
                            let busy_turn = g.busy || g.current_busy_state.is_active();
                            if busy_turn {
                                if mods.contains(KeyModifiers::ALT) {
                                    g.queued_followup = g.queued_followup.saturating_add(1);
                                    drop(g);
                                    let _ = cmd_tx.send(TuiCmd::QueueFollowUp(expanded));
                                } else {
                                    g.queued_steering = g.queued_steering.saturating_add(1);
                                    drop(g);
                                    let _ = cmd_tx.send(TuiCmd::QueueSteering(expanded));
                                }
                                continue;
                            }
                            if let Err(e) = stage_pasted_image_paths(&mut g, &expanded) {
                                g.push_error(format!("[image] {e}"));
                            }
                            drop(g);
                            let _ = cmd_tx.send(TuiCmd::Submit(expanded));
                        }
                        (KeyCode::Char('j'), KeyModifiers::CONTROL) => {
                            composer_history_index = None;
                            composer_history_draft.clear();
                            g.insert_input_char('\n');
                        }
                        (KeyCode::Char('a'), KeyModifiers::CONTROL) => {
                            g.move_input_home_line();
                        }
                        (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                            g.move_input_end_line();
                        }
                        // ── Word movement (Ctrl+←/→  and  Alt+B/F) ──────────
                        (KeyCode::Left, mods) if mods.contains(KeyModifiers::CONTROL) => {
                            g.move_input_word_backward();
                        }
                        (KeyCode::Right, mods) if mods.contains(KeyModifiers::CONTROL) => {
                            g.move_input_word_forward();
                        }
                        (KeyCode::Char('b'), KeyModifiers::ALT) => {
                            g.move_input_word_backward();
                        }
                        (KeyCode::Char('f'), KeyModifiers::ALT) => {
                            g.move_input_word_forward();
                        }
                        // ── Word deletion (Ctrl+W / Alt+Backspace / Alt+D) ───
                        (KeyCode::Char('w'), KeyModifiers::CONTROL) => {
                            g.delete_input_word_backward();
                        }
                        (KeyCode::Char('z'), KeyModifiers::CONTROL) => {
                            g.undo_input();
                        }
                        (KeyCode::Backspace, KeyModifiers::ALT) => {
                            g.delete_input_word_backward();
                        }
                        (KeyCode::Char('d'), KeyModifiers::ALT) => {
                            g.delete_input_word_forward();
                        }
                        // ── Home / End on current logical line ───────────────
                        (KeyCode::Home, _) => {
                            g.move_input_home_line();
                        }
                        (KeyCode::End, _) => {
                            g.move_input_end_line();
                        }
                        // ── Char movement ────────────────────────────────────
                        (KeyCode::Left, _) => {
                            g.move_input_left();
                        }
                        (KeyCode::Right, _) => {
                            g.move_input_right();
                        }
                        (KeyCode::PageUp, _) => {
                            if let Ok(sz) = terminal.size() {
                                let area = Rect::new(0, 0, sz.width, sz.height);
                                let (main_area, _) = layout_with_sidebar(area, g.sidebar_open);
                                let sh = composer_chrome_height(
                                    &slash_entries,
                                    &workspace_files,
                                    &g.input_buffer,
                                    g.cursor_char_idx,
                                );
                                let input_h = if should_hide_composer_when_scrolling(&g) {
                                    0
                                } else {
                                    composer_input_height(&g, main_area.width)
                                };
                                let (tr, _, _, _) = layout_chunks(
                                    main_area,
                                    sh,
                                    input_h,
                                    g.queue_preview_items.len(),
                                    g.subagents.len(),
                                );
                                let (lines, _hits, _) =
                                    transcript_cache.get_or_rebuild(&g, tr.width.saturating_sub(2));
                                let th = tr.height.saturating_sub(2) as usize;
                                let w = tr.width.saturating_sub(2) as usize;
                                scroll_buffer.replace_lines(lines.to_vec());
                                scroll_buffer.set_from_top(g.scroll_lines, th, w);
                                let step = th.max(scroll_speed as usize).max(1);
                                scroll_buffer.scroll_up(step, w, th);
                                let (from_top, _) = scroll_buffer.scroll_position_from_top(th, w);
                                g.scroll_lines = from_top as usize;
                                g.transcript_follow_tail = scroll_buffer.is_sticky();
                            }
                        }
                        (KeyCode::PageDown, _) => {
                            if let Ok(sz) = terminal.size() {
                                let area = Rect::new(0, 0, sz.width, sz.height);
                                let (main_area, _) = layout_with_sidebar(area, g.sidebar_open);
                                let sh = composer_chrome_height(
                                    &slash_entries,
                                    &workspace_files,
                                    &g.input_buffer,
                                    g.cursor_char_idx,
                                );
                                let input_h = if should_hide_composer_when_scrolling(&g) {
                                    0
                                } else {
                                    composer_input_height(&g, main_area.width)
                                };
                                let (tr, _, _, _) = layout_chunks(
                                    main_area,
                                    sh,
                                    input_h,
                                    g.queue_preview_items.len(),
                                    g.subagents.len(),
                                );
                                let (lines, _hits, _) =
                                    transcript_cache.get_or_rebuild(&g, tr.width.saturating_sub(2));
                                let th = tr.height.saturating_sub(2) as usize;
                                let w = tr.width.saturating_sub(2) as usize;
                                scroll_buffer.replace_lines(lines.to_vec());
                                scroll_buffer.set_from_top(g.scroll_lines, th, w);
                                let step = th.max(scroll_speed as usize).max(1);
                                scroll_buffer.scroll_down(step);
                                let (from_top, _) = scroll_buffer.scroll_position_from_top(th, w);
                                g.scroll_lines = from_top as usize;
                                g.transcript_follow_tail = scroll_buffer.is_sticky();
                            }
                        }
                        (KeyCode::Up, _) => {
                            let at_matches = at_completion_matches(
                                &workspace_files,
                                &g.input_buffer,
                                g.cursor_char_idx,
                            );
                            if !at_matches.is_empty()
                                && at_completion_active(&g.input_buffer, g.cursor_char_idx)
                            {
                                g.at_menu_index = g.at_menu_index.saturating_sub(1);
                            } else {
                                let slash_filtered =
                                    filter_slash_entries(&slash_entries, &g.input_buffer);
                                if !slash_filtered.is_empty()
                                    && slash_panel_visible(&g.input_buffer)
                                {
                                    g.slash_menu_index = g.slash_menu_index.saturating_sub(1);
                                } else {
                                    // Multiline: try moving up one visual line first.
                                    let composer_w = terminal
                                        .size()
                                        .map(|sz| sz.width.saturating_sub(8) as usize)
                                        .unwrap_or(80);
                                    let moved_up = g.move_input_up(composer_w);
                                    if moved_up {
                                        // Moved within the composer — don't touch history.
                                    } else {
                                        let can_history = !composer_history.is_empty()
                                            && (g.input_buffer.is_empty()
                                                || composer_history_index.is_some());
                                        if can_history {
                                            if composer_history_index.is_none() {
                                                composer_history_draft = g.input_buffer.clone();
                                                composer_history_index =
                                                    Some(composer_history.len().saturating_sub(1));
                                            } else if let Some(idx) = composer_history_index {
                                                composer_history_index =
                                                    Some(idx.saturating_sub(1));
                                            }
                                            if let Some(idx) = composer_history_index
                                                && let Some(entry) = composer_history.get(idx)
                                            {
                                                g.set_input_text(entry.clone());
                                            }
                                        } else {
                                            if let Ok(sz) = terminal.size() {
                                                let area = Rect::new(0, 0, sz.width, sz.height);
                                                let (main_area, _) =
                                                    layout_with_sidebar(area, g.sidebar_open);
                                                let sh = composer_chrome_height(
                                                    &slash_entries,
                                                    &workspace_files,
                                                    &g.input_buffer,
                                                    g.cursor_char_idx,
                                                );
                                                let input_h =
                                                    if should_hide_composer_when_scrolling(&g) {
                                                        0
                                                    } else {
                                                        composer_input_height(&g, main_area.width)
                                                    };
                                                let (tr, _, _, _) = layout_chunks(
                                                    main_area,
                                                    sh,
                                                    input_h,
                                                    g.queue_preview_items.len(),
                                                    g.subagents.len(),
                                                );
                                                let (lines, _hits, _) = transcript_cache
                                                    .get_or_rebuild(&g, tr.width.saturating_sub(2));
                                                let th = tr.height.saturating_sub(2) as usize;
                                                let w = tr.width.saturating_sub(2) as usize;
                                                scroll_buffer.replace_lines(lines.to_vec());
                                                scroll_buffer.set_from_top(g.scroll_lines, th, w);
                                                scroll_buffer.scroll_up(
                                                    scroll_speed as usize,
                                                    w,
                                                    th,
                                                );
                                                let (from_top, _) =
                                                    scroll_buffer.scroll_position_from_top(th, w);
                                                g.scroll_lines = from_top as usize;
                                                g.transcript_follow_tail =
                                                    scroll_buffer.is_sticky();
                                            }
                                        }
                                    } // end else (moved_up)
                                }
                            }
                        }
                        (KeyCode::Down, _) => {
                            let at_matches = at_completion_matches(
                                &workspace_files,
                                &g.input_buffer,
                                g.cursor_char_idx,
                            );
                            if !at_matches.is_empty()
                                && at_completion_active(&g.input_buffer, g.cursor_char_idx)
                            {
                                let n = at_matches.len();
                                g.at_menu_index = (g.at_menu_index + 1) % n;
                            } else {
                                let slash_filtered =
                                    filter_slash_entries(&slash_entries, &g.input_buffer);
                                if !slash_filtered.is_empty()
                                    && slash_panel_visible(&g.input_buffer)
                                {
                                    let n = slash_filtered.len();
                                    g.slash_menu_index = (g.slash_menu_index + 1) % n;
                                } else {
                                    // Multiline: try moving down one visual line first.
                                    let composer_w = terminal
                                        .size()
                                        .map(|sz| sz.width.saturating_sub(8) as usize)
                                        .unwrap_or(80);
                                    let moved_down = g.move_input_down(composer_w);
                                    if moved_down {
                                        // Moved within composer — leave history alone.
                                    } else {
                                        if let Some(idx) = composer_history_index {
                                            if idx + 1 < composer_history.len() {
                                                composer_history_index = Some(idx + 1);
                                                if let Some(entry) =
                                                    composer_history.get(idx.saturating_add(1))
                                                {
                                                    g.set_input_text(entry.clone());
                                                }
                                            } else {
                                                composer_history_index = None;
                                                g.set_input_text(composer_history_draft.clone());
                                            }
                                        } else {
                                            let sz = terminal.size().ok();
                                            if let Some(sz) = sz {
                                                let area = Rect::new(0, 0, sz.width, sz.height);
                                                let (main_area, _) =
                                                    layout_with_sidebar(area, g.sidebar_open);
                                                let sh = composer_chrome_height(
                                                    &slash_entries,
                                                    &workspace_files,
                                                    &g.input_buffer,
                                                    g.cursor_char_idx,
                                                );
                                                let input_h =
                                                    if should_hide_composer_when_scrolling(&g) {
                                                        0
                                                    } else {
                                                        composer_input_height(&g, main_area.width)
                                                    };
                                                let (tr, _, _, _) = layout_chunks(
                                                    main_area,
                                                    sh,
                                                    input_h,
                                                    g.queue_preview_items.len(),
                                                    g.subagents.len(),
                                                );
                                                let (lines, _hits, _) = transcript_cache
                                                    .get_or_rebuild(&g, tr.width.saturating_sub(2));
                                                let th = tr.height.saturating_sub(2) as usize;
                                                let w = tr.width.saturating_sub(2) as usize;
                                                scroll_buffer.replace_lines(lines.to_vec());
                                                scroll_buffer.set_from_top(g.scroll_lines, th, w);
                                                scroll_buffer.scroll_down(scroll_speed as usize);
                                                let (from_top, _) =
                                                    scroll_buffer.scroll_position_from_top(th, w);
                                                g.scroll_lines = from_top as usize;
                                                g.transcript_follow_tail =
                                                    scroll_buffer.is_sticky();
                                            }
                                        }
                                    } // end else (moved_down)
                                }
                            }
                        }
                        (KeyCode::Backspace, _) => {
                            composer_history_index = None;
                            composer_history_draft.clear();
                            if g.cursor_char_idx > 0 {
                                if let Some((buf, cidx)) =
                                    delete_completed_at_mention(&g.input_buffer, g.cursor_char_idx)
                                {
                                    g.set_input_text_with_cursor(buf, cidx);
                                } else {
                                    g.backspace_input();
                                }
                                if slash_panel_visible(&g.input_buffer) {
                                    let f = filter_slash_entries(&slash_entries, &g.input_buffer);
                                    if !f.is_empty() {
                                        g.slash_menu_index =
                                            g.slash_menu_index.min(f.len().saturating_sub(1));
                                    } else {
                                        g.slash_menu_index = 0;
                                    }
                                }
                            }
                        }
                        (KeyCode::Delete, _) => {
                            composer_history_index = None;
                            composer_history_draft.clear();
                            g.delete_input();
                        }
                        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                            composer_history_index = None;
                            composer_history_draft.clear();
                            g.insert_input_char(c);
                            if slash_panel_visible(&g.input_buffer) {
                                let f = filter_slash_entries(&slash_entries, &g.input_buffer);
                                if !f.is_empty() {
                                    g.slash_menu_index =
                                        g.slash_menu_index.min(f.len().saturating_sub(1));
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    restore_terminal(mouse_capture);
    let _ = execute!(stdout(), MoveToColumn(0));
    Ok(())
}

/// Open a URL in the default browser or a file path in the system editor.
fn open_link_in_system(target: &str) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", target])
            .spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(target).spawn()?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open").arg(target).spawn()?;
    }
    Ok(())
}

/// Returns a compact elapsed-time string like "5m" or "1h 23m".
fn session_time(started: std::time::Instant) -> String {
    let secs = started.elapsed().as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else {
        let h = secs / 3600;
        let m = (secs % 3600) / 60;
        format!("{h}h {m}m")
    }
}

/// Returns a spinner character based on elapsed time since the session started.
/// This gives a visible loading animation in the status bar when busy.
fn busy_spinner(started: std::time::Instant) -> &'static str {
    const SPINNERS: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let elapsed = started.elapsed().as_millis();
    let idx = (elapsed / 80) % SPINNERS.len() as u128;
    SPINNERS[idx as usize]
}
fn keybindings_cheatsheet() -> Vec<String> {
    vec![
        "─── Navigation ───────────────────────────────────────".into(),
        "  ↑ / ↓              Scroll transcript one line".into(),
        "  PgUp / PgDn        Scroll transcript one page".into(),
        "  Home / End         Jump to top / bottom of transcript".into(),
        "  Ctrl+F             Open transcript search".into(),
        "  n / N              Next / previous search match".into(),
        "  Ctrl+G             Go to bottom (follow tail)".into(),
        "".into(),
        "─── Composer ─────────────────────────────────────────".into(),
        "  Enter              Send message".into(),
        "  Shift+Enter        Insert newline".into(),
        "  Alt+Enter          Queue follow-up (when busy)".into(),
        "  ↑ / ↓              History prev/next (or move line when multiline)".into(),
        "  Ctrl+R             History search".into(),
        "  Ctrl+A / Home      Move to line start".into(),
        "  Ctrl+E / End       Move to line end".into(),
        "  Alt+B / Ctrl+←     Word backward".into(),
        "  Alt+F / Ctrl+→     Word forward".into(),
        "  Ctrl+W / Alt+BS    Delete word backward".into(),
        "  Alt+D              Delete word forward".into(),
        "  Ctrl+K             Kill to end of line".into(),
        "  Ctrl+Y             Yank (paste kill ring)".into(),
        "  Ctrl+U             Kill to start of line".into(),
        "".into(),
        "─── Tools ────────────────────────────────────────────".into(),
        "  Ctrl+O             Fold / unfold tool output".into(),
        "  Ctrl+X V           Full-screen transcript (expand/scroll/raw)".into(),
        "  Ctrl+X T           Toggle latest tool block collapsed/expanded".into(),
        "  Ctrl+X X           Ctrl+X leader (prefix for shortcuts below)".into(),
        "".into(),
        "─── Session ──────────────────────────────────────────".into(),
        "  Ctrl+L             Clear transcript".into(),
        "  Ctrl+T             Expand / collapse thinking".into(),
        "  Ctrl+K             Pin latest response (when composer empty)".into(),
        "  Ctrl+P             Open command palette".into(),
        "  Ctrl+C             Cancel current turn / exit (double-tap)".into(),
        "  Esc                Cancel / close modal".into(),
        "".into(),
        "─── Clipboard ────────────────────────────────────────".into(),
        "  F6                 Copy latest assistant response".into(),
        "  F7                 Copy latest tool output".into(),
        "  Click header       Copy that block's text".into(),
        "  Mouse drag         Select + copy range".into(),
        "".into(),
        "─── Models / Agents ──────────────────────────────────".into(),
        "  F2                 Cycle model forward".into(),
        "  Shift+F2           Cycle model backward".into(),
        "  F3                 Cycle agent profile".into(),
        "".into(),
        "─── Approvals ────────────────────────────────────────".into(),
        "  y / Ctrl+Y         Approve tool call".into(),
        "  n / Ctrl+N         Deny tool call".into(),
        "  Ctrl+U             Always allow (add to allowlist)".into(),
        "  ↑ / ↓              Move approval selection".into(),
        "".into(),
        "─── Modals & Overlays ────────────────────────────────".into(),
        "  F1                 This keybindings cheatsheet".into(),
        "  Ctrl+P             Command palette".into(),
        "  /theme             Theme picker".into(),
        "  /models            Model picker".into(),
        "  /connect           Provider connect modal".into(),
        "  /sessions          Session picker".into(),
        "  q / Esc            Close any modal".into(),
        "".into(),
        "─── Slash Commands (type / to see all) ───────────────".into(),
        "  /help              Full help text".into(),
        "  /status            Session + context info".into(),
        "  /undo              Undo last agent turn (git-stash backed)".into(),
        "  /redo              Redo last undone turn".into(),
        "  /retry             Re-send the last user message".into(),
        "  /run <cmd>         Run shell command; output staged as context".into(),
        "  /web <url>         Fetch URL; content staged as context".into(),
        "  /commit            AI-generated commit message for staged changes".into(),
        "  /map               Show workspace file tree".into(),
        "  /diff              Show recent git file changes".into(),
        "  /compact           Summarise and compact context".into(),
        "  /export            Export session to Markdown".into(),
        "  /clear             Clear transcript (keep context)".into(),
        "  /cost              Show token cost summary".into(),
        "  /thinking          Toggle extended thinking".into(),
        "  /plan              Switch to plan mode".into(),
    ]
}

#[cfg(test)]
mod approval_parse_tests {
    use super::{
        TuiCmd, apply_selected_at_completion, completed_at_mention_range_before_cursor,
        composer_line, delete_completed_at_mention, escape_cancels_active_turn, is_click_jitter,
        mouse_scroll_step, parse_approval_verdict, pasted_lines_token, request_turn_cancel,
        stage_pasted_image_paths, transcript_lines_and_hits,
    };
    use crate::tui::branch_picker::{branch_picker_enter_command, filtered_branch_indices};
    use crate::tui::diff_hunk::parse_diff_hunks;
    use crate::tui::markdown::{
        parse_md_line, render_markdown_lines, render_markdown_lines_with_hits,
    };
    use crate::tui::state::TuiSessionState;
    use crossterm::event::KeyModifiers;
    use dcode_ai_common::event::BusyState;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, atomic::AtomicBool};
    use tempfile::tempdir;

    #[test]
    fn parses_yes_with_punctuation_and_synonyms() {
        assert_eq!(parse_approval_verdict("yes"), Some(true));
        assert_eq!(parse_approval_verdict("Yes."), Some(true));
        assert_eq!(parse_approval_verdict("  OK! "), Some(true));
        assert_eq!(parse_approval_verdict("approve"), Some(true));
        assert_eq!(parse_approval_verdict("/approve"), Some(true));
        assert_eq!(parse_approval_verdict("/y"), Some(true));
    }

    #[test]
    fn parses_no_and_deny() {
        assert_eq!(parse_approval_verdict("n"), Some(false));
        assert_eq!(parse_approval_verdict("no."), Some(false));
        assert_eq!(parse_approval_verdict("deny"), Some(false));
        assert_eq!(parse_approval_verdict("/deny"), Some(false));
    }

    #[test]
    fn rejects_unknown() {
        assert_eq!(parse_approval_verdict("maybe"), None);
        assert_eq!(parse_approval_verdict("nope"), None);
        assert_eq!(parse_approval_verdict(""), None);
    }

    #[test]
    fn branch_picker_switches_exact_match_from_typed_query() {
        let branches = vec![
            "interactive-question".into(),
            "main".into(),
            "self-autoresearch".into(),
        ];
        let cmd = branch_picker_enter_command(&branches, "main", 0);
        assert!(matches!(cmd, Some(TuiCmd::SwitchBranch(name)) if name == "main"));
    }

    #[test]
    fn branch_picker_creates_only_with_slash_prefix() {
        let branches = vec!["main".into()];
        let cmd = branch_picker_enter_command(&branches, "/feature-x", 0);
        assert!(matches!(cmd, Some(TuiCmd::CreateBranch(name)) if name == "feature-x"));
    }

    #[test]
    fn branch_picker_filters_case_insensitively() {
        let branches = vec!["Main".into(), "feature/login".into()];
        assert_eq!(filtered_branch_indices(&branches, "main"), vec![0]);
        assert_eq!(filtered_branch_indices(&branches, "LOGIN"), vec![1]);
    }

    #[test]
    fn branch_picker_switches_selected_filtered_branch_by_name() {
        let branches = vec!["alpha".into(), "main".into(), "main-fix".into()];
        let cmd = branch_picker_enter_command(&branches, "mai", 1);
        assert!(matches!(cmd, Some(TuiCmd::SwitchBranch(name)) if name == "main-fix"));
    }

    #[test]
    fn mouse_scroll_step_uses_modifier_acceleration() {
        assert_eq!(mouse_scroll_step(KeyModifiers::NONE, 20, 3), 3);
        assert_eq!(mouse_scroll_step(KeyModifiers::SHIFT, 20, 3), 20);
        assert_eq!(mouse_scroll_step(KeyModifiers::CONTROL, 20, 3), 60);
    }

    #[test]
    fn click_jitter_allows_single_column_drift() {
        let sel = crate::tui::mouse_select::Selection {
            anchor: crate::tui::mouse_select::VisualPos { row: 4, col: 10 },
            cursor: crate::tui::mouse_select::VisualPos { row: 4, col: 11 },
            scroll_from_top: 0,
        };
        assert!(is_click_jitter(&sel));

        let sel_row_change = crate::tui::mouse_select::Selection {
            anchor: crate::tui::mouse_select::VisualPos { row: 4, col: 10 },
            cursor: crate::tui::mouse_select::VisualPos { row: 5, col: 10 },
            scroll_from_top: 0,
        };
        assert!(!is_click_jitter(&sel_row_change));
    }

    #[test]
    fn enter_accepts_selected_at_mention_without_submitting() {
        let workspace_files = vec![
            "crates/cli/src/file_mentions.rs".into(),
            "crates/cli/src/tui/app.rs".into(),
        ];
        let buffer = "check @crates/cli/src/t";
        let cursor_char_idx = buffer.chars().count();

        let (next_buffer, next_cursor_char_idx) =
            apply_selected_at_completion(&workspace_files, buffer, cursor_char_idx, 0, true)
                .expect("active mention should be selectable");

        assert_eq!(next_buffer, "check @crates/cli/src/tui/app.rs ");
        assert_eq!(next_cursor_char_idx, next_buffer.chars().count());
    }

    #[test]
    fn backspace_deletes_completed_at_mention_and_space() {
        let buffer = "check @crates/cli/src/tui/app.rs ";
        let cursor_char_idx = buffer.chars().count();

        let (next_buffer, next_cursor_char_idx) =
            delete_completed_at_mention(buffer, cursor_char_idx)
                .expect("completed mention should delete as one token");

        assert_eq!(next_buffer, "check ");
        assert_eq!(next_cursor_char_idx, "check ".chars().count());
    }

    #[test]
    fn mention_range_includes_inserted_trailing_space() {
        let buffer = "check @crates/cli/src/tui/app.rs ";
        let cursor_char_idx = buffer.chars().count();

        assert_eq!(
            completed_at_mention_range_before_cursor(buffer, cursor_char_idx),
            Some((6, buffer.chars().count()))
        );
    }

    #[test]
    fn composer_line_styles_completed_mentions() {
        let line = composer_line("see @README.md ", 15);
        let mention_span = line
            .spans
            .iter()
            .find(|span| span.content.contains("@README.md"))
            .expect("mention span should exist");

        assert_eq!(mention_span.style.bg, Some(super::theme::mention_bg()));
    }

    #[test]
    fn escape_cancels_active_turn_states_including_approval() {
        let mut state = TuiSessionState::new(
            "session".into(),
            "model".into(),
            "@build".into(),
            "AcceptEdits".into(),
            PathBuf::from("."),
            false,
        );
        assert!(!escape_cancels_active_turn(&state));

        state.set_busy_state(BusyState::Thinking);
        assert!(escape_cancels_active_turn(&state));

        state.set_busy_state(BusyState::Streaming);
        assert!(escape_cancels_active_turn(&state));

        state.set_busy_state(BusyState::ToolRunning);
        assert!(escape_cancels_active_turn(&state));

        state.set_busy_state(BusyState::ApprovalPending);
        assert!(escape_cancels_active_turn(&state));
    }

    #[test]
    fn parse_md_line_styles_inline_code_and_bold() {
        let line = parse_md_line("Use `cargo test` and **fix it**");
        let code = line
            .spans
            .iter()
            .find(|s| s.content == "cargo test")
            .expect("inline code span");
        assert_eq!(code.style.bg, Some(super::theme::surface()));

        let bold = line
            .spans
            .iter()
            .find(|s| s.content == "fix it")
            .expect("bold span");
        assert!(
            bold.style
                .add_modifier
                .contains(ratatui::style::Modifier::BOLD)
        );
    }

    #[test]
    fn parse_md_line_styles_heading_and_list_marker() {
        let heading = parse_md_line("## Heading");
        assert_eq!(heading.spans[0].content, "Heading");

        let list = parse_md_line("- item");
        assert_eq!(list.spans[0].content, "• ");
    }

    #[test]
    fn markdown_event_renderer_renders_heading_and_link() {
        let lines = render_markdown_lines("## Title\nSee [docs](https://example.com)");
        assert_eq!(lines[0].spans[0].content, "Title");
        let link_line = &lines[1];
        let link_span = link_line
            .spans
            .iter()
            .find(|s| s.content == "docs")
            .expect("link label");
        assert!(
            link_span
                .style
                .add_modifier
                .contains(ratatui::style::Modifier::UNDERLINED)
        );
    }

    #[test]
    fn parse_md_line_quote_hides_heading_marker() {
        let quoted_heading = parse_md_line("> ### Hidden marker");
        let rendered = quoted_heading
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>();
        assert_eq!(rendered, "▎ Hidden marker");
    }

    #[test]
    fn markdown_event_renderer_renders_list_and_code_fence() {
        let lines = render_markdown_lines("- one\n- two\n\n```rust\nlet x = 1;\n```");
        assert_eq!(lines[0].spans[0].content, "• ");
        assert_eq!(lines[1].spans[0].content, "• ");
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
                .contains("let x = 1;")
        }));
    }

    #[test]
    fn markdown_event_renderer_renders_table() {
        let lines =
            render_markdown_lines("| Name | Score |\n| --- | ---: |\n| Alice | 10 |\n| Bob | 7 |");
        let flat = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(flat.contains("Alice"));
        assert!(!flat.contains("┌"));
    }

    #[test]
    fn markdown_event_renderer_supports_code_line_numbers_and_copy_hits() {
        let (lines, hits) = render_markdown_lines_with_hits("```rs\nlet x = 1;\n```", true, 80);
        assert!(lines.iter().any(|line| {
            line.spans
                .iter()
                .map(|s| s.content.as_ref())
                .collect::<String>()
                .contains("1 │")
        }));
        assert!(
            hits.iter()
                .any(|h| matches!(h, Some(super::LineClickHit::CopyText(_))))
        );
    }

    #[test]
    fn markdown_event_renderer_styles_diff_lanes() {
        let (lines, _hits) = render_markdown_lines_with_hits(
            "```diff\n+add line\n-del line\n@@ hunk\n```",
            false,
            80,
        );
        let flat = lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(flat.contains("▌ +add line"));
        assert!(flat.contains("▌ -del line"));
    }

    /// Helper: flatten rendered lines to one string per line.
    fn flatten_md(lines: &[ratatui::text::Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn diff_change_counts_ignores_headers() {
        let diff =
            "--- before/x\n+++ after/x\n@@ -1,2 +1,3 @@\n ctx\n-old line\n+new line\n+extra line\n";
        assert_eq!(super::diff_change_counts(diff), (2, 1));
    }

    #[test]
    fn markdown_collapses_consecutive_blank_lines() {
        let lines = render_markdown_lines("alpha\n\n\n\nbravo");
        // No two adjacent fully-empty lines should survive.
        let mut prev_empty = false;
        for line in &lines {
            let empty = line.spans.is_empty();
            assert!(
                !(empty && prev_empty),
                "two consecutive blank lines rendered"
            );
            prev_empty = empty;
        }
        let flat = flatten_md(&lines).join("\n");
        assert!(flat.contains("alpha") && flat.contains("bravo"));
    }

    #[test]
    fn markdown_blockquote_renders_quote_marker() {
        let lines = render_markdown_lines("> quoted text");
        let flat = flatten_md(&lines).join("\n");
        assert!(flat.contains("quoted text"), "quote text missing: {flat}");
        assert!(flat.contains('▎'), "quote marker missing: {flat}");
    }

    #[test]
    fn markdown_nested_list_keeps_both_items() {
        let lines = render_markdown_lines("- outer\n    - inner");
        let flat = flatten_md(&lines).join("\n");
        assert!(flat.contains("outer"), "outer item missing: {flat}");
        assert!(flat.contains("inner"), "inner item missing: {flat}");
    }

    /// Build a minimal session state for transcript-render tests.
    fn transcript_test_state() -> TuiSessionState {
        TuiSessionState::new(
            "session".into(),
            "model".into(),
            "@build".into(),
            "AcceptEdits".into(),
            PathBuf::from("."),
            false,
        )
    }

    #[test]
    fn transcript_renders_user_system_and_error_text() {
        let mut state = transcript_test_state();
        state
            .blocks
            .push(super::DisplayBlock::User("ask me this".into()));
        state
            .blocks
            .push(super::DisplayBlock::System("a system note".into()));
        state
            .blocks
            .push(super::DisplayBlock::ErrorLine("boom failure".into()));

        let (lines, _hits) = transcript_lines_and_hits(&state, 80);
        let flat = flatten_md(&lines).join("\n");
        assert!(flat.contains("ask me this"), "user text missing: {flat}");
        assert!(
            flat.contains("a system note"),
            "system text missing: {flat}"
        );
        assert!(flat.contains("boom failure"), "error text missing: {flat}");
    }

    #[test]
    fn transcript_renders_tool_done_status_and_detail() {
        let mut state = transcript_test_state();
        state.push_block(super::DisplayBlock::ToolDone {
            name: "execute_bash".into(),
            call_id: "call-1".into(),
            ok: true,
            detail: "ls -la".into(),
            duration_ms: Some(1200),
        });

        let (lines, _hits) = transcript_lines_and_hits(&state, 80);
        let flat = flatten_md(&lines).join("\n");
        // The header shows a ● status dot; the body shows the command detail.
        assert!(flat.contains("●"), "status chip missing: {flat}");
        assert!(flat.contains("ls -la"), "tool detail missing: {flat}");
    }

    #[test]
    fn transcript_line_and_hit_counts_match() {
        // Every flattened line must have a parallel hit entry — the indices are
        // used together by mouse-click handling, so a mismatch is a real bug.
        let mut state = transcript_test_state();
        state.push_block(super::DisplayBlock::User("hi".into()));
        state
            .blocks
            .push(super::DisplayBlock::Assistant("hello there".into()));
        state
            .blocks
            .push(super::DisplayBlock::System("note".into()));

        let (lines, hits) = transcript_lines_and_hits(&state, 80);
        assert_eq!(lines.len(), hits.len(), "lines and hits must stay aligned");
    }

    #[test]
    fn pasted_lines_token_counts_multiline_input() {
        assert_eq!(
            pasted_lines_token("a\nb\nc", 1),
            Some("[pasted 3 lines #1]".into())
        );
        assert_eq!(pasted_lines_token("single line", 1), None);
    }

    #[test]
    fn stage_pasted_image_paths_imports_single_path_line() {
        let workspace = tempdir().expect("temp workspace");
        let src_dir = workspace.path().join("assets");
        fs::create_dir_all(&src_dir).expect("create src dir");
        let src = src_dir.join("drop-test.png");
        fs::write(&src, b"not-really-a-png").expect("write source image");

        let mut state = TuiSessionState::new(
            "s-test".into(),
            "model".into(),
            "@build".into(),
            "AcceptEdits".into(),
            workspace.path().to_path_buf(),
            false,
        );

        let staged = stage_pasted_image_paths(&mut state, "./assets/drop-test.png")
            .expect("staging should not error");
        assert_eq!(staged, 1);
        assert_eq!(state.staged_image_attachments.len(), 1);

        let rel = &state.staged_image_attachments[0].path;
        assert!(workspace.path().join(rel).is_file());
    }

    #[test]
    fn request_turn_cancel_is_idempotent() {
        let mut state = TuiSessionState::new(
            "session".into(),
            "model".into(),
            "@build".into(),
            "AcceptEdits".into(),
            PathBuf::from("."),
            false,
        );
        state.set_busy_state(BusyState::Streaming);
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<TuiCmd>();
        let cancel = Arc::new(AtomicBool::new(false));

        request_turn_cancel(&mut state, Some(&cancel), &tx);
        request_turn_cancel(&mut state, Some(&cancel), &tx);

        let first = rx.try_recv().ok();
        let second = rx.try_recv().ok();
        assert!(matches!(first, Some(TuiCmd::CancelTurn)));
        assert!(second.is_none());
        let cancel_msgs = state
            .blocks
            .iter()
            .filter(
                |b| matches!(b, super::DisplayBlock::System(s) if s == "Cancelling current run..."),
            )
            .count();
        assert_eq!(cancel_msgs, 1);
    }

    // ── Snapshot tests (insta) ─────────────────────────────────────────

    fn lines_to_text(lines: &[ratatui::text::Line<'_>]) -> String {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn snapshot_transcript_user_and_assistant() {
        let mut st = TuiSessionState::new(
            "test-snap".into(),
            "test-model".into(),
            "@build".into(),
            "default".into(),
            PathBuf::from("/tmp"),
            false,
        );
        st.blocks
            .push(super::DisplayBlock::User("Hello world".into()));
        st.blocks.push(super::DisplayBlock::Assistant(
            "Hi! How can I help you today?".into(),
        ));
        st.touch_transcript();
        let (lines, _hits) = transcript_lines_and_hits(&st, 60);
        insta::assert_snapshot!("transcript_user_assistant", lines_to_text(&lines));
    }

    #[test]
    fn long_user_message_shows_marker_only_on_first_row() {
        let mut st = TuiSessionState::new(
            "test-wrap".into(),
            "test-model".into(),
            "@build".into(),
            "default".into(),
            PathBuf::from("/tmp"),
            false,
        );
        let long = "one paragraph pasted by the user that easily wraps across \
                    several transcript rows at a narrow terminal width because \
                    it just keeps going and going without a newline";
        st.blocks.push(super::DisplayBlock::User(long.into()));
        st.touch_transcript();
        let (lines, _hits) = transcript_lines_and_hits(&st, 40);
        let texts: Vec<String> = lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        let marker_rows = texts.iter().filter(|t| t.starts_with("› ")).count();
        let continuation_rows = texts
            .iter()
            .filter(|t| t.starts_with("  ") && !t.trim().is_empty())
            .count();
        assert_eq!(marker_rows, 1, "exactly one `›` marker: {texts:#?}");
        assert!(
            continuation_rows >= 2,
            "wrapped continuations indented: {texts:#?}"
        );
    }

    #[test]
    fn snapshot_transcript_tool_done() {
        let mut st = TuiSessionState::new(
            "test-snap".into(),
            "test-model".into(),
            "@build".into(),
            "default".into(),
            PathBuf::from("/tmp"),
            false,
        );
        st.blocks.push(super::DisplayBlock::ToolDone {
            name: "execute_bash".into(),
            call_id: "call-1".into(),
            ok: true,
            detail: "hello world\nexit code: 0".into(),
            duration_ms: Some(150),
        });
        st.touch_transcript();
        let (lines, _hits) = transcript_lines_and_hits(&st, 60);
        insta::assert_snapshot!("transcript_tool_done", lines_to_text(&lines));
    }

    #[test]
    fn snapshot_markdown_code_block() {
        let md = "Here's some code:\n\n```rust\nfn main() {\n    println!(\"hello\");\n}\n```\n\nThat's it.";
        let lines = render_markdown_lines(md);
        insta::assert_snapshot!("markdown_code_block", lines_to_text(&lines));
    }

    #[test]
    fn snapshot_markdown_table() {
        let md = "| Name | Value |\n|------|-------|\n| foo  | 42    |\n| bar  | 99    |";
        let lines = render_markdown_lines(md);
        insta::assert_snapshot!("markdown_table", lines_to_text(&lines));
    }

    #[test]
    fn snapshot_composer_with_mention() {
        let line = composer_line("check @src/main.rs for errors", 30);
        let text = line
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect::<String>();
        insta::assert_snapshot!("composer_mention", text);
    }

    #[test]
    fn snapshot_context_gauge() {
        use crate::tui::widgets::status_bar::context_gauge_spans;
        let spans = context_gauge_spans(64_000, "gpt-4o");
        let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
        insta::assert_snapshot!("context_gauge_50pct", text);
    }

    #[test]
    fn snapshot_diff_hunks() {
        let old = "line1\nline2\nline3\nline4\nline5\n";
        let new = "line1\nmodified2\nline3\nnew_line\nline4\nline5\n";
        let hunks = parse_diff_hunks(old, new);
        let text: String = hunks
            .iter()
            .map(|h| {
                let lines: String = h
                    .lines
                    .iter()
                    .map(|(s, t)| format!("{s}{t}"))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("{}\n{}", h.header, lines)
            })
            .collect::<Vec<_>>()
            .join("\n---\n");
        insta::assert_snapshot!("diff_hunks", text);
    }
}
