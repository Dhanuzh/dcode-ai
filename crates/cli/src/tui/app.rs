//! Full-screen session TUI: transcript, streaming assistant, composer.

#![allow(clippy::collapsible_match, clippy::explicit_into_iter_loop)]

use std::collections::HashSet;
use std::path::PathBuf;

use crate::file_mentions;
use crate::tui::answer_parse::{parse_approval_verdict, parse_tui_question_answer};
use crate::tui::composer_input::*;
use crate::tui::connect_modal::{ConnectAction, build_connect_rows, selectable_row_indices};
use crate::tui::layout::{layout_chunks, layout_with_sidebar};
use crate::tui::mouse::{is_click_jitter, mouse_left_activated, mouse_scroll_step, rect_contains};
use crate::tui::oauth_status::{oauth_logged_in_for_slug, oauth_switch_command_for_slug};
use crate::tui::paste::{expand_paste_tokens, pasted_lines_token};
use crate::tui::path_parse::{extract_embedded_path_fragments, parse_candidate_image_path};
use crate::tui::render::modals::{
    render_at_panel, render_command_palette, render_connect_modal, render_info_modal,
    render_slash_panel,
};
use crate::tui::slash_entries::{
    SLASH_PANEL_MAX_ROWS, SlashEntry, filter_slash_entries, load_slash_entries, slash_panel_height,
    slash_panel_visible,
};
use crate::tui::state::{
    BacktrackKey, BranchPickerKey, CommandPaletteKey, ConnectModalKey, DisplayBlock,
    HistorySearchKey, InfoModalKey, ModelPickerAction, ModelPickerKey, PinnedNote, PinsModalKey,
    ProviderPickerKey, ProviderPickerOutcome, QuestionModalKey, QuestionModalOutcome,
    SessionPickerKey, StagedImage, TuiSessionState,
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
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, BorderType, Borders, Clear as ClearWidget, Padding, Paragraph, Widget},
};
use std::io::stdout;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;

use crate::tui::keymap::translate_key;
use crate::tui::render::pickers::*;
use crate::tui::transcript_overlay::run_transcript_overlay;

mod chrome;
mod dashboard;
mod frame;
mod keys;
mod mouse;
mod vim;

use dashboard::draw_dashboard_frame;
use frame::draw_inline_frame;
use keys::{KeyFlow, handle_key_event};
use mouse::{handle_dashboard_mouse, handle_mouse_event};

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
    CopyText(String),
    /// Toggle full vs preview rendering of thinking blocks.
    ToggleThinking,
    /// Open a URL in the browser or a file path in the editor.
    OpenLink(String),
    /// Toggle collapse/expand of an assistant response block.
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
        scope: crate::tui::state::RewindScope,
    },
    /// Answer for the current `ask_question` (from question mode or `/auto-answer`).
    QuestionAnswer(dcode_ai_common::event::QuestionSelection),
    CycleAgent,
    /// Shift+Tab: Default → Accept edits → Plan → Default.
    CyclePermissionMode,
    /// Re-load MCP servers and hooks into the live session (`/reload`).
    ReloadExtensions,
    /// The sudo password typed into the popup (wiped after use).
    SudoPassword(crate::tui::sudo_prompt::Secret),
    /// The sudo popup was closed without a password.
    SudoCancelled,
    /// Run a `/plugins` browser action (install, update, …) off the UI thread.
    PluginAction(crate::tui::plugin_browser::BrowserAction),
    /// An action chosen in the `/mcp` panel.
    McpAction(crate::tui::mcp_panel::McpAction),
    /// An option picked in a decision prompt (sub-agent merge, memory tidy).
    Choice(crate::tui::choice::ChoiceAction, usize),
    /// `d` in a decision prompt: show what it is about (the branch diff).
    ChoiceDetail(crate::tui::choice::ChoiceAction),
    /// Leave plan mode and start building (`accept_edits` picks the mode).
    ExitPlan {
        accept_edits: bool,
    },
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
use crate::tui::diff_hunk::{build_hunk_modified_input, extract_approval_hunks};

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

/// Startup banner: a Claude-style welcome box (greeting, hints, cwd, model),
/// flushed once to the terminal scrollback at session start.
pub fn banner_lines(model: &str, workspace: &str, width: u16) -> Vec<Line<'static>> {
    let version = env!("CARGO_PKG_VERSION");
    // Claude-style welcome box: accent border, greeting, hints, cwd, model.
    let box_w = (width as usize).clamp(30, 60);
    let inner = box_w - 4;
    // Keep the start of `text`; shorten the path that follows `label`.
    let fit_path = |label: &str, path: &str| -> String {
        let room = inner.saturating_sub(label.chars().count()).max(4);
        let n = path.chars().count();
        if n <= room {
            format!("{label}{path}")
        } else {
            let tail: String = path.chars().skip(n - (room - 1)).collect();
            format!("{label}…{tail}")
        }
    };
    let fit = |text: &str| fit_path("", text);
    let home = std::env::var("HOME").unwrap_or_default();
    let workspace = match workspace.strip_prefix(home.as_str()) {
        Some(rest) if !home.is_empty() => format!("~{rest}"),
        _ => workspace.to_string(),
    };
    let border = Style::default().fg(theme::accent());
    let row = |spans: Vec<Span<'static>>| -> Line<'static> {
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        let mut out = vec![Span::styled("│ ", border)];
        out.extend(spans);
        out.push(Span::raw(" ".repeat(inner.saturating_sub(used))));
        out.push(Span::styled(" │", border));
        Line::from(out)
    };
    let muted = Style::default().fg(theme::muted());
    let mut lines: Vec<Line<'static>> = vec![
        Line::default(),
        Line::from(Span::styled(format!("╭{}╮", "─".repeat(box_w - 2)), border)),
        row(vec![
            Span::styled("✻ ", Style::default().fg(theme::accent())),
            Span::styled(
                "Welcome to dcode-ai!",
                Style::default()
                    .fg(theme::text())
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        row(vec![]),
        row(vec![Span::styled(
            fit("  /help for help, /model to switch model"),
            Style::default()
                .fg(theme::muted())
                .add_modifier(Modifier::ITALIC),
        )]),
        row(vec![]),
        row(vec![Span::styled(fit_path("  cwd: ", &workspace), muted)]),
        row(vec![Span::styled(
            fit(&format!("  model: {model} · v{version}")),
            muted,
        )]),
        Line::from(Span::styled(format!("╰{}╯", "─".repeat(box_w - 2)), border)),
        Line::default(),
        Line::from(Span::styled(" Tips for getting started:", muted)),
        Line::default(),
        Line::from(Span::styled(
            " 1. Ask dcode-ai to explain, fix or build something",
            muted,
        )),
        Line::from(Span::styled(
            " 2. Shift+Tab switches modes: accept edits, plan, auto",
            muted,
        )),
        Line::from(Span::styled(
            " 3. Esc twice rewinds code and conversation",
            muted,
        )),
    ];
    // A newer release, shown once per version, small, right under the box.
    if let Some(latest) = crate::update_check::take_pending_upgrade() {
        let under_box = lines
            .iter()
            .position(|l| l.spans.first().is_some_and(|s| s.content.starts_with('╰')))
            .map_or(lines.len(), |i| i + 1);
        lines.insert(
            under_box,
            Line::from(Span::styled(
                format!("  ⬆ v{latest} available (you have v{version}) · /update"),
                muted,
            )),
        );
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
        // Messages typed during the run go back into the prompt to edit,
        // not off as new turns.
        let mut unsent = state.take_unsent();
        if !unsent.is_empty() {
            if !state.input_buffer.is_empty() {
                unsent.push(state.input_buffer.clone());
            }
            state.set_input_text(unsent.join("\n"));
        }
        let _ = cmd_tx.send(TuiCmd::CancelTurn);
    }
}

/// Stage images named by paths in `pasted`. `inline` puts an `[Image #N]`
/// placeholder in the composer for each (paste / drag-and-drop); otherwise
/// they ride along with the message being submitted.
/// Read a clipboard image on a thread and attach it; with `text_fallback`,
/// clipboard text naming image files is imported instead.
fn spawn_clipboard_paste(state: Arc<Mutex<TuiSessionState>>, text_fallback: bool) {
    let Some((ws, sid)) = state
        .lock()
        .ok()
        .map(|g| (g.workspace_root.clone(), g.session_id.clone()))
    else {
        return;
    };
    std::thread::spawn(move || {
        let image = crate::image_attach::paste_clipboard_image(&ws, &sid);
        let text = match (&image, text_fallback) {
            (Err(_), true) => Clipboard::new().ok().and_then(|mut cb| cb.get_text().ok()),
            _ => None,
        };
        let Ok(mut g) = state.lock() else {
            return;
        };
        g.pasting = false;
        match image {
            Ok(att) => {
                g.attach_image_inline(att);
            }
            Err(image_err) if text_fallback => match text {
                Some(text) => match stage_pasted_image_paths(&mut g, &text, true) {
                    Ok(0) => g.push_error(format!("[image] {image_err}")),
                    Ok(_) => {}
                    Err(path_err) => g.push_error(format!(
                        "[image] {image_err}; clipboard path import failed: {path_err}"
                    )),
                },
                None => g.push_error(format!("[image] {image_err}")),
            },
            Err(_) => {}
        }
    });
}

fn stage_pasted_image_paths(
    state: &mut TuiSessionState,
    pasted: &str,
    inline: bool,
) -> Result<usize, String> {
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
                    if inline {
                        state.attach_image_inline(att);
                    } else {
                        state.staged_image_attachments.push(StagedImage {
                            token: None,
                            attachment: att,
                        });
                    }
                }
                Err(e) if first_error.is_none() => {
                    first_error = Some(e);
                }
                Err(_) => {}
            }
        }
    }

    if staged > 0 {
        return Ok(staged);
    }

    if tried_any {
        return Err(first_error.unwrap_or_else(|| "failed to import pasted image path".into()));
    }

    Ok(0)
}

fn insert_pasted_text(state: &mut TuiSessionState, slash_entries: &[SlashEntry], pasted: &str) {
    // Normalize Windows CRLF → LF before anything else. On Windows, clipboard
    // text uses \r\n; without this, \r ends up in the buffer (renders as garbage)
    // and in the paste store (sent to the AI with embedded carriage returns).
    let normalized = pasted.replace("\r\n", "\n").replace('\r', "\n");
    state.paste_counter += 1;
    let insert_text = match pasted_lines_token(&normalized, state.paste_counter) {
        Some(tok) => {
            state.paste_store.insert(tok.clone(), normalized);
            tok
        }
        None => normalized,
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
    view_mode: crate::tui::state::ViewMode,
    width: u16,
    revision: u64,
    code_line_numbers: bool,
    zoom_offset: i32,
    /// Running-tool bullet phase the lines were built with (`None` = no
    /// running tool), so the ● blinks.
    blink: Option<bool>,
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
        let stale = self.view_mode != state.view_mode
            || self.width != normalized_width
            || self.revision != state.transcript_rev
            || self.code_line_numbers != state.code_line_numbers
            || self.zoom_offset != state.transcript_zoom_offset;
        let blink = crate::tui::transcript::has_running_tool(state)
            .then(crate::tui::transcript::tool_blink_on);
        let stale = stale || self.blink != blink;
        if stale {
            self.blink = blink;
            let (lines, hits) = transcript_lines_and_hits(state, normalized_width);
            self.view_mode = state.view_mode;
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

// (Renderers moved to tui/render/modals.rs)

// (Renderers moved to tui/render/modals.rs)

// (Renderers moved to tui/render/modals.rs)

// (Renderers moved to tui/render/modals.rs)

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
    crate::tui::markdown::warm_up_highlighting();
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

    // Slash entries: hardcoded commands + discovered skills (reloaded when
    // plugins change).
    let skill_dirs = vec![PathBuf::from(".dcode-ai/skills")];
    let workspace_root = {
        let g = state.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        g.workspace_root.clone()
    };
    let mut slash_entries = load_slash_entries(&workspace_root, &skill_dirs);
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
    // Full-screen dashboard terminal (alternate screen), present while
    // `view_mode == Dashboard`. The inline terminal keeps its state meanwhile.
    let mut dash_terminal: Option<
        ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    > = None;
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
            // System notes (e.g. the remembered permission mode) don't count;
            // a resumed conversation does.
            let fresh = g
                .blocks
                .iter()
                .all(|b| matches!(b, crate::tui::state::DisplayBlock::System(_)));
            (g.model.clone(), ws, show_run_banner || fresh)
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
        // A newer release found by the background check after startup.
        if let Some(latest) = crate::update_check::take_pending_upgrade()
            && let Ok(mut g) = state.lock()
        {
            g.push_block(DisplayBlock::System(format!(
                "⬆ dcode-ai v{latest} is available · /update"
            )));
            g.touch_transcript();
        }
        // A clipboard paste waits for the clipboard off the UI thread.
        let paste = state.lock().ok().and_then(|mut g| {
            let r = g.paste_request.take();
            if r.is_some() {
                g.pasting = true;
            }
            r
        });
        if let Some(text_fallback) = paste {
            spawn_clipboard_paste(Arc::clone(&state), text_fallback);
        }

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
            if let Some(dt) = dash_terminal.as_mut() {
                // The overlay left the alternate screen; return to it.
                let _ = execute!(
                    stdout(),
                    crossterm::terminal::EnterAlternateScreen,
                    crossterm::event::EnableMouseCapture
                );
                crate::tui::terminal::push_key_flags();
                let _ = dt.clear();
            } else {
                let _ = terminal.clear();
            }
            continue;
        }

        {
            let mut g = state.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
            if g.should_exit {
                break;
            }

            // ── Hybrid view: enter/leave the full-screen dashboard ──
            let want_dashboard = g.view_mode == crate::tui::state::ViewMode::Dashboard;
            if want_dashboard && dash_terminal.is_none() {
                // The dashboard owns the screen, so it takes the mouse too:
                // without capture, terminals turn the wheel into arrow keys
                // and clicks never arrive. (The inline view leaves the mouse
                // to the terminal for native scrollback and selection.)
                let _ = execute!(
                    stdout(),
                    crossterm::terminal::EnterAlternateScreen,
                    crossterm::event::EnableMouseCapture
                );
                crate::tui::terminal::push_key_flags();
                match ratatui::Terminal::new(ratatui::backend::CrosstermBackend::new(stdout())) {
                    Ok(t) => dash_terminal = Some(t),
                    Err(e) => {
                        let _ = execute!(
                            stdout(),
                            crossterm::event::DisableMouseCapture,
                            crossterm::terminal::LeaveAlternateScreen
                        );
                        g.view_mode = crate::tui::state::ViewMode::Inline;
                        g.push_error(format!("dashboard unavailable: {e}"));
                    }
                }
                transcript_cache.invalidate();
            } else if !want_dashboard && dash_terminal.take().is_some() {
                crate::tui::terminal::pop_key_flags();
                let _ = execute!(
                    stdout(),
                    crossterm::event::DisableMouseCapture,
                    crossterm::terminal::LeaveAlternateScreen
                );
                // Repaint the inline pane; blocks produced meanwhile flush
                // into scrollback below.
                let _ = terminal.clear();
                transcript_cache.invalidate();
            }
            let in_dashboard = dash_terminal.is_some();
            if std::mem::take(&mut g.reload_slash_entries) {
                slash_entries = load_slash_entries(&workspace_root, &skill_dirs);
            }
            // Plan mode: a finished turn that ends in a reply offers to build.
            if was_busy
                && !g.busy
                && g.permission_mode.eq_ignore_ascii_case("plan")
                && g.active_question.is_none()
                && matches!(g.blocks.last(), Some(DisplayBlock::Assistant(_)))
            {
                g.plan_exit_index = Some(0);
            }
            // The turn may have edited files: refresh a visible changes tab.
            if in_dashboard
                && was_busy
                && !g.busy
                && g.rail_tab == crate::tui::state::RailTab::Changes
            {
                let ws = g.workspace_root.clone();
                g.diff_review.refresh(&ws);
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
            if g.request_clear && !in_dashboard {
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
            // Paused while the dashboard owns the screen; resumes on return.
            if !in_dashboard {
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

            if let Some(dt) = dash_terminal.as_mut() {
                dt.draw(|frame| {
                    draw_dashboard_frame(
                        frame,
                        &mut g,
                        &slash_filtered,
                        &at_matches,
                        chrome_h,
                        &mut transcript_cache,
                    );
                })?;
            } else {
                terminal.draw(|frame| {
                    draw_inline_frame(
                        frame,
                        &mut g,
                        &slash_filtered,
                        &at_matches,
                        chrome_h,
                        &mut transcript_cache,
                    );
                })?;
            }
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
                Event::Mouse(_)
                    if g.plugin_browser.is_some()
                        || g.mcp_panel.is_some()
                        || g.setup_picker.is_some()
                        || g.sudo_prompt.is_some() =>
                {
                    continue;
                }
                Event::Mouse(m) if g.view_mode == crate::tui::state::ViewMode::Dashboard => {
                    handle_dashboard_mouse(m, &mut g, &mut transcript_cache);
                }
                Event::Mouse(m) => {
                    let Ok(sz) = terminal.size() else {
                        continue;
                    };
                    handle_mouse_event(
                        m,
                        &mut g,
                        sz,
                        &slash_entries,
                        &workspace_files,
                        &mut transcript_cache,
                        &mut scroll_buffer,
                        scroll_speed,
                        &cmd_tx,
                    );
                }
                Event::Paste(pasted) => {
                    if let Some(panel) = g.mcp_panel.as_mut().filter(|p| p.in_auth()) {
                        panel.paste(&pasted.replace(['\r', '\n'], ""));
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
                        let staged_image = pasted.trim().is_empty();
                        if staged_image {
                            g.paste_request = Some(false);
                        }
                        if !staged_image {
                            match stage_pasted_image_paths(&mut g, &pasted, true) {
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
                    let terminal_size = terminal.size().ok();
                    let flow = handle_key_event(
                        key,
                        &mut g,
                        terminal_size,
                        &slash_entries,
                        &workspace_files,
                        &mut transcript_cache,
                        &mut scroll_buffer,
                        scroll_speed,
                        &mut composer_history,
                        &mut composer_history_index,
                        &mut composer_history_draft,
                        &mut ctrl_c_armed_at,
                        &cmd_tx,
                        question_answer_tx.as_ref(),
                        approval_answer_tx.as_ref(),
                        cancel_flag.as_ref(),
                    );
                    if flow == KeyFlow::Exit {
                        break;
                    }
                }
            }
        }
    }

    if dash_terminal.take().is_some() {
        crate::tui::terminal::pop_key_flags();
        let _ = execute!(
            stdout(),
            crossterm::event::DisableMouseCapture,
            crossterm::terminal::LeaveAlternateScreen
        );
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

fn keybindings_cheatsheet() -> Vec<String> {
    vec![
        "─── Essentials ───────────────────────────────────────".into(),
        "  Shift+Tab          Cycle mode: default → accept edits → plan".into(),
        "  Esc                Interrupt the running turn".into(),
        "  Esc Esc            Rewind code and/or conversation".into(),
        "  ! cmd              Run a shell command".into(),
        "  # note             Save a memory note".into(),
        "  @path              Mention a file".into(),
        "  /                  Commands (plugins: /plugin:command)".into(),
        "  1 / 2 / 3          Answer a permission prompt".into(),
        "  Ctrl+\\             Full-screen dashboard (Alt+1/2/3 rail tabs, Ctrl+D focus rail, ←/→ switch)".into(),
        "".into(),
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
        "  Enter (busy)       Send now: the agent reads it at its next step".into(),
        "  Tab / Alt+Enter    Queue for after the turn (when busy)".into(),
        "  ↑ (empty, busy)    Take back unsent messages to edit".into(),
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
        assert_eq!(
            crate::tui::render::tool_detail::diff_change_counts(diff),
            (2, 1)
        );
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
            input: String::new(),
            ok: true,
            detail: "ls -la".into(),
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
            Some("[Pasted text #1 +3 lines]".into())
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

        let staged = stage_pasted_image_paths(&mut state, "./assets/drop-test.png", true)
            .expect("staging should not error");
        assert_eq!(staged, 1);
        assert_eq!(state.staged_image_attachments.len(), 1);
        // Claude-style placeholder in the composer instead of the path.
        assert_eq!(state.input_buffer, "[Image #1] ");

        let rel = state.staged_image_attachments[0].attachment.path.clone();
        assert!(workspace.path().join(&rel).is_file());

        // Deleting the placeholder drops the image from the message.
        state.backspace_input(); // trailing space
        state.backspace_input(); // whole token
        assert_eq!(state.input_buffer, "");
        assert!(state.take_images_for_message("hello").is_empty());
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

    #[test]
    fn cancel_puts_unsent_messages_back_in_the_prompt() {
        let mut state = TuiSessionState::new(
            "session".into(),
            "model".into(),
            "@build".into(),
            "AcceptEdits".into(),
            PathBuf::from("."),
            false,
        );
        state.steer_inbox = Some(Default::default());
        state.set_busy_state(BusyState::Streaming);
        assert!(state.steer("use tabs".into()));
        state.followup_queue.push_back("then run tests".into());
        assert_eq!(state.unsent_count(), 2);
        state.set_input_text("draft");
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<TuiCmd>();
        let cancel = Arc::new(AtomicBool::new(false));
        request_turn_cancel(&mut state, Some(&cancel), &tx);
        assert_eq!(state.input_buffer, "use tabs\nthen run tests\ndraft");
        assert_eq!(state.unsent_count(), 0);
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
        let marker_rows = texts.iter().filter(|t| t.starts_with("> ")).count();
        let continuation_rows = texts
            .iter()
            .filter(|t| t.starts_with("  ") && !t.trim().is_empty())
            .count();
        assert_eq!(marker_rows, 1, "exactly one `>` marker: {texts:#?}");
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
            input: String::new(),
            ok: true,
            detail: "hello world\nexit code: 0".into(),
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
