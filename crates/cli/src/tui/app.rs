//! Full-screen session TUI: transcript, streaming assistant, composer.

#![allow(clippy::collapsible_match, clippy::explicit_into_iter_loop)]

use std::collections::HashSet;
use std::path::PathBuf;

use crate::file_mentions;
use crate::tool_ui;
use crate::tui::answer_parse::{parse_approval_verdict, parse_tui_question_answer};
use crate::tui::branch_picker::filtered_branch_indices;
use crate::tui::composer_input::*;
use crate::tui::connect_modal::{
    ConnectAction, ConnectRow, build_connect_rows, clamp_selection, row_index_for_selection,
    selectable_row_indices, selection_pulse, status_dots, title_sparkle,
};
use crate::tui::layout::{centered_rect, layout_chunks, layout_with_sidebar, sidebar_fit};
use crate::tui::mouse::{is_click_jitter, mouse_left_activated, mouse_scroll_step, rect_contains};
use crate::tui::oauth_status::{
    oauth_logged_in_for_slug, oauth_login_provider_slug, oauth_switch_command_for_slug,
};
use crate::tui::palette::{
    PaletteRow, filter_palette_rows, palette_command_for_label, palette_selectable_indices,
};
use crate::tui::paste::{expand_paste_tokens, pasted_lines_token};
use crate::tui::path_parse::{extract_embedded_path_fragments, parse_candidate_image_path};
use crate::tui::render_helpers::{
    char_window, permission_mode_pill, progress_bar, subagent_phase_progress, truncate_chars,
    wrap_preformatted_line, wrap_text,
};
use crate::tui::slash_entries::*;
use crate::tui::state::{
    ApprovalRequest, BranchPickerKey, CommandPaletteKey, ConnectModalKey, DisplayBlock,
    HistorySearchKey, InfoModalKey, ModelPickerAction, ModelPickerKey, PinnedNote, PinsModalKey,
    ProviderPickerKey, ProviderPickerOutcome, QuestionModalKey, QuestionModalOutcome,
    SessionPickerKey, TuiSessionState,
};
use crate::tui::terminal::{restore_terminal, setup_terminal};
use crate::tui::transcript::transcript_lines_and_hits;
use arboard::Clipboard;
use crossterm::{
    cursor::MoveToColumn,
    event::{Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind, poll, read},
    execute,
};
use dcode_ai_common::auth::AuthStore;
use dcode_ai_common::config::ProviderKind;
use dcode_ai_common::event::{BusyState, QuestionSelection};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, Borders, Clear as ClearWidget, Padding, Paragraph, Scrollbar, ScrollbarOrientation,
        ScrollbarState, Wrap,
    },
};
use std::io::stdout;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

const STARTUP_APPROVE_ALL_QUESTION_ID: &str = "startup-approve-all";

/// Message from TUI to the approval dispatch task.
#[derive(Debug)]
pub enum ApprovalAnswer {
    Verdict { call_id: String, approved: bool },
    AllowPattern { call_id: String, pattern: String },
}

#[derive(Debug, Clone)]
pub(crate) enum LineClickHit {
    Question(QuestionSelection),
    CopyText(String),
    /// Toggle full vs preview rendering of thinking blocks.
    ToggleThinking,
}

/// Per flattened transcript line: click action (same indices as `transcript_lines`).
pub(crate) type LineAnswerHit = Option<LineClickHit>;

#[derive(Debug)]
pub enum TuiCmd {
    Submit(String),
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
}

use super::theme;

const COMMAND_PALETTE_WIDTH: u16 = 56;
const COMMAND_PALETTE_MAX_ROWS: usize = 10;

pub fn session_start_banner() -> String {
    [
        "   ___",
        "  /   \\",
        " | x x |",
        " |  ^  |   dcode-ai",
        " |_____|",
        "  |   |",
    ]
    .join("\n")
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
        state.blocks.push(DisplayBlock::System(format!(
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

fn line_has_text(line: &Line<'_>) -> bool {
    line.spans.iter().any(|s| !s.content.trim().is_empty())
}

pub(crate) fn push_section_gap(lines: &mut Vec<Line<'static>>, hits: &mut Vec<LineAnswerHit>) {
    if lines.last().is_some_and(line_has_text) {
        push_transcript_line(lines, hits, Line::default(), None);
    }
}

#[derive(Default)]
struct TranscriptRenderCache {
    width: u16,
    revision: u64,
    code_line_numbers: bool,
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
    fn get_or_rebuild<'a>(
        &'a mut self,
        state: &TuiSessionState,
        width: u16,
    ) -> (&'a [Line<'static>], &'a [LineAnswerHit]) {
        let normalized_width = width.max(20);
        if self.width != normalized_width
            || self.revision != state.transcript_rev
            || self.code_line_numbers != state.code_line_numbers
        {
            let (lines, hits) = transcript_lines_and_hits(state, normalized_width);
            self.width = normalized_width;
            self.revision = state.transcript_rev;
            self.code_line_numbers = state.code_line_numbers;
            self.lines = lines;
            self.hits = hits;
        }
        (&self.lines, &self.hits)
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

pub(crate) fn push_tool_detail_lines(
    lines: &mut Vec<Line<'static>>,
    hits: &mut Vec<LineAnswerHit>,
    detail: &str,
    width: usize,
    max_lines: usize,
) {
    fn wrap_tool_detail_text(text: &str, width: usize) -> Vec<String> {
        if width < 8 {
            return vec![text.to_string()];
        }
        // Preserve code/diff alignment (Ironclaw/Koda style):
        // tool-detail lanes use hard wrapping, never word-wrap.
        wrap_preformatted_line(text, width)
    }

    let mut rendered = 0usize;
    let mut omitted = 0usize;
    let mut diff_old_line: Option<usize> = None;
    let mut diff_new_line: Option<usize> = None;
    let has_diff_payload = detail.lines().any(|l| {
        l.starts_with("diff ")
            || l.starts_with("@@")
            || l.starts_with("+++")
            || l.starts_with("---")
            || (l.starts_with('+') && !l.starts_with("+++"))
            || (l.starts_with('-') && !l.starts_with("---"))
    });
    // Prepend a `+adds −dels` stat line so the scale of a change reads at a glance.
    if has_diff_payload {
        let (adds, dels) = diff_change_counts(detail);
        if adds > 0 || dels > 0 {
            push_transcript_line(
                lines,
                hits,
                Line::from(vec![
                    Span::styled("  ", Style::default().fg(theme::muted())),
                    Span::styled(
                        format!("+{adds}"),
                        Style::default()
                            .fg(theme::success())
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(
                        format!("−{dels}"),
                        Style::default()
                            .fg(theme::error())
                            .add_modifier(Modifier::BOLD),
                    ),
                ]),
                None,
            );
            rendered += 1;
        }
    }
    for source_line in detail.lines() {
        if let Some((old_start, new_start)) = parse_unified_hunk_header(source_line) {
            diff_old_line = Some(old_start);
            diff_new_line = Some(new_start);
        }
        let is_note = source_line.starts_with("Wrote ")
            || source_line.starts_with("Edited ")
            || source_line.starts_with("Patched ")
            || source_line.starts_with("Replaced match at ");
        if has_diff_payload && is_note {
            continue;
        }
        let is_meta = source_line.starts_with("+++")
            || source_line.starts_with("---")
            || source_line.starts_with("@@")
            || source_line.starts_with("diff ")
            || source_line.starts_with("index ")
            || source_line.starts_with("new file mode ")
            || source_line.starts_with("deleted file mode ")
            || source_line.starts_with("rename from ")
            || source_line.starts_with("rename to ");
        let is_add = source_line.starts_with('+') && !source_line.starts_with("+++");
        let is_del = source_line.starts_with('-') && !source_line.starts_with("---");
        let is_ctx = source_line.starts_with(' ');
        let (lane, lane_style, text_style, payload) = if is_note {
            (
                "▎ ",
                Style::default().fg(theme::assistant()),
                Style::default()
                    .fg(theme::assistant())
                    .add_modifier(Modifier::BOLD),
                source_line.to_string(),
            )
        } else if is_meta {
            (
                "┆ ",
                Style::default().fg(theme::warn()),
                Style::default()
                    .fg(theme::warn())
                    .add_modifier(Modifier::BOLD),
                source_line.to_string(),
            )
        } else if is_add {
            (
                "+ ",
                Style::default()
                    .fg(theme::success())
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(theme::success()),
                source_line
                    .strip_prefix('+')
                    .unwrap_or(source_line)
                    .to_string(),
            )
        } else if is_del {
            (
                "- ",
                Style::default()
                    .fg(theme::error())
                    .add_modifier(Modifier::BOLD),
                Style::default().fg(theme::error()),
                source_line
                    .strip_prefix('-')
                    .unwrap_or(source_line)
                    .to_string(),
            )
        } else if is_ctx {
            (
                "  ",
                Style::default().fg(theme::muted()),
                Style::default().fg(theme::text()),
                source_line
                    .strip_prefix(' ')
                    .unwrap_or(source_line)
                    .to_string(),
            )
        } else {
            (
                "│ ",
                Style::default().fg(theme::muted()),
                Style::default().fg(theme::text()),
                source_line.to_string(),
            )
        };
        let wrapped = wrap_tool_detail_text(&payload, width.max(8));
        for (idx, line) in wrapped.into_iter().enumerate() {
            if rendered < max_lines {
                let cont_lane = if idx == 0 { lane } else { "  " };
                let gutter = if idx == 0 {
                    if is_add {
                        let g = format!(
                            "{:>4} {:>4} ",
                            "",
                            diff_new_line.map(|n| n.to_string()).unwrap_or_default()
                        );
                        if let Some(n) = diff_new_line.as_mut() {
                            *n += 1;
                        }
                        g
                    } else if is_del {
                        let g = format!(
                            "{:>4} {:>4} ",
                            diff_old_line.map(|n| n.to_string()).unwrap_or_default(),
                            ""
                        );
                        if let Some(n) = diff_old_line.as_mut() {
                            *n += 1;
                        }
                        g
                    } else if is_ctx {
                        let g = format!(
                            "{:>4} {:>4} ",
                            diff_old_line.map(|n| n.to_string()).unwrap_or_default(),
                            diff_new_line.map(|n| n.to_string()).unwrap_or_default()
                        );
                        if let Some(n) = diff_old_line.as_mut() {
                            *n += 1;
                        }
                        if let Some(n) = diff_new_line.as_mut() {
                            *n += 1;
                        }
                        g
                    } else {
                        "          ".to_string()
                    }
                } else {
                    "          ".to_string()
                };
                push_transcript_line(
                    lines,
                    hits,
                    Line::from(vec![
                        Span::styled("  ", Style::default().fg(theme::muted())),
                        Span::styled(gutter, Style::default().fg(theme::muted())),
                        Span::styled(
                            cont_lane,
                            if idx == 0 {
                                lane_style
                            } else {
                                Style::default().fg(theme::muted())
                            },
                        ),
                        Span::styled(line, text_style),
                    ]),
                    None,
                );
            } else {
                omitted += 1;
            }
            rendered += 1;
        }
        if source_line.is_empty() {
            if rendered < max_lines {
                push_transcript_line(lines, hits, Line::default(), None);
            } else {
                omitted += 1;
            }
            rendered += 1;
        }
    }
    if omitted > 0 {
        push_transcript_line(
            lines,
            hits,
            Line::from(Span::styled(
                format!("  … +{omitted} lines"),
                Style::default().fg(theme::muted()),
            )),
            None,
        );
    }
}

fn parse_unified_hunk_header(line: &str) -> Option<(usize, usize)> {
    // @@ -old_start,old_count +new_start,new_count @@
    let trimmed = line.trim();
    if !trimmed.starts_with("@@") {
        return None;
    }
    let rest = trimmed.strip_prefix("@@")?.trim_start();
    let end = rest.find("@@")?;
    let body = rest[..end].trim();
    let mut parts = body.split_whitespace();
    let old = parts.next()?;
    let new = parts.next()?;
    let old_start = old
        .strip_prefix('-')?
        .split(',')
        .next()?
        .parse::<usize>()
        .ok()?;
    let new_start = new
        .strip_prefix('+')?
        .split(',')
        .next()?
        .parse::<usize>()
        .ok()?;
    Some((old_start, new_start))
}

/// Render the approval request as a centered popup overlay. The popup body
/// reuses the same content layout as `render_approval_block` (icon + label,
/// description, full input, action hint) but lives above the transcript so
/// the user can't miss it while scrolling tool output.
fn render_approval_popup(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    req: &ApprovalRequest,
    selected: usize,
) {
    // Keep approval modal compact (Koda/Ironclaw style), not a large sheet.
    let max_w = area.width.saturating_sub(2);
    let popup_w = max_w.min(84).max(max_w.min(56));
    let max_h = area.height.saturating_sub(2);
    let popup_h = max_h.min(13).max(max_h.min(9));
    let popup_area = centered_rect(area, popup_w, popup_h);

    let selected = selected.min(2);
    let inner_w = popup_area.width.saturating_sub(4) as usize;
    let ui = tool_ui::metadata(&req.tool);
    let preview = tool_input_preview(&req.tool, &req.input);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("◆ ", Style::default().fg(theme::success())),
        Span::styled(
            ui.label,
            Style::default()
                .fg(theme::success())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" requires approval", Style::default().fg(theme::muted())),
    ]));
    lines.push(Line::default());
    lines.push(Line::from(vec![
        Span::styled("name:", Style::default().fg(theme::success())),
        Span::styled(" ", Style::default()),
        Span::styled(
            format!("\"{}\"", req.tool),
            Style::default().fg(theme::text()),
        ),
    ]));
    lines.push(Line::default());

    let option_row = |idx: usize, label: &str| -> Line<'static> {
        let active = selected == idx;
        let marker = if active { "● " } else { "○ " };
        let mut style = Style::default().fg(theme::muted());
        if active {
            style = Style::default()
                .fg(theme::success())
                .add_modifier(Modifier::BOLD);
        }
        Line::from(vec![
            Span::styled(marker, style),
            Span::styled(label.to_string(), style),
        ])
    };
    lines.push(option_row(0, "Approve (y)"));
    lines.push(option_row(1, "Always approve (a)"));
    lines.push(option_row(2, "Deny (n)"));
    lines.push(Line::default());

    if !preview.is_empty() {
        lines.push(Line::from(Span::styled(
            truncate_chars(
                &format!("input: {preview}"),
                inner_w.saturating_sub(2).max(24),
            ),
            Style::default().fg(theme::muted()),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            truncate_chars(&req.description, inner_w.saturating_sub(2).max(24)),
            Style::default().fg(theme::muted()),
        )));
    }
    lines.push(Line::from(Span::styled(
        "↑↓ select  Enter confirm  Esc cancel",
        Style::default().fg(theme::muted()),
    )));

    frame.render_widget(ClearWidget, popup_area);
    let popup = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::warn()))
                .title(Span::styled(
                    " Tool Approval ",
                    Style::default()
                        .fg(theme::warn())
                        .add_modifier(Modifier::BOLD),
                )),
        )
        .style(Style::default().bg(theme::surface()))
        .wrap(Wrap { trim: false });
    frame.render_widget(popup, popup_area);
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

    // Load slash entries once: hardcoded commands + discovered skills
    let skill_dirs = vec![PathBuf::from(".dcode-ai/skills")];
    let workspace_root = {
        let g = state.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        g.workspace_root.clone()
    };
    let slash_entries = load_slash_entries(&workspace_root, &skill_dirs);
    let workspace_files = file_mentions::discover_workspace_files(&workspace_root);
    let mut transcript_cache = TranscriptRenderCache::default();
    let mut scroll_buffer = crate::tui::scroll_buffer::ScrollBuffer::default();
    let mut composer_history: Vec<String> = Vec::new();
    let mut composer_history_index: Option<usize> = None;
    let mut composer_history_draft = String::new();
    let mut ctrl_c_armed_at: Option<std::time::Instant> = None;

    if let Ok(mut g) = state.lock() {
        // Show logo on fresh sessions (empty transcript) and in explicit run mode.
        let should_show_banner = show_run_banner || g.blocks.is_empty();
        if should_show_banner {
            let banner = session_start_banner();
            let already_present = g
                .blocks
                .iter()
                .any(|b| matches!(b, DisplayBlock::System(s) if s == &banner));
            if !already_present {
                g.blocks.push(DisplayBlock::System(banner));
                g.blocks.push(DisplayBlock::System(
                    "Interactive run — type a message. Ctrl+P commands, /keymaps shortcuts.".into(),
                ));
                g.touch_transcript();
            }
        }
    }

    loop {
        {
            let mut g = state.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
            if g.should_exit {
                break;
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

            terminal.draw(|frame| {
                let area = frame.area();
                let (main_area, sidebar_opt) = layout_with_sidebar(area, g.sidebar_open);
                let input_h = if should_hide_composer_when_scrolling(&g) {
                    0
                } else {
                    composer_input_height(&g, main_area.width)
                };
                let vp = crate::tui::tui_viewport::layout(
                    main_area,
                    chrome_h,
                    input_h,
                    g.queue_preview_items.len(),
                    g.subagents.len(),
                );
                let (tr, st_r, slash_opt, inp_r) = (vp.transcript, vp.status, vp.slash, vp.input);

                let transcript_h = tr.height.saturating_sub(2) as usize;
                let inner_w = tr.width.saturating_sub(2);
                let (lines, _hits) = transcript_cache.get_or_rebuild(&g, inner_w);
                scroll_buffer.replace_lines(lines.to_vec());
                if g.transcript_follow_tail {
                    scroll_buffer.scroll_to_bottom();
                } else {
                    scroll_buffer.set_from_top(g.scroll_lines, transcript_h, inner_w as usize);
                }
                let (from_top, _) = scroll_buffer.scroll_position_from_top(transcript_h, inner_w as usize);
                g.scroll_lines = from_top as usize;
                let total = scroll_buffer.len();
                let max_scroll = total.saturating_sub(transcript_h);

                let search_matches = transcript_search_matches(lines, &g.transcript_search_query);
                if !search_matches.is_empty() {
                    g.transcript_search_index =
                        g.transcript_search_index.min(search_matches.len().saturating_sub(1));
                    if g.transcript_search_open {
                        let target = search_matches[g.transcript_search_index];
                        if target < g.scroll_lines {
                            g.scroll_lines = target;
                            g.transcript_follow_tail = false;
                            scroll_buffer.set_from_top(
                                g.scroll_lines,
                                transcript_h,
                                inner_w as usize,
                            );
                        } else {
                            let bottom = g.scroll_lines.saturating_add(transcript_h.max(1));
                            if target >= bottom {
                                g.scroll_lines = target
                                    .saturating_sub(transcript_h.saturating_sub(1))
                                    .min(max_scroll);
                                g.transcript_follow_tail = false;
                                scroll_buffer.set_from_top(
                                    g.scroll_lines,
                                    transcript_h,
                                    inner_w as usize,
                                );
                            }
                        }
                    }
                } else {
                    g.transcript_search_index = 0;
                }

                let start = g.scroll_lines;
                let end = (start + transcript_h).min(total);
                let mut visible: Vec<Line> = if start < end {
                    lines[start..end].to_vec()
                } else {
                    vec![]
                };
                if !search_matches.is_empty() {
                    let match_set: HashSet<usize> = search_matches.iter().copied().collect();
                    let active_match_line = search_matches
                        .get(g.transcript_search_index)
                        .copied()
                        .unwrap_or(search_matches[0]);
                    for (row, line) in visible.iter_mut().enumerate() {
                        let gline = start + row;
                        if match_set.contains(&gline) {
                            let style = if gline == active_match_line {
                                Style::default().bg(theme::assistant()).fg(Color::Black)
                            } else {
                                Style::default().bg(theme::warn()).fg(Color::Black)
                            };
                            for span in &mut line.spans {
                                span.style = span.style.patch(style);
                            }
                        }
                    }
                }

                // Apply mouse-selection highlight, if any. Selection rows are
                // in buffer-space; shift into the visible slice's local coords
                // before delegating to mouse_select::apply_selection_highlight.
                if let Some(ref sel) = g.mouse_selection {
                    let (sel_start, sel_end) = sel.ordered();
                    let visible_start = start;
                    let visible_end = end.saturating_sub(1);
                    let overlaps_visible = (sel_end.row as usize) >= visible_start
                        && (sel_start.row as usize) <= visible_end;
                    if overlaps_visible {
                        let inner_w = tr.width.saturating_sub(2) as usize;
                        let start_u16 = start.min(u16::MAX as usize) as u16;
                        let local_anchor = sel.anchor.row.saturating_sub(start_u16);
                        let local_cursor = sel.cursor.row.saturating_sub(start_u16);
                        let local_sel = crate::tui::mouse_select::Selection {
                            anchor: crate::tui::mouse_select::VisualPos {
                                row: local_anchor,
                                col: sel.anchor.col,
                            },
                            cursor: crate::tui::mouse_select::VisualPos {
                                row: local_cursor,
                                col: sel.cursor.col,
                            },
                            scroll_from_top: sel.scroll_from_top,
                        };
                        visible = crate::tui::mouse_select::apply_selection_highlight(
                            visible,
                            &local_sel,
                            inner_w.max(1),
                        );
                    }
                }

                let scroll_info = if g.scroll_lines > 0 || !g.transcript_follow_tail {
                    format!(" lines {}–{} of {} ", start + 1, end.min(total), total)
                } else {
                    format!(" {} lines ", total)
                };
                let search_info = if !search_matches.is_empty() {
                    format!(
                        " · find {}/{}",
                        g.transcript_search_index + 1,
                        search_matches.len()
                    )
                } else if !g.transcript_search_query.trim().is_empty() {
                    " · find 0".to_string()
                } else {
                    String::new()
                };
                let title = format!(
                    " transcript —{scroll_info}{search_info}(wheel scroll · Ctrl+F find) ",
                );
                let main = Paragraph::new(Text::from(visible))
                    .block(
                        Block::default()
                            .borders(Borders::TOP | Borders::LEFT | Borders::RIGHT)
                            .border_style(Style::default().fg(theme::border()))
                            .title(Span::styled(title, Style::default().fg(theme::muted()))),
                    )
                    .style(Style::default().bg(theme::bg()));

                frame.render_widget(main, tr);

                // Scrollbar gutter on the transcript's right edge — only when the
                // content overflows the viewport. Skips the top border row so the
                // track aligns with text rows, not the title.
                if total > transcript_h && tr.height > 2 {
                    let mut sb_state =
                        ScrollbarState::new(max_scroll.max(1)).position(start.min(max_scroll));
                    let sb_area = Rect::new(
                        tr.x + tr.width.saturating_sub(1),
                        tr.y + 1,
                        1,
                        tr.height.saturating_sub(1),
                    );
                    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
                        .begin_symbol(None)
                        .end_symbol(None)
                        .track_symbol(Some("│"))
                        .thumb_symbol("█")
                        .track_style(Style::default().fg(theme::border()))
                        .thumb_style(Style::default().fg(theme::muted()));
                    frame.render_stateful_widget(scrollbar, sb_area, &mut sb_state);
                }

                let activity_rows = g
                    .subagents
                    .iter()
                    .map(|row| crate::tui::widgets::child_activity_overlay::ActivityRow {
                        id: row.id.clone(),
                        phase: row.phase.clone(),
                        detail: row.detail.clone(),
                        running: row.running,
                    })
                    .collect::<Vec<_>>();
                crate::tui::tui_viewport::render_activity_overlay(
                    frame,
                    vp.activity_overlay,
                    &activity_rows,
                    g.subagents.len(),
                );
                crate::tui::tui_viewport::render_queue_preview(
                    frame,
                    vp.queue_preview,
                    &g.queue_preview_items,
                    g.queue_preview_items.len(),
                );

                if let Some(sidebar) = sidebar_opt {
                    let sections = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Length(11),
                            Constraint::Length(7),
                            Constraint::Min(9),
                        ])
                        .split(sidebar);

                    let ws_line = if g.workspace_display.is_empty() {
                        "—".to_string()
                    } else {
                        sidebar_fit(&g.workspace_display, 26)
                    };
                    let session_lines = vec![
                        Line::from(Span::styled(
                            "workspace",
                            Style::default().fg(theme::muted()),
                        )),
                        Line::from(ws_line),
                        Line::default(),
                        Line::from(format!("session {}", &g.session_id[..8.min(g.session_id.len())])),
                        Line::from(format!("model   {}", g.model)),
                        Line::from(format!("agent   {}", g.agent_profile)),
                        Line::from(format!("mode    {}", g.permission_mode)),
                        Line::from(format!(
                            "status  {}",
                            if g.busy { "busy" } else { "idle" }
                        )),
                        Line::from(format!("blocks  {}", g.blocks.len())),
                        Line::from(format!("lines   {total}")),
                    ];
                    let session_block = Paragraph::new(Text::from(session_lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::border()))
                                .title(Span::styled(
                                    " context ",
                                    Style::default().fg(theme::muted()),
                                )),
                        )
                        .style(Style::default().bg(theme::surface()))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(session_block, sections[0]);

                    let usage_lines = vec![
                        Line::from(format!("input   {}", g.input_tokens)),
                        Line::from(format!("output  {}", g.output_tokens)),
                        Line::from(format!("total   {}", g.input_tokens + g.output_tokens)),
                        Line::from(format!("cost    ${:.4}", g.cost_usd)),
                        Line::default(),
                        Line::from(if g.active_approval.is_some() {
                            "pending approval"
                        } else if g.active_question.is_some() {
                            "pending question"
                        } else {
                            "no pending prompt"
                        }),
                    ];
                    let usage_block = Paragraph::new(Text::from(usage_lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::border()))
                                .title(Span::styled(
                                    " usage ",
                                    Style::default().fg(theme::muted()),
                                )),
                        )
                        .style(Style::default().bg(theme::surface()))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(usage_block, sections[1]);

                    let mut todo_lines: Vec<Line> = vec![Line::from(Span::styled(
                        "sub-agents",
                        Style::default()
                            .fg(theme::muted())
                            .add_modifier(Modifier::BOLD),
                    ))];
                    if g.subagents.is_empty() {
                        todo_lines.push(Line::from(Span::styled(
                            "none (spawn shows here)",
                            Style::default().fg(theme::muted()),
                        )));
                    } else {
                        for row in g.subagents.iter().take(8) {
                            let dot = if row.running { "●" } else { "○" };
                            let id8 = sidebar_fit(&row.id, 8);
                            let ph = sidebar_fit(&row.phase, 11);
                            todo_lines.push(Line::from(vec![
                                Span::styled(
                                    format!("{dot} "),
                                    Style::default().fg(if row.running {
                                        theme::warn()
                                    } else {
                                        theme::muted()
                                    }),
                                ),
                                Span::styled(format!("{id8} "), Style::default().fg(theme::text())),
                                Span::styled(ph, Style::default().fg(theme::tool())),
                            ]));
                            if !row.detail.is_empty() {
                                todo_lines.push(Line::from(Span::styled(
                                    format!("  {}", sidebar_fit(&row.detail, 26)),
                                    Style::default().fg(theme::muted()),
                                )));
                            }
                            if let Some(ref skill_name) = row.skill {
                                todo_lines.push(Line::from(Span::styled(
                                    format!("  [{}]", sidebar_fit(skill_name, 24)),
                                    Style::default().fg(theme::warn()),
                                )));
                            }
                            if !row.task.is_empty() && row.task != "(sub-agent)" {
                                todo_lines.push(Line::from(Span::styled(
                                    format!("  {}", sidebar_fit(&row.task, 26)),
                                    Style::default().fg(theme::text()),
                                )));
                            }
                        }
                    }
                    todo_lines.push(Line::default());
                    todo_lines.push(Line::from(Span::styled(
                        "dev",
                        Style::default()
                            .fg(theme::muted())
                            .add_modifier(Modifier::BOLD),
                    )));
                    todo_lines.push(Line::from(Span::styled(
                        ".dcode-ai/sessions",
                        Style::default().fg(theme::user()),
                    )));
                    todo_lines.push(Line::from(Span::styled(
                        "Ctrl+P commands",
                        Style::default().fg(theme::muted()),
                    )));
                    let todo_block = Paragraph::new(Text::from(todo_lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::border()))
                                .title(Span::styled(
                                    " sidebar ",
                                    Style::default().fg(theme::muted()),
                                )),
                        )
                        .style(Style::default().bg(theme::surface()))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(todo_block, sections[2]);
                }

                let elapsed = g.started.elapsed().as_secs();
                let indicator_text = crate::tui::busy_indicator::render_indicator(
                    g.current_busy_state,
                    g.busy_state_since,
                );
                let status_top_row = st_r;
                let status_bar = crate::tui::widgets::status_bar::StatusBar {
                    model: &g.model,
                    agent: &g.agent_profile,
                    busy_label: &indicator_text,
                    elapsed_secs: elapsed,
                    mcp_servers: g.mcp_server_count,
                    sandbox_status: None,
                    context_tokens: g.context_tokens,
                    tokens_in: g.input_tokens,
                    tokens_out: g.output_tokens,
                    cost_usd: g.cost_usd,
                    permission_bypass: toolbar_permission_is_bypass(&g.permission_mode),
                };

                crate::tui::tui_viewport::render_status_bar(frame, status_top_row, status_bar);
                g.branch_chip_bounds = None;
                g.sidebar_toggle_bounds = None;

                if let Some(sr) = slash_opt {
                    if slash_panel_visible(&g.input_buffer) && !slash_filtered.is_empty() {
                        let n_show = slash_filtered.len().min(SLASH_PANEL_MAX_ROWS);
                        let max_scroll = slash_filtered.len().saturating_sub(n_show);
                        let list_scroll = g
                            .slash_menu_index
                            .saturating_sub(n_show.saturating_sub(1))
                            .min(max_scroll);
                        let mut slash_lines: Vec<Line> = Vec::new();
                        for (i, entry) in slash_filtered[list_scroll..list_scroll + n_show]
                            .iter()
                            .enumerate()
                        {
                            let global = list_scroll + i;
                            let st = if global == g.slash_menu_index {
                                Style::default()
                                    .fg(Color::Black)
                                    .bg(theme::user())
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                Style::default().fg(theme::text())
                            };
                            slash_lines.push(Line::from(Span::styled(entry.display_text(), st)));
                        }
                        if slash_filtered.len() > n_show {
                            slash_lines.push(Line::from(Span::styled(
                                format!(
                                    " ─ {}/{} · ↑↓",
                                    g.slash_menu_index + 1,
                                    slash_filtered.len()
                                ),
                                Style::default().fg(theme::muted()),
                            )));
                        }
                        let slash_w = Paragraph::new(Text::from(slash_lines))
                            .block(
                                Block::default()
                                    .borders(Borders::ALL)
                                    .border_style(Style::default().fg(theme::border()))
                                    .title(Span::styled(
                                        " commands (↑↓ Tab complete) ",
                                        Style::default().fg(theme::muted()),
                                    )),
                            )
                            .style(Style::default().bg(theme::surface()));
                        frame.render_widget(slash_w, sr);
                    } else if !at_matches.is_empty() {
                        let n_show = at_matches.len().min(SLASH_PANEL_MAX_ROWS);
                        let max_scroll = at_matches.len().saturating_sub(n_show);
                        let pick = g.at_menu_index.min(at_matches.len().saturating_sub(1));
                        let list_scroll =
                            pick.saturating_sub(n_show.saturating_sub(1)).min(max_scroll);
                        let mut lines: Vec<Line> = Vec::new();
                        for (i, path) in at_matches[list_scroll..list_scroll + n_show]
                            .iter()
                            .enumerate()
                        {
                            let global = list_scroll + i;
                            let st = if global == pick {
                                Style::default()
                                    .fg(Color::Black)
                                    .bg(theme::user())
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                Style::default().fg(theme::text())
                            };
                            // Show path without @ prefix since @ is already in the buffer
                            lines.push(Line::from(Span::styled(format!(" {path}"), st)));
                        }
                        if at_matches.len() > n_show {
                            lines.push(Line::from(Span::styled(
                                format!(" ─ {}/{} · ↑↓ Tab", pick + 1, at_matches.len()),
                                Style::default().fg(theme::muted()),
                            )));
                        }
                        let at_w = Paragraph::new(Text::from(lines))
                            .block(
                                Block::default()
                                    .borders(Borders::ALL)
                                    .border_style(Style::default().fg(theme::border()))
                                    .title(Span::styled(
                                        " files (@ mention) ",
                                        Style::default().fg(theme::muted()),
                                    )),
                            )
                            .style(Style::default().bg(theme::surface()));
                        frame.render_widget(at_w, sr);
                    }
                }

                let input_line = composer_line(&g.input_buffer, g.cursor_char_idx);

                let hint = if g.active_approval.is_some() {
                    Some(Line::from(Span::styled(
                        "Approval: y/n · Ctrl+Y approve · Ctrl+N deny · Ctrl+U always allow · /approve · /deny",
                        Style::default().fg(theme::error()),
                    )))
                } else if g.active_question.is_some() && !g.question_modal_open {
                    Some(Line::from(Span::styled(
                        "Enter/0 suggested · 1-n option · click option · /auto-answer",
                        Style::default().fg(theme::warn()),
                    )))
                } else if g.busy || !matches!(g.current_busy_state, BusyState::Idle) {
                    Some(Line::from(Span::styled(
                        "Busy: Enter queue · Alt+Enter follow-up · Esc cancel",
                        Style::default().fg(theme::tool()),
                    )))
                } else {
                    None
                };
                let mut input_lines = vec![input_line];
                if !g.staged_image_attachments.is_empty() {
                    input_lines.push(Line::from(Span::styled(
                        format!(
                            "  {} image(s) staged · Enter to send · /image clear",
                            g.staged_image_attachments.len()
                        ),
                        Style::default().fg(theme::success()),
                    )));
                }
                if let Some(hint) = hint {
                    input_lines.push(hint);
                }
                // Composer title: DCODE pill + model + branch context, so the
                // input box shows what you're driving without /status.
                let mut title_spans = vec![permission_mode_pill(&g.permission_mode)];
                if !g.model.is_empty() {
                    title_spans.push(Span::styled(" ", Style::default()));
                    title_spans.push(Span::styled(
                        format!("/{}", truncate_chars(&g.model, 20)),
                        Style::default().fg(theme::muted()),
                    ));
                }
                if !g.current_branch.is_empty() {
                    title_spans.push(Span::styled(" · ", Style::default().fg(theme::border())));
                    title_spans.push(Span::styled(
                        format!("⎇ {}", truncate_chars(&g.current_branch, 18)),
                        Style::default().fg(theme::muted()),
                    ));
                }
                title_spans.push(Span::styled(" ", Style::default()));
                let input_block = Paragraph::new(Text::from(input_lines))
                    .block(
                        Block::default()
                            .borders(Borders::TOP)
                            .border_style(Style::default().fg(theme::border()))
                            .title(Line::from(title_spans))
                            .padding(Padding::new(2, 2, 0, 1)),
                    )
                    .style(Style::default().bg(theme::surface()))
                    .wrap(Wrap { trim: false });

                frame.render_widget(input_block, inp_r);

                if g.command_palette_open {
                    let filtered = filter_palette_rows(&g.command_palette_query);
                    let selectable = palette_selectable_indices(&filtered);
                    let pick_abs = if selectable.is_empty() {
                        0
                    } else {
                        selectable[g.palette_index.min(selectable.len().saturating_sub(1))]
                    };
                    let anim_ms = g.started.elapsed().as_millis();
                    let cursor = selection_pulse(anim_ms);
                    let total_cmds = selectable.len();
                    let total_vis = filtered.len().clamp(1, COMMAND_PALETTE_MAX_ROWS);
                    let mut popup_area =
                        centered_rect(area, COMMAND_PALETTE_WIDTH, (total_vis as u16).saturating_add(9));
                    // Slight downward nudge for visual center against the composer/footer.
                    popup_area.y = popup_area.y.saturating_add(1);
                    let list_scroll = pick_abs.saturating_sub(COMMAND_PALETTE_MAX_ROWS / 2);
                    let list_end = (list_scroll + COMMAND_PALETTE_MAX_ROWS).min(filtered.len());
                    let mut popup_lines = vec![
                        Line::from(vec![
                            Span::styled(
                                "  Filter ",
                                Style::default()
                                    .fg(theme::muted())
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(
                                if g.command_palette_query.is_empty() {
                                    "type to filter"
                                } else {
                                    g.command_palette_query.as_str()
                                },
                                Style::default().fg(theme::text()),
                            ),
                            Span::styled("  ·  ", Style::default().fg(theme::muted())),
                            Span::styled(
                                format!("{total_cmds} commands"),
                                Style::default().fg(theme::tool()),
                            ),
                        ]),
                        Line::default(),
                        Line::default(),
                    ];
                    if selectable.is_empty() {
                        popup_lines.push(Line::from(Span::styled(
                            " No matching commands",
                            Style::default().fg(theme::muted()),
                        )));
                    } else {
                        if list_scroll > 0 {
                            popup_lines.push(Line::from(Span::styled(
                                format!("  ▲ {} above", list_scroll),
                                Style::default().fg(theme::muted()),
                            )));
                        }
                        for (abs, idx) in filtered
                            .iter()
                            .enumerate()
                            .take(list_end)
                            .skip(list_scroll)
                        {
                            match *idx {
                                PaletteRow::Section(name) => {
                                    popup_lines.push(Line::from(Span::styled(
                                        format!("  ─ {} ─", name.to_ascii_uppercase()),
                                        Style::default()
                                            .fg(theme::muted())
                                            .add_modifier(Modifier::BOLD),
                                    )));
                                }
                                PaletteRow::Entry { label, shortcut } => {
                                    let is_selected = abs == pick_abs;
                                    let label_style = if is_selected {
                                        Style::default()
                                            .fg(Color::Black)
                                            .bg(theme::user())
                                            .add_modifier(Modifier::BOLD)
                                    } else {
                                        Style::default().fg(theme::text())
                                    };
                                    let cmd = palette_command_for_label(label);
                                    let cmd_style = if is_selected {
                                        Style::default()
                                            .fg(Color::Black)
                                            .bg(theme::user())
                                    } else {
                                        Style::default().fg(theme::muted())
                                    };
                                    let prefix = if is_selected { cursor } else { "  " };
                                    let mut spans = vec![
                                        Span::styled(format!(" {prefix}{label}"), label_style),
                                        Span::styled(format!("  {cmd}"), cmd_style),
                                    ];
                                    if !shortcut.is_empty() {
                                        spans.push(Span::styled("  ", Style::default().fg(theme::muted())));
                                        spans.push(Span::styled(
                                            format!(" {shortcut} "),
                                            Style::default()
                                                .fg(Color::Black)
                                                .bg(theme::assistant())
                                                .add_modifier(Modifier::BOLD),
                                        ));
                                    }
                                    popup_lines.push(Line::from(spans));
                                }
                            }
                        }
                        let remaining_below = filtered.len().saturating_sub(list_end);
                        if remaining_below > 0 {
                            popup_lines.push(Line::from(Span::styled(
                                format!("  ▼ {} more", remaining_below),
                                Style::default().fg(theme::muted()),
                            )));
                        }
                    }
                    popup_lines.push(Line::default());
                    popup_lines.push(Line::default());
                    popup_lines.push(Line::from(Span::styled(
                        " ↑↓ move · Enter apply · Esc close ",
                        Style::default().fg(theme::muted()),
                    )));
                    frame.render_widget(ClearWidget, popup_area);
                    let popup = Paragraph::new(Text::from(popup_lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::border()))
                                .padding(Padding::new(1, 1, 0, 0))
                                .title(Span::styled(
                                    " command palette (ctrl+p) ",
                                    Style::default().fg(theme::muted()),
                                )),
                        )
                        .style(Style::default().bg(theme::surface()))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }

                if g.transcript_search_open {
                    let inner_w = tr.width.saturating_sub(2);
                    let (lines, _) = transcript_cache.get_or_rebuild(&g, inner_w);
                    let matches = transcript_search_matches(lines, &g.transcript_search_query);
                    let status = if g.transcript_search_query.trim().is_empty() {
                        "type to search transcript".to_string()
                    } else if matches.is_empty() {
                        "no matches".to_string()
                    } else {
                        format!(
                            "{} / {} matches",
                            g.transcript_search_index + 1,
                            matches.len()
                        )
                    };
                    let popup_area = centered_rect(area, 56, 9);
                    let lines = vec![
                        Line::from(vec![
                            Span::styled(
                                " Query ",
                                Style::default()
                                    .fg(theme::muted())
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(
                                if g.transcript_search_query.is_empty() {
                                    "type to filter transcript"
                                } else {
                                    g.transcript_search_query.as_str()
                                },
                                Style::default().fg(theme::text()),
                            ),
                        ]),
                        Line::default(),
                        Line::from(Span::styled(
                            format!(" {status}"),
                            Style::default().fg(theme::assistant()),
                        )),
                        Line::default(),
                        Line::from(Span::styled(
                            " Enter/Down next · Up previous · Backspace edit · Esc close ",
                            Style::default().fg(theme::muted()),
                        )),
                    ];
                    frame.render_widget(ClearWidget, popup_area);
                    let popup = Paragraph::new(Text::from(lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::border()))
                                .title(Span::styled(
                                    " transcript search (ctrl+f) ",
                                    Style::default().fg(theme::muted()),
                                )),
                        )
                        .style(Style::default().bg(theme::surface()))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }

                if g.composer_history_search_open {
                    let needle = g.composer_history_search_query.to_ascii_lowercase();
                    let mut matches: Vec<String> = composer_history
                        .iter()
                        .rev()
                        .filter(|entry| {
                            needle.is_empty() || entry.to_ascii_lowercase().contains(&needle)
                        })
                        .take(8)
                        .cloned()
                        .collect();
                    if matches.is_empty() {
                        matches.push("no matches".to_string());
                    }
                    let pick = g
                        .composer_history_search_index
                        .min(matches.len().saturating_sub(1));
                    let popup_area = centered_rect(area, 72, 12);
                    let mut lines = vec![Line::from(vec![
                        Span::styled(
                            " Query ",
                            Style::default()
                                .fg(theme::muted())
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            if g.composer_history_search_query.is_empty() {
                                "type to search composer history"
                            } else {
                                g.composer_history_search_query.as_str()
                            },
                            Style::default().fg(theme::text()),
                        ),
                    ])];
                    lines.push(Line::default());
                    for (idx, entry) in matches.iter().enumerate() {
                        let st = if idx == pick {
                            Style::default().fg(Color::Black).bg(theme::user())
                        } else {
                            Style::default().fg(theme::text())
                        };
                        lines.push(Line::from(Span::styled(
                            format!(" {} {}", if idx == pick { "▸" } else { " " }, truncate_chars(entry, 62)),
                            st,
                        )));
                    }
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        " Enter use · ↑↓ select · Backspace edit · Esc close ",
                        Style::default().fg(theme::muted()),
                    )));
                    frame.render_widget(ClearWidget, popup_area);
                    let popup = Paragraph::new(Text::from(lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::border()))
                                .title(Span::styled(
                                    " composer history search (ctrl+r) ",
                                    Style::default().fg(theme::muted()),
                                )),
                        )
                        .style(Style::default().bg(theme::surface()))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }

                // Branch picker popup.
                if g.branch_picker_open {
                    let branches = &g.branch_picker_branches;
                    let filtered = filtered_branch_indices(branches, &g.branch_picker_query);

                    let popup_h = (filtered.len().min(12) as u16).saturating_add(6).max(8);
                    let popup_area = centered_rect(area, 36, popup_h);

                    let mut popup_lines = vec![
                        Line::from(vec![
                            Span::styled(" Branch ", Style::default().fg(theme::muted()).add_modifier(Modifier::BOLD)),
                            Span::styled(
                                if g.branch_picker_query.is_empty() {
                                    "".to_string()
                                } else {
                                    format!(": {}", g.branch_picker_query)
                                },
                                Style::default().fg(theme::text()),
                            ),
                        ]),
                        Line::default(),
                    ];

                    if filtered.is_empty() {
                        popup_lines.push(Line::from(Span::styled(
                            "  (no branches — type a name to create)",
                            Style::default().fg(theme::muted()),
                        )));
                    } else {
                        let n_show = filtered.len().min(12);
                        let list_scroll = g
                            .branch_picker_index
                            .saturating_sub(n_show.saturating_sub(1))
                            .min(filtered.len().saturating_sub(n_show));
                        for (i, branch_idx) in filtered[list_scroll..list_scroll + n_show].iter().enumerate() {
                            let filtered_idx = list_scroll + i;
                            let branch = &branches[*branch_idx];
                            let style = if filtered_idx == g.branch_picker_index {
                                Style::default()
                                    .fg(Color::Black)
                                    .bg(theme::user())
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                Style::default().fg(theme::text())
                            };
                            let mark = if branch.as_str() == g.current_branch { " *" } else { "" };
                            popup_lines.push(Line::from(Span::styled(format!(" {branch}{mark}"), style)));
                        }
                    }

                    popup_lines.push(Line::default());
                    popup_lines.push(Line::from(Span::styled(
                        " Enter switch  /name new  Esc close",
                        Style::default().fg(theme::muted()),
                    )));

                    frame.render_widget(ClearWidget, popup_area);
                    let popup = Paragraph::new(Text::from(popup_lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::border()))
                                .title(Span::styled(" git branch ", Style::default().fg(theme::muted()))),
                        )
                        .style(Style::default().bg(theme::surface()))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }

                // LLM provider picker (default provider or API-key target).
                if g.provider_picker_open {
                    let names: Vec<&'static str> = ProviderKind::ALL
                        .iter()
                        .map(|p| p.display_name())
                        .collect();
                    let rows = (names.len() as u16).saturating_add(6).max(8);
                    let popup_area = centered_rect(area, 40, rows);
                    let mut lines: Vec<Line> = vec![
                        Line::from(Span::styled(
                            if g.provider_picker_for_api_key {
                                " Select provider for API key "
                            } else {
                                " Default LLM provider "
                            },
                            Style::default().fg(theme::muted()).add_modifier(Modifier::BOLD),
                        )),
                        Line::default(),
                    ];
                    for (i, name) in names.iter().enumerate() {
                        let st = if i == g.provider_picker_index {
                            Style::default()
                                .fg(Color::Black)
                                .bg(theme::user())
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(theme::text())
                        };
                        lines.push(Line::from(Span::styled(format!(" {name}"), st)));
                    }
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        " Enter confirm · Esc cancel ",
                        Style::default().fg(theme::muted()),
                    )));
                    frame.render_widget(ClearWidget, popup_area);
                    let popup = Paragraph::new(Text::from(lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::border()))
                                .title(Span::styled(" settings ", Style::default().fg(theme::muted()))),
                        )
                        .style(Style::default().bg(theme::surface()))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }

                if g.permission_picker_open {
                    const PERM_ROWS: &[(&str, &str)] = &[
                        ("Default", "ask before risky actions"),
                        ("Plan", "planning-first interaction"),
                        ("AcceptEdits", "approve file edits, prompt for dangerous"),
                        ("DontAsk", "auto-approve most tool actions"),
                        (
                            "BypassPermissions",
                            "read/edit auto; first bash asks once per session",
                        ),
                    ];
                    let rows = (PERM_ROWS.len() as u16).saturating_add(8).max(10);
                    let popup_area = centered_rect(area, 78, rows);
                    let mut lines: Vec<Line> = vec![
                        Line::from(Span::styled(
                            " Permission mode ",
                            Style::default().fg(theme::muted()).add_modifier(Modifier::BOLD),
                        )),
                        Line::from(Span::styled(
                            format!(" current: {}", g.permission_mode),
                            Style::default().fg(theme::assistant()),
                        )),
                        Line::default(),
                    ];
                    for (i, (name, desc)) in PERM_ROWS.iter().enumerate() {
                        let selected = i == g.permission_picker_index;
                        let st = if selected {
                            Style::default().fg(Color::Black).bg(theme::user()).add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(theme::text())
                        };
                        let desc_st = if selected {
                            Style::default().fg(Color::Black).bg(theme::user())
                        } else {
                            Style::default().fg(theme::muted())
                        };
                        lines.push(Line::from(vec![
                            Span::styled(format!(" [{}] {:<16}", i, name), st),
                            Span::styled(format!(" {desc}"), desc_st),
                        ]));
                    }
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        " ↑↓ select · Enter apply · Esc close ",
                        Style::default().fg(theme::muted()),
                    )));
                    frame.render_widget(ClearWidget, popup_area);
                    let popup = Paragraph::new(Text::from(lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::border()))
                                .title(Span::styled(" permissions ", Style::default().fg(theme::muted()))),
                        )
                        .style(Style::default().bg(theme::surface()))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }

                if g.theme_picker_open {
                    let entries = g.theme_picker_entries.clone();
                    let rows = (entries.len() as u16).saturating_add(6).clamp(8, 20);
                    let popup_area = centered_rect(area, 44, rows);
                    let mut lines: Vec<Line> = vec![
                        Line::from(Span::styled(
                            " Theme ",
                            Style::default()
                                .fg(theme::muted())
                                .add_modifier(Modifier::BOLD),
                        )),
                        Line::default(),
                    ];
                    for (i, name) in entries.iter().enumerate() {
                        let st = if i == g.theme_picker_index {
                            Style::default()
                                .fg(Color::Black)
                                .bg(theme::user())
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(theme::text())
                        };
                        let marker = if i == g.theme_picker_index { "▸ " } else { "  " };
                        lines.push(Line::from(Span::styled(format!(" {marker}{name}"), st)));
                    }
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        " Enter apply · Esc cancel ",
                        Style::default().fg(theme::muted()),
                    )));
                    frame.render_widget(ClearWidget, popup_area);
                    let popup = Paragraph::new(Text::from(lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::border()))
                                .title(Span::styled(
                                    " theme ",
                                    Style::default().fg(theme::muted()),
                                )),
                        )
                        .style(Style::default().bg(theme::surface()))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }

                if g.agent_picker_open {
                    const AGENT_LABELS: &[(&str, &str)] = &[
                        ("@build", "Full-access agent for development"),
                        ("@plan", "Read-only analysis and planning"),
                        ("@review", "Focused code review"),
                        ("@fix", "Bug diagnosis and minimal fixes"),
                        ("@test", "Testing and validation"),
                    ];
                    let rows = (AGENT_LABELS.len() as u16).saturating_add(6).max(8);
                    let popup_area = centered_rect(area, 52, rows);
                    let mut lines: Vec<Line> = vec![
                        Line::from(Span::styled(
                            " Agent profile ",
                            Style::default().fg(theme::muted()).add_modifier(Modifier::BOLD),
                        )),
                        Line::default(),
                    ];
                    for (i, (name, desc)) in AGENT_LABELS.iter().enumerate() {
                        let st = if i == g.agent_picker_index {
                            Style::default().fg(Color::Black).bg(theme::user()).add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(theme::text())
                        };
                        let desc_st = if i == g.agent_picker_index {
                            Style::default().fg(Color::Black).bg(theme::user())
                        } else {
                            Style::default().fg(theme::muted())
                        };
                        lines.push(Line::from(vec![
                            Span::styled(format!(" {name:<10}"), st),
                            Span::styled(format!(" {desc}"), desc_st),
                        ]));
                    }
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        " Enter apply · Esc cancel ",
                        Style::default().fg(theme::muted()),
                    )));
                    frame.render_widget(ClearWidget, popup_area);
                    let popup = Paragraph::new(Text::from(lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::border()))
                                .title(Span::styled(" agent ", Style::default().fg(theme::muted()))),
                        )
                        .style(Style::default().bg(theme::surface()))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }

                // Question modal popup (arrow-key option picker).
                if g.question_modal_open
                    && let Some(ref q) = g.active_question
                {
                        let has_chat_option = q.allow_custom;
                        let total_items = 1 + q.options.len() + if has_chat_option { 1 } else { 0 };
                        let rows = (total_items as u16).saturating_add(12).max(12);
                        let popup_w = 82u16.min(area.width.saturating_sub(4));
                        let popup_area = centered_rect(area, popup_w, rows);

                        let mut lines: Vec<Line> = vec![
                            Line::from(Span::styled(
                                " Pick one option ",
                                Style::default()
                                    .fg(theme::warn())
                                    .add_modifier(Modifier::BOLD),
                            )),
                            Line::default(),
                        ];
                        for text_line in wrap_text(&q.prompt, popup_w.saturating_sub(8) as usize) {
                            lines.push(Line::from(Span::styled(
                                format!(" {text_line}"),
                                Style::default()
                                    .fg(theme::assistant())
                                    .add_modifier(Modifier::BOLD),
                            )));
                        }
                        lines.push(Line::default());

                        // Suggested answer (index 0)
                        let suggested_label = format!("suggested: {}", q.suggested_answer);
                        if g.question_modal_index == 0 {
                            lines.push(Line::from(Span::styled(
                                format!(" [0] {suggested_label}"),
                                Style::default()
                                    .fg(Color::Black)
                                    .bg(theme::user())
                                    .add_modifier(Modifier::BOLD),
                            )));
                        } else {
                            lines.push(Line::from(Span::styled(
                                format!(" [0] {suggested_label}"),
                                Style::default().fg(theme::text()),
                            )));
                        }

                        // Options (index 1..n)
                        for (i, o) in q.options.iter().enumerate() {
                            let item_idx = i + 1;
                            let label = format!("({}) {}", o.id, o.label);
                            if g.question_modal_index == item_idx {
                                lines.push(Line::from(Span::styled(
                                    format!(" [{item_idx}] {label}"),
                                    Style::default()
                                        .fg(Color::Black)
                                        .bg(theme::user())
                                        .add_modifier(Modifier::BOLD),
                                )));
                            } else {
                                lines.push(Line::from(Span::styled(
                                    format!(" [{item_idx}] {label}"),
                                    Style::default().fg(theme::text()),
                                )));
                            }
                        }

                        // "Chat about this" (last item, only if allow_custom)
                        if has_chat_option {
                            let chat_idx = 1 + q.options.len();
                            if g.question_modal_index == chat_idx {
                                lines.push(Line::from(Span::styled(
                                    " [c] Chat about this in composer",
                                    Style::default()
                                        .fg(Color::Black)
                                        .bg(theme::user())
                                        .add_modifier(Modifier::BOLD),
                                )));
                            } else {
                                lines.push(Line::from(Span::styled(
                                    " [c] Chat about this in composer",
                                    Style::default()
                                        .fg(theme::muted())
                                        .add_modifier(Modifier::ITALIC),
                                )));
                            }
                        }

                        // Footer
                        lines.push(Line::default());
                        let footer_text = if has_chat_option {
                            " ↑↓ select · Enter confirm · Esc switch to composer input "
                        } else {
                            " ↑↓ select · Enter confirm "
                        };
                        lines.push(Line::from(Span::styled(
                            footer_text,
                            Style::default().fg(theme::muted()),
                        )));

                        frame.render_widget(ClearWidget, popup_area);
                        let popup = Paragraph::new(Text::from(lines))
                            .block(
                                Block::default()
                                    .borders(Borders::ALL)
                                    .border_style(Style::default().fg(theme::border()))
                                    .title(Span::styled(
                                        " question ",
                                        Style::default().fg(theme::warn()),
                                    )),
                            )
                            .style(Style::default().bg(theme::surface()))
                            .wrap(Wrap { trim: false });
                        frame.render_widget(popup, popup_area);
                }

                if g.pins_modal_open {
                    let total = g.pinned_notes.len();
                    let rows = (total.min(10) as u16).saturating_add(8).max(9);
                    let popup_area = centered_rect(area, 74, rows);
                    let pick = g.pins_modal_index.min(total.saturating_sub(1));
                    let mut lines: Vec<Line> = vec![
                        Line::from(Span::styled(
                            " pinned notes ",
                            Style::default()
                                .fg(theme::warn())
                                .add_modifier(Modifier::BOLD),
                        )),
                        Line::default(),
                    ];
                    if g.pinned_notes.is_empty() {
                        lines.push(Line::from(Span::styled(
                            " No pinned notes yet. Use Ctrl+K to pin the latest response.",
                            Style::default().fg(theme::muted()),
                        )));
                    } else {
                        for (idx, note) in g.pinned_notes.iter().enumerate().take(10) {
                            let st = if idx == pick {
                                Style::default()
                                    .fg(Color::Black)
                                    .bg(theme::user())
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                Style::default().fg(theme::text())
                            };
                            let preview = truncate_chars(&note.body, 38);
                            lines.push(Line::from(vec![
                                Span::styled(format!(" {:>2}. ", idx + 1), st),
                                Span::styled(format!("{:<18}", truncate_chars(&note.title, 18)), st),
                                Span::styled(" · ", st),
                                Span::styled(preview, st),
                            ]));
                        }
                        if total > 10 {
                            lines.push(Line::from(Span::styled(
                                format!("  … +{} more", total - 10),
                                Style::default().fg(theme::muted()),
                            )));
                        }
                    }
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        " Enter jump top · Backspace remove · F6 copy selected · Esc close ",
                        Style::default().fg(theme::muted()),
                    )));
                    frame.render_widget(ClearWidget, popup_area);
                    let popup = Paragraph::new(Text::from(lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::border()))
                                .title(Span::styled(" pins ", Style::default().fg(theme::muted()))),
                        )
                        .style(Style::default().bg(theme::surface()))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }

                if g.subagent_modal_open {
                    let total = g.subagents.len();
                    let rows = (total.min(10) as u16).saturating_add(9).max(10);
                    let popup_area = centered_rect(area, 78, rows);
                    let pick = g.subagent_modal_index.min(total.saturating_sub(1));
                    let mut lines: Vec<Line> = vec![
                        Line::from(Span::styled(
                            " sub-agent dashboard ",
                            Style::default()
                                .fg(theme::assistant())
                                .add_modifier(Modifier::BOLD),
                        )),
                        Line::default(),
                    ];
                    if g.subagents.is_empty() {
                        lines.push(Line::from(Span::styled(
                            " No active sub-agents.",
                            Style::default().fg(theme::muted()),
                        )));
                    } else {
                        for (idx, row) in g.subagents.iter().enumerate().take(10) {
                            let st = if idx == pick {
                                Style::default()
                                    .fg(Color::Black)
                                    .bg(theme::user())
                                    .add_modifier(Modifier::BOLD)
                            } else if row.running {
                                Style::default().fg(theme::text())
                            } else {
                                Style::default().fg(theme::muted())
                            };
                            let prog = subagent_phase_progress(&row.phase, row.running);
                            let pbar = progress_bar(prog, 16);
                            lines.push(Line::from(vec![
                                Span::styled(format!(" {:>2}. ", idx + 1), st),
                                Span::styled(format!("{:<8}", sidebar_fit(&row.id, 8)), st),
                                Span::styled(format!(" {:<11}", sidebar_fit(&row.phase, 11)), st),
                                Span::styled(format!(" {}", pbar), st),
                            ]));
                            if !row.detail.is_empty() {
                                lines.push(Line::from(Span::styled(
                                    format!("      {}", truncate_chars(&row.detail, 62)),
                                    if idx == pick {
                                        st
                                    } else {
                                        Style::default().fg(theme::muted())
                                    },
                                )));
                            }
                        }
                        if total > 10 {
                            lines.push(Line::from(Span::styled(
                                format!("  … +{} more", total - 10),
                                Style::default().fg(theme::muted()),
                            )));
                        }
                    }
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        " Enter focus session · ↑↓ select · Esc close ",
                        Style::default().fg(theme::muted()),
                    )));
                    frame.render_widget(ClearWidget, popup_area);
                    let popup = Paragraph::new(Text::from(lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::border()))
                                .title(Span::styled(
                                    " sub-agents (ctrl+g) ",
                                    Style::default().fg(theme::muted()),
                                )),
                        )
                        .style(Style::default().bg(theme::surface()))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }

                if g.session_picker_open {
                    let filter = g.session_picker_search.to_ascii_lowercase();
                    let filtered_indices: Vec<usize> = g.session_picker_entries.iter().enumerate()
                        .filter(|(_, entry)| {
                            filter.is_empty()
                                || entry.search_text.to_ascii_lowercase().contains(&filter)
                        })
                        .map(|(i, _)| i)
                        .collect();
                    const SESSION_PICKER_MAX_ROWS: usize = 16;
                    let n_filtered = filtered_indices.len();
                    let viewport_rows = n_filtered.min(SESSION_PICKER_MAX_ROWS);
                    let rows = (viewport_rows as u16).saturating_add(8).max(10);
                    let popup_area = centered_rect(area, 56, rows);
                    let pick = g.session_picker_index.min(n_filtered.saturating_sub(1));

                    if pick < g.session_picker_scroll {
                        g.session_picker_scroll = pick;
                    } else if viewport_rows > 0 && pick >= g.session_picker_scroll + viewport_rows {
                        g.session_picker_scroll = pick.saturating_sub(viewport_rows - 1);
                    }
                    g.session_picker_scroll = g.session_picker_scroll.min(n_filtered.saturating_sub(viewport_rows));
                    let list_start = g.session_picker_scroll;
                    let list_end = (list_start + viewport_rows).min(n_filtered);

                    let search_display = if g.session_picker_search.is_empty() {
                        "type to filter".to_string()
                    } else {
                        g.session_picker_search.clone()
                    };
                    let mut lines: Vec<Line> = vec![
                        Line::from(vec![
                            Span::styled(" Search ", Style::default().fg(theme::muted()).add_modifier(Modifier::BOLD)),
                            Span::styled(search_display, Style::default().fg(theme::text())),
                        ]),
                        Line::default(),
                    ];
                    if filtered_indices.is_empty() {
                        lines.push(Line::from(Span::styled(" No matching sessions", Style::default().fg(theme::muted()))));
                    } else {
                        if list_start > 0 {
                            lines.push(Line::from(Span::styled(
                                format!("  ▲ {} more", list_start),
                                Style::default().fg(theme::muted()),
                            )));
                        }
                        let current_session_id = g.session_id.clone();
                        for (vis_idx, &filt_idx) in filtered_indices
                            .iter()
                            .enumerate()
                            .skip(list_start)
                            .take(list_end.saturating_sub(list_start))
                        {
                            let entry = &g.session_picker_entries[filt_idx];
                            let is_current = entry.id == current_session_id;
                            let marker = if is_current { " *" } else { "" };
                            let st = if vis_idx == pick {
                                Style::default().fg(Color::Black).bg(theme::user()).add_modifier(Modifier::BOLD)
                            } else {
                                Style::default().fg(theme::text())
                            };
                            lines.push(Line::from(Span::styled(
                                format!(" {}{marker}", entry.label),
                                st,
                            )));
                        }
                        let remaining_below = n_filtered.saturating_sub(list_end);
                        if remaining_below > 0 {
                            lines.push(Line::from(Span::styled(
                                format!("  ▼ {} more", remaining_below),
                                Style::default().fg(theme::muted()),
                            )));
                        }
                    }
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        " Enter resume · Esc close ",
                        Style::default().fg(theme::muted()),
                    )));
                    frame.render_widget(ClearWidget, popup_area);
                    let popup = Paragraph::new(Text::from(lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::border()))
                                .title(Span::styled(" sessions ", Style::default().fg(theme::muted()))),
                        )
                        .style(Style::default().bg(theme::surface()))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }

                if g.api_key_modal_open {
                    let provider = g
                        .api_key_target_provider
                        .map(|p| p.display_name())
                        .unwrap_or("provider");
                    let popup_area = centered_rect(area, 66, 12);
                    let headline = if g.api_key_connect_after_save {
                        " Connect provider "
                    } else {
                        " API key "
                    };
                    let hint = if g.api_key_target_has_existing {
                        " Press Enter to keep current key, or paste a new key to replace it. "
                    } else {
                        " Paste API key, then press Enter. "
                    };
                    let masked = if g.api_key_input.is_empty() {
                        String::new()
                    } else {
                        "*".repeat(g.api_key_input.chars().count())
                    };
                    let validation_line = if g.onboarding_mode {
                        match &g.validation_status {
                            Some(crate::tui::state::OnboardingValidation::Validating) => {
                                Some(Line::from(Span::styled(
                                    " Validating...",
                                    Style::default().fg(Color::Yellow),
                                )))
                            }
                            Some(crate::tui::state::OnboardingValidation::Failed(msg)) => {
                                Some(Line::from(Span::styled(
                                    format!(" {}", msg),
                                    Style::default().fg(Color::Red),
                                )))
                            }
                            _ => None,
                        }
                    } else {
                        None
                    };
                    let mut lines = vec![
                        Line::from(vec![
                            Span::styled(
                                format!(" Provider: {provider}"),
                                Style::default()
                                    .fg(theme::text())
                                    .add_modifier(Modifier::BOLD),
                            ),
                        ]),
                        Line::default(),
                        Line::from(vec![
                            Span::styled(" API key ", Style::default().fg(theme::muted())),
                            Span::styled(masked, Style::default().fg(theme::user())),
                        ]),
                        Line::default(),
                        Line::from(Span::styled(hint, Style::default().fg(theme::muted()))),
                        Line::from(Span::styled(
                            " Enter confirm · Esc cancel ",
                            Style::default().fg(theme::muted()),
                        )),
                    ];
                    if let Some(vline) = validation_line {
                        lines.push(vline);
                    }
                    frame.render_widget(ClearWidget, popup_area);
                    let popup = Paragraph::new(Text::from(lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::border()))
                                .title(Span::styled(headline, Style::default().fg(theme::muted()))),
                        )
                        .style(Style::default().bg(theme::surface()))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }

                // Generic info modal (read-only scrollable popup).
                if g.info_modal_open {
                    let max_vis = area.height.saturating_sub(8).clamp(8, 30) as usize;
                    let n_lines = g.info_modal_lines.len();
                    let popup_h = (n_lines.min(max_vis) as u16).saturating_add(6).max(8);
                    let max_line_chars = g
                        .info_modal_lines
                        .iter()
                        .map(|line| line.chars().count())
                        .max()
                        .unwrap_or(40);
                    let max_popup_w = area.width.saturating_sub(2).max(32);
                    let min_popup_w = 64u16.min(max_popup_w);
                    let popup_w = (max_line_chars.min(320) as u16)
                        .saturating_add(6)
                        .clamp(min_popup_w, max_popup_w);
                    let popup_area = centered_rect(area, popup_w, popup_h);
                    let n_show = n_lines.min(max_vis).max(1);
                    let content_w = popup_w.saturating_sub(4) as usize;
                    g.info_modal_view_rows = n_show;
                    g.info_modal_view_cols = content_w.max(1);
                    let max_scroll = n_lines.saturating_sub(n_show);
                    g.info_modal_scroll = g.info_modal_scroll.min(max_scroll);
                    let max_hscroll = max_line_chars.saturating_sub(content_w.max(1));
                    g.info_modal_hscroll = g.info_modal_hscroll.min(max_hscroll);
                    let start = g.info_modal_scroll;
                    let end = (start + n_show).min(n_lines);
                    let mut lines: Vec<Line> = Vec::new();
                    for line in &g.info_modal_lines[start..end] {
                        lines.push(Line::from(Span::styled(
                            format!(
                                " {}",
                                char_window(line, g.info_modal_hscroll, content_w.max(1))
                            ),
                            Style::default().fg(theme::text()),
                        )));
                    }
                    if n_lines > max_vis {
                        lines.push(Line::from(Span::styled(
                            format!(
                                " ─ {}/{} · ↑↓ vertical · ←→ horizontal ({}/{})",
                                start + 1,
                                n_lines,
                                g.info_modal_hscroll + 1,
                                max_hscroll + 1
                            ),
                            Style::default().fg(theme::muted()),
                        )));
                    } else {
                        lines.push(Line::from(Span::styled(
                            format!(
                                " ─ ←→ horizontal ({}/{})",
                                g.info_modal_hscroll + 1,
                                max_hscroll + 1
                            ),
                            Style::default().fg(theme::muted()),
                        )));
                    }
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        " Esc close ",
                        Style::default().fg(theme::muted()),
                    )));
                    frame.render_widget(ClearWidget, popup_area);
                    let title = format!(" {} ", g.info_modal_title);
                    let popup = Paragraph::new(Text::from(lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::border()))
                                .title(Span::styled(title, Style::default().fg(theme::muted()))),
                        )
                        .style(Style::default().bg(theme::surface()))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }

                // Model picker popup.
                if g.model_picker_open {
                    let filter = g.model_picker_search.to_ascii_lowercase();

                    // Pre-compute indices for visible/selectable items and scroll
                    // without holding an immutable borrow on `g` that conflicts
                    // with the scroll update.
                    let vis_indices: Vec<usize> = g
                        .model_picker_entries
                        .iter()
                        .enumerate()
                        .filter(|(_, e)| {
                            e.is_header
                                || filter.is_empty()
                                || e.label.to_ascii_lowercase().contains(&filter)
                                || e.detail.to_ascii_lowercase().contains(&filter)
                        })
                        .map(|(i, _)| i)
                        .collect();
                    let selectable_vis: Vec<usize> = vis_indices
                        .iter()
                        .enumerate()
                        .filter(|&(_, &orig)| !g.model_picker_entries[orig].is_header)
                        .map(|(vi, _)| vi)
                        .collect();
                    let n_sel = selectable_vis.len();
                    let pick = if n_sel > 0 {
                        g.model_picker_index.min(n_sel - 1)
                    } else {
                        0
                    };
                    let selected_vis_idx = selectable_vis.get(pick).copied().unwrap_or(0);

                    const MODEL_PICKER_MAX_ROWS: usize = 18;
                    let n_visible = vis_indices.len();
                    let viewport_rows = n_visible.min(MODEL_PICKER_MAX_ROWS);
                    let popup_h = (viewport_rows as u16).saturating_add(7).max(10);
                    let popup_area = centered_rect(area, 62, popup_h);

                    // Keep the selected item visible within the viewport.
                    if selected_vis_idx < g.model_picker_scroll {
                        g.model_picker_scroll = selected_vis_idx;
                    } else if viewport_rows > 0 && selected_vis_idx >= g.model_picker_scroll + viewport_rows {
                        g.model_picker_scroll = selected_vis_idx.saturating_sub(viewport_rows - 1);
                    }
                    g.model_picker_scroll = g.model_picker_scroll.min(n_visible.saturating_sub(viewport_rows));
                    let list_start = g.model_picker_scroll;
                    let list_end = (list_start + viewport_rows).min(n_visible);

                    let search_display = if g.model_picker_search.is_empty() {
                        "type to filter…".to_string()
                    } else {
                        g.model_picker_search.clone()
                    };
                    let mut lines: Vec<Line> = vec![
                        Line::from(vec![
                            Span::styled(
                                "Search ",
                                Style::default()
                                    .fg(theme::muted())
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(
                                search_display,
                                Style::default().fg(theme::text()),
                            ),
                        ]),
                        Line::default(),
                    ];
                    if vis_indices.is_empty() {
                        lines.push(Line::from(Span::styled(
                            " No models match",
                            Style::default().fg(theme::muted()),
                        )));
                    } else {
                        if list_start > 0 {
                            lines.push(Line::from(Span::styled(
                                format!("  ▲ {} more", list_start),
                                Style::default().fg(theme::muted()),
                            )));
                        }
                        for (vi, &model_idx) in vis_indices
                            .iter()
                            .enumerate()
                            .skip(list_start)
                            .take(list_end.saturating_sub(list_start))
                        {
                            let entry = &g.model_picker_entries[model_idx];
                            if entry.is_header {
                                lines.push(Line::from(Span::styled(
                                    format!(" {}", entry.label),
                                    Style::default()
                                        .fg(theme::assistant())
                                        .add_modifier(Modifier::BOLD),
                                )));
                            } else {
                                let is_sel = selected_vis_idx == vi;
                                let main_st = if is_sel {
                                    Style::default()
                                        .fg(Color::Black)
                                        .bg(theme::user())
                                        .add_modifier(Modifier::BOLD)
                                } else {
                                    Style::default().fg(theme::text())
                                };
                                let sub_st = if is_sel {
                                    main_st
                                } else {
                                    Style::default().fg(theme::muted())
                                };
                                lines.push(Line::from(vec![
                                    Span::styled(format!("   {}", entry.label), main_st),
                                    Span::styled(format!("  {}", entry.detail), sub_st),
                                ]));
                            }
                        }
                        let remaining_below = n_visible.saturating_sub(list_end);
                        if remaining_below > 0 {
                            lines.push(Line::from(Span::styled(
                                format!("  ▼ {} more", remaining_below),
                                Style::default().fg(theme::muted()),
                            )));
                        }
                    }
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        " ↑↓ select · Enter apply · Esc close ",
                        Style::default().fg(theme::muted()),
                    )));
                    frame.render_widget(ClearWidget, popup_area);
                    let popup = Paragraph::new(Text::from(lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::border()))
                                .title(Span::styled(
                                    " models ",
                                    Style::default().fg(theme::muted()),
                                )),
                        )
                        .style(Style::default().bg(theme::surface()))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }

                // OpenCode-style "Connect a provider" (`/connect`).
                if g.connect_modal_open {
                    let rows = build_connect_rows(&g.connect_search);
                    let total_providers = selectable_row_indices(&rows).len();
                    let sel = clamp_selection(g.connect_menu_index, &rows);
                    let selected_row = row_index_for_selection(&rows, sel);
                    let anim_ms = g.started.elapsed().as_millis();
                    let cursor = selection_pulse(anim_ms);
                    let sparkle = title_sparkle(anim_ms);
                    let body_lines = rows.len().max(1);
                    let popup_h = (body_lines as u16).saturating_add(10).clamp(13, 25);
                    let popup_area = centered_rect(area, 74, popup_h);
                    let viewport_rows = popup_h.saturating_sub(8).max(1) as usize;
                    let max_scroll = rows.len().saturating_sub(viewport_rows);
                    if let Some(sr) = selected_row {
                        if sr < g.connect_modal_scroll {
                            g.connect_modal_scroll = sr;
                        } else if sr >= g.connect_modal_scroll.saturating_add(viewport_rows) {
                            g.connect_modal_scroll = sr.saturating_sub(viewport_rows - 1);
                        }
                    }
                    g.connect_modal_scroll = g.connect_modal_scroll.min(max_scroll);
                    let list_start = g.connect_modal_scroll;
                    let list_end = (list_start + viewport_rows).min(rows.len());
                    let auth_store = AuthStore::load().unwrap_or_default();
                    let mut lines: Vec<Line> = vec![
                        Line::from(vec![
                            Span::styled(
                                "Filter ",
                                Style::default()
                                    .fg(theme::muted())
                                    .add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(
                                if g.connect_search.is_empty() {
                                    "type to filter…"
                                } else {
                                    g.connect_search.as_str()
                                },
                                Style::default().fg(theme::text()),
                            ),
                            Span::styled("  ·  ", Style::default().fg(theme::muted())),
                            Span::styled(
                                format!("{total_providers} providers"),
                                Style::default().fg(theme::tool()),
                            ),
                        ]),
                        Line::default(),
                    ];
                    if rows.is_empty() {
                        lines.push(Line::from(Span::styled(
                            " No providers match",
                            Style::default().fg(theme::muted()),
                        )));
                    } else {
                        if list_start > 0 {
                            lines.push(Line::from(Span::styled(
                                format!(" ▲ {} above", list_start),
                                Style::default().fg(theme::muted()),
                            )));
                        }
                        for (i, row) in rows
                            .iter()
                            .enumerate()
                            .skip(list_start)
                            .take(viewport_rows)
                        {
                            match row {
                                ConnectRow::Section { title } => {
                                    lines.push(Line::from(vec![
                                        Span::styled(
                                            "  ─ ",
                                            Style::default().fg(theme::muted()),
                                        ),
                                        Span::styled(
                                            format!("{} ", title.to_ascii_uppercase()),
                                            Style::default()
                                                .fg(theme::muted())
                                                .add_modifier(Modifier::BOLD),
                                        ),
                                        Span::styled("─", Style::default().fg(theme::muted())),
                                    ]));
                                }
                                ConnectRow::Provider {
                                    kind: _,
                                    title,
                                    subtitle,
                                    action,
                                } => {
                                    let is_sel = selected_row == Some(i);
                                    let main_st = if is_sel {
                                        Style::default()
                                            .fg(theme::user())
                                            .add_modifier(Modifier::BOLD)
                                    } else {
                                        Style::default().fg(theme::text())
                                    };
                                    let sub_st = Style::default().fg(theme::muted());
                                    let (chip, chip_st) = match action {
                                        ConnectAction::OAuthLogin(slug) => {
                                            if oauth_logged_in_for_slug(&auth_store, slug) {
                                                (
                                                    " connected ".to_string(),
                                                    Style::default()
                                                        .fg(Color::Black)
                                                        .bg(theme::success())
                                                        .add_modifier(Modifier::BOLD),
                                                )
                                            } else {
                                                (
                                                    format!(" login{} ", status_dots(anim_ms)),
                                                    Style::default()
                                                        .fg(Color::Black)
                                                        .bg(theme::warn())
                                                        .add_modifier(Modifier::BOLD),
                                                )
                                            }
                                        }
                                        ConnectAction::PromptApiKey(ProviderKind::OpenRouter) => {
                                            (
                                                " api key ".to_string(),
                                                Style::default()
                                                    .fg(Color::Black)
                                                    .bg(theme::warn())
                                                    .add_modifier(Modifier::BOLD),
                                            )
                                        }
                                        ConnectAction::PromptApiKey(_) => (
                                            " api key ".to_string(),
                                            Style::default()
                                                .fg(Color::Black)
                                                .bg(theme::warn())
                                                .add_modifier(Modifier::BOLD),
                                        ),
                                        ConnectAction::Submit(_) => {
                                            (
                                                " local ".to_string(),
                                                Style::default()
                                                    .fg(Color::Black)
                                                    .bg(theme::tool())
                                                    .add_modifier(Modifier::BOLD),
                                            )
                                        }
                                    };
                                    let prefix = if is_sel {
                                        cursor.to_string()
                                    } else {
                                        "  ".to_string()
                                    };
                                    lines.push(Line::from(vec![
                                        Span::styled(format!(" {prefix}{title}"), main_st),
                                        Span::styled(" ", Style::default().fg(theme::muted())),
                                        Span::styled(chip, chip_st),
                                        Span::styled(format!("  {subtitle}"), sub_st),
                                    ]));
                                }
                            }
                        }
                        let remaining_below = rows.len().saturating_sub(list_end);
                        if remaining_below > 0 {
                            lines.push(Line::from(Span::styled(
                                format!(" ▼ {} more", remaining_below),
                                Style::default().fg(theme::muted()),
                            )));
                        }
                    }
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        " ↑↓ move · Enter connect · Esc close ",
                        Style::default().fg(theme::muted()),
                    )));
                    frame.render_widget(ClearWidget, popup_area);
                    let title = Line::from(vec![
                        Span::styled(
                            format!(" {sparkle} Connect a provider {sparkle} "),
                            Style::default().fg(theme::muted()),
                        ),
                        Span::styled(" esc ", Style::default().fg(theme::muted())),
                    ]);
                    let popup = Paragraph::new(Text::from(lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::border()))
                                .title(title),
                        )
                        .style(Style::default().bg(theme::surface()))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }

                if g.anthropic_oauth_modal_open {
                    let popup_area = centered_rect(area, 92, 20);
                    let mut lines = vec![
                        Line::from(Span::styled(
                            " Open this URL in your browser, then paste the authorization code below. ",
                            Style::default().fg(theme::muted()),
                        )),
                        Line::default(),
                    ];
                    for wrapped in wrap_text(&g.anthropic_oauth_url, 78) {
                        lines.push(Line::from(Span::styled(
                            format!(" {wrapped}"),
                            Style::default().fg(theme::text()),
                        )));
                    }
                    lines.push(Line::default());
                    lines.push(Line::from(vec![
                        Span::styled(" Authorization code ", Style::default().fg(theme::muted())),
                        Span::styled(
                            g.anthropic_oauth_code_input.clone(),
                            Style::default().fg(theme::user()),
                        ),
                    ]));
                    lines.push(Line::default());
                    lines.push(Line::from(Span::styled(
                        " Paste: Ctrl+V / Shift+Insert · Enter confirm · Esc cancel ",
                        Style::default().fg(theme::muted()),
                    )));
                    frame.render_widget(ClearWidget, popup_area);
                    let popup = Paragraph::new(Text::from(lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::border()))
                                .title(Span::styled(
                                    " Anthropic OAuth login ",
                                    Style::default().fg(theme::muted()),
                                )),
                        )
                        .style(Style::default().bg(theme::surface()))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }

                // Approval popup overlay — drawn LAST so it sits above transcript &
                // any other modal-less surface. Skipped while the connect modal owns
                // the screen so onboarding flow isn't visually competing.
                if let Some(req) = g.active_approval.clone()
                    && !g.connect_modal_open
                    && !g.onboarding_mode
                {
                    render_approval_popup(frame, area, &req, g.approval_option_index);
                }
            })?;
        }

        let poll_timeout = if let Ok(g) = state.lock() {
            if g.busy || !matches!(g.current_busy_state, BusyState::Idle) {
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
                Event::Mouse(_) if g.session_picker_open => continue,
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
                        let (lines, _hits) = transcript_cache.get_or_rebuild(&g, inner_w);
                        let th = tr.height.saturating_sub(2) as usize;
                        scroll_buffer.replace_lines(lines.to_vec());
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
                                let (lines, _hits) =
                                    transcript_cache.get_or_rebuild(&g, inner_w_eff as u16);
                                scroll_buffer.replace_lines(lines.to_vec());
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
                                let (lines, _hits) =
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
                                    let n = text.chars().count();
                                    match crate::tui::mouse_select::copy_to_clipboard(&text) {
                                        Ok(msg) => {
                                            g.blocks.push(DisplayBlock::System(format!(
                                                " Copied {n} chars {msg}"
                                            )));
                                            g.touch_transcript();
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
                        let (_lines, hits) = transcript_cache.get_or_rebuild(&g, inner_w);
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
                                                    let feedback = match copy_to_clipboard(&text) {
                                                        Ok(_) => " Copied code block to clipboard"
                                                            .to_string(),
                                                        Err(e) => {
                                                            format!(" Clipboard copy failed: {e}")
                                                        }
                                                    };
                                                    g.blocks.push(DisplayBlock::System(feedback));
                                                    g.touch_transcript();
                                                    g.transcript_follow_tail = true;
                                                }
                                                LineClickHit::ToggleThinking => {
                                                    g.thinking_expanded = !g.thinking_expanded;
                                                    g.touch_transcript();
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
                Event::FocusGained | Event::FocusLost => {}
                Event::Key(key) => {
                    if matches!(key.kind, KeyEventKind::Release) {
                        continue;
                    }

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
                            g.blocks.push(DisplayBlock::System(
                                "Press Ctrl+C again within 1.5s to exit.".into(),
                            ));
                            g.touch_transcript();
                            g.transcript_follow_tail = true;
                        }
                        continue;
                    } else {
                        ctrl_c_armed_at = None;
                    }

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
                            g.blocks.push(DisplayBlock::System(msg));
                            g.touch_transcript();
                            g.transcript_follow_tail = true;
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
                                let (lines, _) = transcript_cache.get_or_rebuild(&g, inner_w);
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
                                let (lines, _) = transcript_cache.get_or_rebuild(&g, inner_w);
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
                                g.blocks.push(DisplayBlock::System(msg));
                                g.touch_transcript();
                            }
                            KeyCode::Char('F') => {
                                g.toggle_all_tool_blocks();
                                let state_msg = if g.all_tools_collapsed {
                                    "Collapsed all tool blocks (Ctrl+X F to expand)"
                                } else {
                                    "Expanded all tool blocks"
                                };
                                g.blocks.push(DisplayBlock::System(state_msg.into()));
                                g.touch_transcript();
                            }
                            _ => {}
                        }
                        continue;
                    }

                    match (key.code, key.modifiers) {
                        (KeyCode::Esc, _) if escape_cancels_active_turn(&g) => {
                            request_turn_cancel(&mut g, cancel_flag.as_ref(), &cmd_tx);
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
                            g.blocks.push(DisplayBlock::System(msg));
                            g.touch_transcript();
                            g.transcript_follow_tail = true;
                        }
                        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                            // Handled earlier in Event::Key with double-press semantics.
                        }
                        (KeyCode::Char('l'), KeyModifiers::CONTROL) => {
                            g.blocks.clear();
                            g.streaming_assistant = None;
                            g.streaming_thinking = None;
                            g.touch_transcript();
                            g.scroll_lines = 0;
                            g.transcript_follow_tail = true;
                            composer_history_index = None;
                            composer_history_draft.clear();
                        }
                        (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                            g.command_palette_open = true;
                            g.command_palette_query.clear();
                            g.palette_index = 0;
                        }
                        (KeyCode::Char('o'), KeyModifiers::CONTROL) => {
                            if g.pinned_notes.is_empty() {
                                g.blocks.push(DisplayBlock::System(
                                    "No pinned notes yet. Use Ctrl+K to pin the latest response."
                                        .into(),
                                ));
                                g.touch_transcript();
                                g.transcript_follow_tail = true;
                            } else {
                                g.open_pins_modal();
                            }
                        }
                        (KeyCode::Char('g'), KeyModifiers::CONTROL) => {
                            if g.subagents.is_empty() {
                                g.blocks.push(DisplayBlock::System(
                                    "No sub-agents to focus right now.".into(),
                                ));
                                g.touch_transcript();
                                g.transcript_follow_tail = true;
                            } else {
                                g.open_subagent_modal();
                            }
                        }
                        (KeyCode::Char('k'), KeyModifiers::CONTROL) => {
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
                            g.blocks.push(DisplayBlock::System(msg));
                            g.touch_transcript();
                            g.transcript_follow_tail = true;
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
                                    g.blocks.push(DisplayBlock::System(format!(
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
                        (KeyCode::F(2), KeyModifiers::NONE) => {
                            drop(g);
                            let _ = cmd_tx.send(TuiCmd::CycleModel(true));
                        }
                        (KeyCode::F(2), KeyModifiers::SHIFT) => {
                            drop(g);
                            let _ = cmd_tx.send(TuiCmd::CycleModel(false));
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
                                g.clear_input();
                                g.blocks.push(DisplayBlock::System(format!(
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
                        (KeyCode::Char('y'), KeyModifiers::CONTROL) => {
                            if let Some(req) = g.active_approval.clone() {
                                let call_id = req.call_id.clone();
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
                        (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
                            if let Some(req) = g.active_approval.clone() {
                                let call_id = req.call_id.clone();
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
                                g.clear_input();
                                g.blocks.push(DisplayBlock::System(format!(
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
                            let line = g.take_input_text();
                            g.slash_menu_index = 0;
                            let active_approval = g.active_approval.clone();
                            let active_q = g.active_question.clone();
                            if let Some(req) = active_approval {
                                let t = line.trim();
                                if t.is_empty() {
                                    let call_id = req.call_id.clone();
                                    let selection = g.approval_option_index.min(2);
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
                                    drop(g);
                                    if let Some(ref tx) = approval_answer_tx {
                                        let _ =
                                            tx.send(ApprovalAnswer::Verdict { call_id, approved });
                                    } else {
                                        let _ = cmd_tx.send(TuiCmd::CancelTurn);
                                    }
                                    continue;
                                }
                                g.blocks.push(DisplayBlock::System(
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
                                g.blocks.push(DisplayBlock::System(
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
                            let busy_turn =
                                g.busy || !matches!(g.current_busy_state, BusyState::Idle);
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
                            g.move_input_home();
                        }
                        (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                            g.move_input_end();
                        }
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
                                let (lines, _hits) =
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
                                let (lines, _hits) =
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
                                    let can_history = !composer_history.is_empty()
                                        && (g.input_buffer.is_empty()
                                            || composer_history_index.is_some());
                                    if can_history {
                                        if composer_history_index.is_none() {
                                            composer_history_draft = g.input_buffer.clone();
                                            composer_history_index =
                                                Some(composer_history.len().saturating_sub(1));
                                        } else if let Some(idx) = composer_history_index {
                                            composer_history_index = Some(idx.saturating_sub(1));
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
                                            let input_h = if should_hide_composer_when_scrolling(&g)
                                            {
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
                                            let (lines, _hits) = transcript_cache
                                                .get_or_rebuild(&g, tr.width.saturating_sub(2));
                                            let th = tr.height.saturating_sub(2) as usize;
                                            let w = tr.width.saturating_sub(2) as usize;
                                            scroll_buffer.replace_lines(lines.to_vec());
                                            scroll_buffer.set_from_top(g.scroll_lines, th, w);
                                            scroll_buffer.scroll_up(scroll_speed as usize, w, th);
                                            let (from_top, _) =
                                                scroll_buffer.scroll_position_from_top(th, w);
                                            g.scroll_lines = from_top as usize;
                                            g.transcript_follow_tail = scroll_buffer.is_sticky();
                                        }
                                    }
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
                                            let input_h = if should_hide_composer_when_scrolling(&g)
                                            {
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
                                            let (lines, _hits) = transcript_cache
                                                .get_or_rebuild(&g, tr.width.saturating_sub(2));
                                            let th = tr.height.saturating_sub(2) as usize;
                                            let w = tr.width.saturating_sub(2) as usize;
                                            scroll_buffer.replace_lines(lines.to_vec());
                                            scroll_buffer.set_from_top(g.scroll_lines, th, w);
                                            scroll_buffer.scroll_down(scroll_speed as usize);
                                            let (from_top, _) =
                                                scroll_buffer.scroll_position_from_top(th, w);
                                            g.scroll_lines = from_top as usize;
                                            g.transcript_follow_tail = scroll_buffer.is_sticky();
                                        }
                                    }
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
                _ => {}
            }
        }
    }

    restore_terminal(mouse_capture);
    let _ = execute!(stdout(), MoveToColumn(0));
    Ok(())
}

#[cfg(test)]
mod approval_parse_tests {
    use super::{
        TuiCmd, apply_selected_at_completion, completed_at_mention_range_before_cursor,
        composer_line, delete_completed_at_mention, escape_cancels_active_turn,
        filtered_branch_indices, is_click_jitter, mouse_scroll_step, parse_approval_verdict,
        pasted_lines_token, request_turn_cancel, stage_pasted_image_paths,
        transcript_lines_and_hits,
    };
    use crate::tui::branch_picker::branch_picker_enter_command;
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

    #[test]
    fn assistant_header_exposes_copy_click_target() {
        let mut state = TuiSessionState::new(
            "session".into(),
            "model".into(),
            "@build".into(),
            "AcceptEdits".into(),
            PathBuf::from("."),
            false,
        );
        state
            .blocks
            .push(super::DisplayBlock::Assistant("hello world".into()));
        let (_lines, hits) = transcript_lines_and_hits(&state, 80);
        assert!(hits.iter().any(|h| matches!(
            h,
            Some(super::LineClickHit::CopyText(text)) if text == "hello world"
        )));
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
        state.blocks.push(super::DisplayBlock::ToolDone {
            name: "execute_bash".into(),
            call_id: "call-1".into(),
            ok: true,
            detail: "ls -la".into(),
            duration_ms: Some(1200),
        });

        let (lines, _hits) = transcript_lines_and_hits(&state, 80);
        let flat = flatten_md(&lines).join("\n");
        // The header shows a DONE status chip; the body shows the command detail.
        assert!(flat.contains("DONE"), "status chip missing: {flat}");
        assert!(flat.contains("ls -la"), "tool detail missing: {flat}");
    }

    #[test]
    fn transcript_line_and_hit_counts_match() {
        // Every flattened line must have a parallel hit entry — the indices are
        // used together by mouse-click handling, so a mismatch is a real bug.
        let mut state = transcript_test_state();
        state.blocks.push(super::DisplayBlock::User("hi".into()));
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
}
