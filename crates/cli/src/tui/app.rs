//! Full-screen session TUI: transcript, streaming assistant, composer.

#![allow(clippy::collapsible_match, clippy::explicit_into_iter_loop)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::file_mentions;
use crate::slash_commands::SLASH_COMMANDS;
use crate::tool_ui;
use crate::tui::connect_modal::{
    ConnectRow, build_connect_rows, clamp_selection, provider_at_selection,
    row_index_for_selection, selectable_row_indices, selection_pulse, status_dots, title_sparkle,
};
use crate::tui::state::{
    ApprovalRequest, DisplayBlock, ModelPickerAction, ModelPickerEntry, PinnedNote, TuiSessionState,
};
use arboard::Clipboard;
use crossterm::{
    cursor::{Hide, MoveToColumn, Show},
    event::{
        DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseButton, MouseEventKind, poll, read,
    },
    execute,
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode,
    },
};
use dcode_ai_common::auth::AuthStore;
use dcode_ai_common::config::ProviderKind;
use dcode_ai_common::event::{BusyState, QuestionSelection};
use dcode_ai_core::approval::suggest_allow_pattern;
use dcode_ai_core::skills::{SkillCatalog, SkillSource};
use pulldown_cmark::{Alignment, CodeBlockKind, Event as MdEvent, Options, Parser, Tag, TagEnd};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear as ClearWidget, Padding, Paragraph, Wrap},
};
use std::io::{Stdout, stdout};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::SyntaxSet;
use tokio::sync::mpsc::UnboundedSender;

/// Message from TUI to the approval dispatch task.
#[derive(Debug)]
pub enum ApprovalAnswer {
    Verdict { call_id: String, approved: bool },
    AllowPattern { call_id: String, pattern: String },
}

#[derive(Debug, Clone)]
enum LineClickHit {
    Question(QuestionSelection),
    CopyText(String),
}

/// Per flattened transcript line: click action (same indices as `transcript_lines`).
type LineAnswerHit = Option<LineClickHit>;

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

const SLASH_PANEL_MAX_ROWS: usize = 8;
const COMMAND_PALETTE_WIDTH: u16 = 48;
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

fn slash_panel_visible(buffer: &str) -> bool {
    buffer.starts_with('/') && !buffer.contains(' ')
}

fn oauth_login_provider_slug(kind: ProviderKind) -> Option<&'static str> {
    match kind {
        ProviderKind::OpenAi => Some("openai"),
        ProviderKind::Anthropic => Some("anthropic"),
        ProviderKind::Antigravity => Some("antigravity"),
        // Copilot uses the OpenAI provider surface at runtime, but auth is a distinct login flow.
        ProviderKind::OpenCodeZen | ProviderKind::OpenRouter => None,
    }
}

fn cursor_byte_index(line: &str, cursor_char_idx: usize) -> usize {
    line.char_indices()
        .nth(cursor_char_idx)
        .map(|(i, _)| i)
        .unwrap_or(line.len())
}

fn at_panel_height(n: usize) -> u16 {
    if n == 0 {
        return 0;
    }
    (n.min(SLASH_PANEL_MAX_ROWS) as u16).saturating_add(2)
}

fn at_completion_active(buffer: &str, cursor_char_idx: usize) -> bool {
    if slash_panel_visible(buffer) {
        return false;
    }
    let b = cursor_byte_index(buffer, cursor_char_idx);
    file_mentions::at_token_before_cursor(buffer, b).is_some()
}

fn at_completion_matches(
    workspace_files: &[String],
    buffer: &str,
    cursor_char_idx: usize,
) -> Vec<String> {
    if !at_completion_active(buffer, cursor_char_idx) {
        return Vec::new();
    }
    let b = cursor_byte_index(buffer, cursor_char_idx);
    let Some((_, prefix)) = file_mentions::at_token_before_cursor(buffer, b) else {
        return Vec::new();
    };
    file_mentions::filter_paths_prefix(workspace_files, &prefix)
}

fn composer_chrome_height(
    slash_entries: &[SlashEntry],
    workspace_files: &[String],
    buffer: &str,
    cursor_char_idx: usize,
) -> u16 {
    let slash_filtered = filter_slash_entries(slash_entries, buffer);
    let at_matches = at_completion_matches(workspace_files, buffer, cursor_char_idx);
    let slash_h = if slash_panel_visible(buffer) {
        slash_panel_height(slash_filtered.len())
    } else {
        0
    };
    let at_h = if !at_matches.is_empty() {
        at_panel_height(at_matches.len())
    } else {
        0
    };
    slash_h.max(at_h)
}

fn composer_input_height(state: &TuiSessionState, area_width: u16) -> u16 {
    let inner_w = area_width.saturating_sub(4).max(1) as usize;
    let prompt_w = 2usize; // "› "
    let raw_lines: Vec<&str> = state.input_buffer.split('\n').collect();
    let mut wrapped_input_lines = 0usize;
    for (idx, line) in raw_lines.iter().enumerate() {
        let cells = line.chars().count() + usize::from(idx == 0) * prompt_w;
        wrapped_input_lines += cells.max(1).div_ceil(inner_w);
    }
    let wrapped_input_lines = wrapped_input_lines.max(1) as u16;

    let mut content_lines = wrapped_input_lines;
    if !state.staged_image_attachments.is_empty() {
        content_lines = content_lines.saturating_add(1);
    }
    let show_hint = state.active_approval.is_some()
        || (state.active_question.is_some() && !state.question_modal_open)
        || state.busy
        || !matches!(state.current_busy_state, BusyState::Idle);
    if show_hint {
        content_lines = content_lines.saturating_add(1);
    }

    content_lines.saturating_add(2).clamp(3, 11)
}

fn should_hide_composer_when_scrolling(state: &TuiSessionState) -> bool {
    !state.transcript_follow_tail
        && state.input_buffer.trim().is_empty()
        && state.staged_image_attachments.is_empty()
        && state.active_approval.is_none()
        && state.active_question.is_none()
        && !state.busy
        && matches!(state.current_busy_state, BusyState::Idle)
}

/// Replace `@prefix` before cursor with `@choice` (relative path).
fn apply_at_completion(buffer: &str, cursor_char_idx: usize, choice: &str) -> (String, usize) {
    let b = cursor_byte_index(buffer, cursor_char_idx);
    let Some((at_byte, _prefix)) = file_mentions::at_token_before_cursor(buffer, b) else {
        return (buffer.to_string(), cursor_char_idx);
    };
    let before = &buffer[..at_byte.saturating_add(1)];
    let after = &buffer[b..];
    let new_buf = format!("{before}{choice}{after}");
    let new_byte = at_byte + 1 + choice.len();
    let new_char = new_buf[..new_byte.min(new_buf.len())].chars().count();
    (new_buf, new_char)
}

fn apply_selected_at_completion(
    workspace_files: &[String],
    buffer: &str,
    cursor_char_idx: usize,
    at_menu_index: usize,
    append_space: bool,
) -> Option<(String, usize)> {
    let at_matches = at_completion_matches(workspace_files, buffer, cursor_char_idx);
    if at_matches.is_empty() || !at_completion_active(buffer, cursor_char_idx) {
        return None;
    }

    let pick = at_menu_index.min(at_matches.len().saturating_sub(1));
    let choice = at_matches.get(pick)?;
    let (mut new_buf, mut new_cursor_char_idx) =
        apply_at_completion(buffer, cursor_char_idx, choice);

    if append_space {
        let insert_at = cursor_byte_index(&new_buf, new_cursor_char_idx);
        new_buf.insert(insert_at, ' ');
        new_cursor_char_idx += 1;
    }

    Some((new_buf, new_cursor_char_idx))
}

fn at_mention_char_ranges(buffer: &str) -> Vec<(usize, usize)> {
    file_mentions::parse_at_mentions(buffer)
        .into_iter()
        .map(|(start, end, _)| {
            let start_char = buffer[..start].chars().count();
            let end_char = buffer[..end].chars().count();
            (start_char, end_char)
        })
        .collect()
}

fn completed_at_mention_range_before_cursor(
    buffer: &str,
    cursor_char_idx: usize,
) -> Option<(usize, usize)> {
    let chars: Vec<char> = buffer.chars().collect();
    for (start_char, end_char) in at_mention_char_ranges(buffer) {
        if end_char == cursor_char_idx {
            return Some((start_char, end_char));
        }
        if end_char < chars.len()
            && end_char + 1 == cursor_char_idx
            && chars.get(end_char) == Some(&' ')
        {
            return Some((start_char, end_char + 1));
        }
    }
    None
}

fn remove_char_range(buffer: &str, start_char_idx: usize, end_char_idx: usize) -> String {
    let mut chars: Vec<char> = buffer.chars().collect();
    chars.drain(start_char_idx..end_char_idx);
    chars.into_iter().collect()
}

fn delete_completed_at_mention(buffer: &str, cursor_char_idx: usize) -> Option<(String, usize)> {
    let (start_char, end_char) = completed_at_mention_range_before_cursor(buffer, cursor_char_idx)?;
    Some((remove_char_range(buffer, start_char, end_char), start_char))
}

fn push_styled_run(
    spans: &mut Vec<Span<'static>>,
    text: &mut String,
    current_style: &mut Option<Style>,
    style: Style,
    ch: char,
) {
    if current_style.as_ref() != Some(&style) && !text.is_empty() {
        spans.push(Span::styled(
            std::mem::take(text),
            current_style.unwrap_or_default(),
        ));
    }
    *current_style = Some(style);
    text.push(ch);
}

fn composer_line(buffer: &str, cursor_char_idx: usize) -> Line<'static> {
    let prompt = Span::styled("› ", Style::default().fg(theme::user()).bold());
    let placeholder = "Ask anything... (Shift+Enter for newline, / for commands, /keymaps)";
    let chars: Vec<char> = buffer.chars().collect();
    let mention_ranges = at_mention_char_ranges(buffer);
    let cursor_char_idx = cursor_char_idx.min(chars.len());
    let mut spans = vec![prompt];
    let mut run = String::new();
    let mut run_style: Option<Style> = None;

    for idx in 0..=chars.len() {
        if idx == cursor_char_idx {
            let cursor_char = chars.get(idx).copied().unwrap_or(' ');
            let in_mention = idx < chars.len()
                && mention_ranges
                    .iter()
                    .any(|(start, end)| *start <= idx && idx < *end);
            let cursor_style = if in_mention {
                Style::default()
                    .bg(theme::user())
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .bg(theme::muted())
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD)
            };
            push_styled_run(
                &mut spans,
                &mut run,
                &mut run_style,
                cursor_style,
                cursor_char,
            );
            if idx == chars.len() {
                break;
            }
            continue;
        }

        let Some(ch) = chars.get(idx).copied() else {
            break;
        };
        let in_mention = mention_ranges
            .iter()
            .any(|(start, end)| *start <= idx && idx < *end);
        let style = if in_mention {
            Style::default()
                .fg(theme::text())
                .bg(theme::mention_bg())
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::text())
        };
        push_styled_run(&mut spans, &mut run, &mut run_style, style, ch);
    }

    if !run.is_empty() {
        spans.push(Span::styled(run, run_style.unwrap_or_default()));
    }
    if buffer.is_empty() {
        spans.push(Span::styled(
            placeholder.to_string(),
            Style::default().fg(theme::muted()),
        ));
    }

    Line::from(spans)
}

/// Entry for the slash panel: either a hardcoded command or a discovered skill.
#[derive(Clone)]
pub enum SlashEntry {
    Command(&'static str),
    Skill {
        command: String,
        description: Option<String>,
        source: SkillSource,
    },
}

impl SlashEntry {
    fn command_str(&self) -> String {
        match self {
            SlashEntry::Command(s) => s.to_string(),
            SlashEntry::Skill { command, .. } => format!("/{command}"),
        }
    }

    fn display_text(&self) -> String {
        match self {
            SlashEntry::Command(s) => s.to_string(),
            SlashEntry::Skill {
                command,
                description,
                source,
            } => {
                let tag = match source {
                    SkillSource::AgentsMd => " (AGENTS.md)",
                    SkillSource::FileSystem => " (skill dir)",
                };
                match description {
                    Some(desc) => format!("/{command:<20} — {desc}{tag}"),
                    None => format!("/{command}{tag}"),
                }
            }
        }
    }
}

/// Collect skills from SkillCatalog for slash panel display.
fn collect_skill_entries(workspace_root: &Path, skill_dirs: &[PathBuf]) -> Vec<SlashEntry> {
    match SkillCatalog::discover(workspace_root, skill_dirs) {
        Ok(skills) => skills
            .into_iter()
            .map(|s| SlashEntry::Skill {
                command: s.command,
                description: s.description,
                source: s.source,
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Load all slash-commands: hardcoded commands + discovered skills.
fn load_slash_entries(workspace_root: &Path, skill_dirs: &[PathBuf]) -> Vec<SlashEntry> {
    let mut entries: Vec<SlashEntry> = SLASH_COMMANDS
        .iter()
        .map(|c| SlashEntry::Command(c))
        .collect();

    // Add discovered skills
    entries.extend(collect_skill_entries(workspace_root, skill_dirs));

    // Sort by command name
    entries.sort_by(|a, b| {
        a.command_str()
            .to_lowercase()
            .cmp(&b.command_str().to_lowercase())
    });
    entries.dedup_by(|a, b| a.command_str().eq_ignore_ascii_case(&b.command_str()));
    entries
}

/// Filter slash entries by buffer prefix.
fn filter_slash_entries<'a>(entries: &'a [SlashEntry], buffer: &str) -> Vec<&'a SlashEntry> {
    if !slash_panel_visible(buffer) {
        return Vec::new();
    }
    let needle = buffer.trim_start_matches('/').to_lowercase();
    entries
        .iter()
        .filter(|e| {
            e.command_str()
                .trim_start_matches('/')
                .to_lowercase()
                .starts_with(&needle)
        })
        .collect()
}

fn branch_filter_text(query: &str) -> &str {
    query.trim().strip_prefix('/').unwrap_or(query.trim())
}

fn filtered_branch_indices(branches: &[String], query: &str) -> Vec<usize> {
    let filter = branch_filter_text(query).to_ascii_lowercase();
    if filter.is_empty() {
        return (0..branches.len()).collect();
    }
    branches
        .iter()
        .enumerate()
        .filter(|(_, branch)| branch.to_ascii_lowercase().contains(&filter))
        .map(|(idx, _)| idx)
        .collect()
}

fn branch_picker_enter_command(
    branches: &[String],
    query: &str,
    selected_filtered_idx: usize,
) -> Option<TuiCmd> {
    let raw_query = query.trim();
    let branch_name = branch_filter_text(raw_query).trim();
    let filtered = filtered_branch_indices(branches, raw_query);

    if raw_query.starts_with('/') {
        return (!branch_name.is_empty()).then(|| TuiCmd::CreateBranch(branch_name.to_string()));
    }

    if !branch_name.is_empty()
        && let Some((idx, _)) = branches
            .iter()
            .enumerate()
            .find(|(_, branch)| branch.eq_ignore_ascii_case(branch_name))
    {
        return Some(TuiCmd::SwitchBranch(branches[idx].clone()));
    }

    filtered
        .get(selected_filtered_idx)
        .copied()
        .map(|idx| TuiCmd::SwitchBranch(branches[idx].clone()))
}

/// A row in the categorized command palette.
#[derive(Clone)]
enum PaletteRow {
    Section(&'static str),
    Entry {
        label: &'static str,
        shortcut: &'static str,
    },
}

const PALETTE_CATALOG: &[PaletteRow] = &[
    PaletteRow::Section("Suggested"),
    PaletteRow::Entry {
        label: "Switch model",
        shortcut: "ctrl+x m",
    },
    PaletteRow::Entry {
        label: "Connect provider",
        shortcut: "",
    },
    PaletteRow::Section("Session"),
    PaletteRow::Entry {
        label: "Open editor",
        shortcut: "ctrl+x e",
    },
    PaletteRow::Entry {
        label: "Switch session",
        shortcut: "ctrl+x l",
    },
    PaletteRow::Entry {
        label: "New session",
        shortcut: "ctrl+x n",
    },
    PaletteRow::Entry {
        label: "Compact",
        shortcut: "ctrl+x c",
    },
    PaletteRow::Entry {
        label: "Export session",
        shortcut: "",
    },
    PaletteRow::Section("Prompt"),
    PaletteRow::Entry {
        label: "Skills",
        shortcut: "",
    },
    PaletteRow::Entry {
        label: "Agent profile",
        shortcut: "ctrl+x a",
    },
    PaletteRow::Entry {
        label: "Toggle thinking",
        shortcut: "",
    },
    PaletteRow::Section("Provider"),
    PaletteRow::Entry {
        label: "Connect provider",
        shortcut: "",
    },
    PaletteRow::Entry {
        label: "Switch provider",
        shortcut: "",
    },
    PaletteRow::Section("System"),
    PaletteRow::Entry {
        label: "View status",
        shortcut: "ctrl+x s",
    },
    PaletteRow::Entry {
        label: "Config",
        shortcut: "",
    },
    PaletteRow::Entry {
        label: "Doctor",
        shortcut: "",
    },
    PaletteRow::Entry {
        label: "Help",
        shortcut: "ctrl+x h",
    },
    PaletteRow::Entry {
        label: "Keymaps",
        shortcut: "",
    },
    PaletteRow::Entry {
        label: "Permissions",
        shortcut: "",
    },
    PaletteRow::Entry {
        label: "Memory",
        shortcut: "",
    },
    PaletteRow::Entry {
        label: "Logs",
        shortcut: "",
    },
    PaletteRow::Entry {
        label: "MCP servers",
        shortcut: "",
    },
    PaletteRow::Entry {
        label: "Clear screen",
        shortcut: "ctrl+l",
    },
    PaletteRow::Entry {
        label: "Exit",
        shortcut: "ctrl+x q",
    },
];

fn palette_command_for_label(label: &str) -> &'static str {
    match label {
        "Switch model" => "/models",
        "Connect provider" => "/connect",
        "Open editor" => "/editor",
        "Switch session" => "/sessions",
        "New session" => "/new",
        "Compact" => "/compact",
        "Export session" => "/export",
        "Skills" => "/skills",
        "Agent profile" => "/agent",
        "Toggle thinking" => "/thinking",
        "Switch provider" => "/provider",
        "View status" => "/status",
        "Config" => "/config",
        "Doctor" => "/doctor",
        "Help" => "/help",
        "Keymaps" => "/keymaps",
        "Permissions" => "/permissions",
        "Memory" => "/memory",
        "Logs" => "/logs",
        "MCP servers" => "/mcp",
        "Clear screen" => "/clear",
        "Exit" => "/exit",
        _ => "/help",
    }
}

fn filter_palette_rows(query: &str) -> Vec<&'static PaletteRow> {
    let needle = query.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return PALETTE_CATALOG.iter().collect();
    }
    let mut result: Vec<&'static PaletteRow> = Vec::new();
    let mut pending_section: Option<&'static PaletteRow> = None;
    for row in PALETTE_CATALOG {
        match row {
            PaletteRow::Section(_) => {
                pending_section = Some(row);
            }
            PaletteRow::Entry { label, shortcut } => {
                if label.to_ascii_lowercase().contains(&needle)
                    || shortcut.to_ascii_lowercase().contains(&needle)
                    || palette_command_for_label(label).contains(&needle)
                {
                    if let Some(s) = pending_section.take() {
                        result.push(s);
                    }
                    result.push(row);
                }
            }
        }
    }
    result
}

fn palette_selectable_indices(rows: &[&PaletteRow]) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter_map(|(i, r)| matches!(r, PaletteRow::Entry { .. }).then_some(i))
        .collect()
}

fn slash_panel_height(filtered_len: usize) -> u16 {
    if filtered_len == 0 {
        return 0;
    }
    let rows = filtered_len.min(SLASH_PANEL_MAX_ROWS);
    let footer = if filtered_len > SLASH_PANEL_MAX_ROWS {
        1
    } else {
        0
    };
    // borders (2) + command rows + optional footer
    (rows as u16)
        .saturating_add(footer)
        .saturating_add(2)
        .min(14)
}

fn layout_chunks(
    area: Rect,
    slash_h: u16,
    input_h: u16,
    queue_total: usize,
    activity_total: usize,
) -> (Rect, Rect, Option<Rect>, Rect) {
    let vp = crate::tui::tui_viewport::layout(area, slash_h, input_h, queue_total, activity_total);
    (vp.transcript, vp.status, vp.slash, vp.input)
}

fn sidebar_fit(s: &str, max_chars: usize) -> String {
    let t = s.trim();
    if t.chars().count() <= max_chars {
        t.to_string()
    } else {
        format!(
            "{}…",
            t.chars()
                .take(max_chars.saturating_sub(1))
                .collect::<String>()
        )
    }
}

fn layout_with_sidebar(area: Rect, _sidebar_open: bool) -> (Rect, Option<Rect>) {
    // Fullscreen-only layout: right sidebar removed.
    // Context/session details are command-driven (/status, /config, etc.).
    (area, None)
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    const POPUP_W_PAD: u16 = 10;
    const POPUP_H_PAD: u16 = 3;
    let target_w = width.saturating_add(POPUP_W_PAD);
    let target_h = height.saturating_add(POPUP_H_PAD);
    let popup_w = target_w
        .min(area.width.saturating_sub(2).max(20))
        .min(area.width);
    let popup_h = target_h
        .min(area.height.saturating_sub(2).max(6))
        .min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(popup_w) / 2,
        area.y + area.height.saturating_sub(popup_h) / 2,
        popup_w,
        popup_h,
    )
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

fn pasted_lines_token(pasted: &str, counter: u32) -> Option<String> {
    let normalized = pasted.replace("\r\n", "\n").replace('\r', "\n");
    let trimmed = normalized.trim_end_matches('\n');
    let line_count = if trimmed.is_empty() {
        0
    } else {
        trimmed.split('\n').count()
    };
    (line_count > 1).then(|| format!("[pasted {line_count} lines #{counter}]"))
}

fn expand_paste_tokens(text: &str, store: &std::collections::HashMap<String, String>) -> String {
    let mut result = text.to_string();
    for (token, content) in store {
        result = result.replace(token.as_str(), content.as_str());
    }
    result
}

fn strip_outer_quotes(s: &str) -> &str {
    let t = s.trim();
    if t.len() >= 2 {
        let first = t.as_bytes()[0] as char;
        let last = t.as_bytes()[t.len() - 1] as char;
        if matches!((first, last), ('\'', '\'') | ('"', '"') | ('`', '`')) {
            return &t[1..t.len() - 1];
        }
    }
    t
}

fn normalize_file_url_path(raw: &str) -> Option<PathBuf> {
    if !raw.starts_with("file://") {
        return None;
    }

    if let Ok(url) = url::Url::parse(raw)
        && url.scheme() == "file"
        && let Ok(path) = url.to_file_path()
    {
        return Some(path);
    }

    let decoded = urlencoding::decode(raw.strip_prefix("file://")?)
        .ok()?
        .into_owned();
    Some(PathBuf::from(decoded))
}

fn looks_like_windows_drive_path(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
}

fn windows_drive_path_to_wsl(raw: &str) -> Option<PathBuf> {
    if !looks_like_windows_drive_path(raw) {
        return None;
    }
    let drive = raw.chars().next()?.to_ascii_lowercase();
    let rest = raw[3..].replace('\\', "/");
    Some(PathBuf::from(format!("/mnt/{drive}/{rest}")))
}

fn unescape_shell_path(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\'
            && let Some(next) = chars.peek().copied()
            && matches!(
                next,
                ' ' | '\'' | '"' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | '\\'
            )
        {
            out.push(next);
            chars.next();
            continue;
        }
        out.push(ch);
    }
    out
}

fn looks_like_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "webp" | "gif"
            )
        })
        .unwrap_or(false)
}

fn path_looks_explicit(raw: &str, path: &Path) -> bool {
    path.is_absolute()
        || raw.contains('/')
        || raw.contains('\\')
        || raw.starts_with("./")
        || raw.starts_with("../")
}

fn parse_candidate_image_path(raw_line: &str) -> Option<PathBuf> {
    let raw = strip_outer_quotes(raw_line);
    if raw.is_empty() {
        return None;
    }

    let mut candidate = normalize_file_url_path(raw)
        .or_else(|| windows_drive_path_to_wsl(raw))
        .unwrap_or_else(|| PathBuf::from(unescape_shell_path(raw)));
    if !looks_like_image_path(&candidate) {
        return None;
    }

    let candidate_text = candidate.to_string_lossy().into_owned();
    if !path_looks_explicit(raw, &candidate) && !path_looks_explicit(&candidate_text, &candidate) {
        return None;
    }

    // Handle file:///C:/... URLs on Unix-like systems by mapping to /mnt/<drive>/...
    if cfg!(not(windows))
        && let Some(s) = candidate.to_str()
        && s.len() >= 4
        && s.starts_with('/')
        && s.as_bytes()[1].is_ascii_alphabetic()
        && s.as_bytes()[2] == b':'
        && s.as_bytes()[3] == b'/'
        && let Some(mapped) = windows_drive_path_to_wsl(&s[1..])
    {
        candidate = mapped;
    }

    Some(candidate)
}

fn extract_quoted_fragments(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    for quote in ['"', '\'', '`'] {
        let mut start: Option<usize> = None;
        for (idx, ch) in line.char_indices() {
            if ch == quote {
                if let Some(s) = start.take() {
                    if idx > s + 1 {
                        out.push(line[s..=idx].to_string());
                    }
                } else {
                    start = Some(idx);
                }
            }
        }
    }
    out
}

fn extract_embedded_path_fragments(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let image_exts = [".png", ".jpg", ".jpeg", ".webp", ".gif"];
    let trimmed = line.trim();
    if !trimmed.is_empty() {
        out.push(trimmed.to_string());
    }
    out.extend(extract_quoted_fragments(trimmed));

    let bytes = trimmed.as_bytes();
    for (idx, b) in bytes.iter().enumerate() {
        let looks_unix = *b == b'/';
        let looks_windows = if idx + 2 < bytes.len() {
            bytes[idx].is_ascii_alphabetic()
                && bytes[idx + 1] == b':'
                && matches!(bytes[idx + 2], b'\\' | b'/')
        } else {
            false
        };
        if !(looks_unix || looks_windows) {
            continue;
        }

        for ext in image_exts {
            let mut search_from = idx;
            while let Some(found) = trimmed[search_from..].find(ext) {
                let end = search_from + found + ext.len();
                if end <= idx {
                    search_from += found + ext.len();
                    continue;
                }
                let candidate = trimmed[idx..end].trim().trim_end_matches(|c: char| {
                    matches!(c, ')' | ']' | '}' | '"' | '\'' | '`' | ',' | ';' | ':')
                });
                if !candidate.is_empty() {
                    out.push(candidate.to_string());
                }
                search_from += found + ext.len();
            }
        }
    }

    out
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

fn rect_contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x
        && col < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
}

fn mouse_left_activated(kind: MouseEventKind) -> bool {
    matches!(kind, MouseEventKind::Up(MouseButton::Left))
}

fn is_click_jitter(selection: &crate::tui::mouse_select::Selection) -> bool {
    selection.anchor.row == selection.cursor.row
        && selection.anchor.col.abs_diff(selection.cursor.col) <= 1
}

fn mouse_scroll_step(modifiers: KeyModifiers, viewport_lines: usize, base_step: usize) -> usize {
    let base = base_step.max(1);
    if modifiers.contains(KeyModifiers::CONTROL) {
        viewport_lines.max(1).saturating_mul(3).max(base)
    } else if modifiers.contains(KeyModifiers::SHIFT) {
        viewport_lines.max(1).max(base)
    } else {
        base
    }
}

/// Run a git command synchronously and return stdout.
fn git_run(args: &[&str], cwd: Option<&Path>) -> Option<String> {
    let cwd = cwd?;
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if output.status.success() {
        Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        None
    }
}

/// Get the current git branch name for `workspace`.
pub fn git_current_branch(workspace: &Path) -> Option<String> {
    git_run(&["rev-parse", "--abbrev-ref", "HEAD"], Some(workspace))
}

/// List local git branches for `workspace`. Current branch is marked with `*`.
pub fn git_list_branches(workspace: &Path) -> Vec<String> {
    git_run(&["branch", "--no-color"], Some(workspace))
        .map(|out| {
            out.lines()
                .map(|l| {
                    l.trim_start_matches("* ")
                        .trim_start_matches("+ ")
                        .trim()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Create a new branch `name` and check it out in `workspace`.
pub fn git_create_branch(workspace: &Path, name: &str) -> bool {
    git_run(&["checkout", "-b", name], Some(workspace)).is_some()
}

/// Switch to an existing branch `name` in `workspace`.
pub fn git_switch_branch(workspace: &Path, name: &str) -> bool {
    git_run(&["checkout", name], Some(workspace)).is_some()
}

pub fn setup_terminal(mouse_capture: bool) -> anyhow::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().map_err(|e| anyhow::anyhow!("enable_raw_mode: {e}"))?;
    let res: anyhow::Result<Terminal<CrosstermBackend<Stdout>>> = (|| {
        let mut out = stdout();
        execute!(out, EnterAlternateScreen)?;
        let _ = execute!(out, EnableBracketedPaste);
        use std::io::Write;
        if mouse_capture {
            // Koda-style selective mouse capture:
            // - ?1002h button-event tracking (includes drag with button held)
            // - ?1006h SGR extended coordinates
            // This enables click-drag range selection in the in-app transcript.
            out.write_all(b"\x1b[?1002h\x1b[?1006h")
                .map_err(|e| anyhow::anyhow!("mouse enable: {e}"))?;
        }
        let _ = out.flush();
        execute!(out, Hide)?;
        execute!(out, Clear(ClearType::All))?;
        Ok(Terminal::new(CrosstermBackend::new(out))?)
    })();
    if res.is_err() {
        let _ = disable_raw_mode();
    }
    res
}

pub fn restore_terminal(_mouse_capture: bool) {
    let mut out = stdout();
    let _ = execute!(out, Show);
    let _ = execute!(out, DisableBracketedPaste);
    use std::io::Write;
    let _ = out.write_all(b"\x1b[?1002l\x1b[?1006l");
    let _ = out.flush();
    let _ = execute!(out, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

#[inline]
fn push_transcript_line(
    lines: &mut Vec<Line<'static>>,
    hits: &mut Vec<LineAnswerHit>,
    line: Line<'static>,
    hit: LineAnswerHit,
) {
    lines.push(line);
    hits.push(hit);
}

fn prefixed_line(prefix: Span<'static>, mut line: Line<'static>) -> Line<'static> {
    let mut spans = Vec::with_capacity(line.spans.len() + 1);
    spans.push(prefix);
    spans.append(&mut line.spans);
    Line::from(spans)
}

fn line_has_text(line: &Line<'_>) -> bool {
    line.spans.iter().any(|s| !s.content.trim().is_empty())
}

fn push_section_gap(lines: &mut Vec<Line<'static>>, hits: &mut Vec<LineAnswerHit>) {
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

fn subagent_phase_progress(phase: &str, running: bool) -> u8 {
    let p = phase.to_ascii_lowercase();
    if !running
        || p.contains("done")
        || p.contains("complete")
        || p.contains("success")
        || p.contains("finished")
    {
        return 100;
    }
    if p.contains("spawn") || p.contains("queue") {
        15
    } else if p.contains("plan") {
        30
    } else if p.contains("search") || p.contains("inspect") || p.contains("read") {
        45
    } else if p.contains("edit") || p.contains("write") || p.contains("patch") {
        70
    } else if p.contains("test") || p.contains("verify") {
        85
    } else {
        55
    }
}

fn progress_bar(percent: u8, width: usize) -> String {
    let w = width.max(8);
    let filled = (usize::from(percent) * w) / 100;
    let mut out = String::with_capacity(w + 10);
    out.push('[');
    out.push_str(&"=".repeat(filled));
    out.push_str(&"·".repeat(w.saturating_sub(filled)));
    out.push(']');
    out.push(' ');
    out.push_str(&format!("{percent:>3}%"));
    out
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

/// Build scrollable transcript lines + optional mouse/click targets per line.
fn transcript_lines_and_hits(
    state: &TuiSessionState,
    width: u16,
) -> (Vec<Line<'static>>, Vec<LineAnswerHit>) {
    let w = width.max(20) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut hits: Vec<LineAnswerHit> = Vec::new();

    if !state.pinned_notes.is_empty() {
        push_transcript_line(
            &mut lines,
            &mut hits,
            Line::from(vec![
                Span::styled(
                    " pinned ",
                    Style::default()
                        .fg(Color::Black)
                        .bg(theme::warn())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(
                        " {} item(s) · Ctrl+O list · Ctrl+K pin last",
                        state.pinned_notes.len()
                    ),
                    Style::default().fg(theme::muted()),
                ),
            ]),
            None,
        );
        for (idx, note) in state.pinned_notes.iter().enumerate().take(4) {
            let preview = truncate_chars(note.body.trim(), w.saturating_sub(24).max(24));
            push_transcript_line(
                &mut lines,
                &mut hits,
                Line::from(vec![
                    Span::styled(
                        format!("  [{}] ", idx + 1),
                        Style::default().fg(theme::warn()),
                    ),
                    Span::styled(
                        format!("{:<18}", truncate_chars(&note.title, 18)),
                        Style::default().fg(theme::assistant()),
                    ),
                    Span::styled(" · ", Style::default().fg(theme::muted())),
                    Span::styled(preview, Style::default().fg(theme::text())),
                    Span::styled(
                        "  copy",
                        Style::default()
                            .fg(theme::tool())
                            .add_modifier(Modifier::UNDERLINED),
                    ),
                ]),
                Some(LineClickHit::CopyText(note.body.clone())),
            );
        }
        if state.pinned_notes.len() > 4 {
            push_transcript_line(
                &mut lines,
                &mut hits,
                Line::from(Span::styled(
                    format!("  … +{} more pinned", state.pinned_notes.len() - 4),
                    Style::default().fg(theme::muted()),
                )),
                None,
            );
        }
        push_transcript_line(&mut lines, &mut hits, Line::default(), None);
    }

    for block in &state.blocks {
        match block {
            DisplayBlock::User(content) => {
                push_section_gap(&mut lines, &mut hits);
                let user_chip = format!(" {:<9} ", "user");
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(vec![Span::styled(
                        user_chip,
                        Style::default()
                            .fg(Color::Black)
                            .bg(theme::user())
                            .add_modifier(Modifier::BOLD),
                    )]),
                    None,
                );
                for text_line in wrap_text(content, w) {
                    push_transcript_line(
                        &mut lines,
                        &mut hits,
                        prefixed_line(
                            Span::styled("▏ ", Style::default().fg(theme::user())),
                            Line::from(Span::styled(text_line, Style::default().fg(theme::text()))),
                        ),
                        None,
                    );
                }
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
            }
            DisplayBlock::Assistant(content) => {
                push_section_gap(&mut lines, &mut hits);
                let assistant_chip = format!(" {:<9} ", "assistant");
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(vec![
                        Span::styled(
                            assistant_chip,
                            Style::default()
                                .fg(Color::Black)
                                .bg(theme::assistant())
                                .bold(),
                        ),
                        Span::styled(
                            " response ",
                            Style::default()
                                .fg(theme::assistant())
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    Some(LineClickHit::CopyText(content.clone())),
                );
                let (md_lines, md_hits) =
                    render_markdown_lines_with_hits(content, state.code_line_numbers);
                for (md_line, md_hit) in md_lines.into_iter().zip(md_hits) {
                    push_transcript_line(
                        &mut lines,
                        &mut hits,
                        prefixed_line(
                            Span::styled("▏ ", Style::default().fg(theme::assistant())),
                            md_line,
                        ),
                        md_hit,
                    );
                }
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
            }
            DisplayBlock::ToolRunning {
                name,
                input,
                call_id,
            } => {
                push_section_gap(&mut lines, &mut hits);
                let ui = tool_ui::metadata(name);
                let collapsed = state.is_tool_block_collapsed(call_id);
                let fold_indicator = if collapsed { "▸" } else { "▾" };
                let mut header_spans = vec![
                    Span::styled(
                        format!("{fold_indicator} "),
                        Style::default().fg(theme::muted()),
                    ),
                    Span::styled("◉ ", tool_dot_style(name)),
                    Span::styled(
                        ui.label,
                        Style::default()
                            .fg(theme::tool())
                            .add_modifier(Modifier::BOLD),
                    ),
                ];
                let detail_spans = tool_header_detail_spans(name, input);
                if !detail_spans.is_empty() {
                    header_spans.push(Span::raw(" · "));
                    header_spans.extend(detail_spans);
                }
                header_spans.push(Span::raw("  "));
                header_spans.push(tool_effect_badge(name));
                header_spans.push(Span::raw(" "));
                header_spans.push(tool_status_chip("RUNNING", theme::warn()));
                push_transcript_line(&mut lines, &mut hits, Line::from(header_spans), None);
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
            }
            DisplayBlock::ApprovalPending(req) => {
                // Full approval UI lives in a popup overlay; transcript only shows a
                // compact placeholder so the user has visual continuity in scrollback.
                push_section_gap(&mut lines, &mut hits);
                let ui = tool_ui::metadata(&req.tool);
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(vec![
                        Span::styled(
                            format!(" {} ", ui.icon),
                            Style::default().fg(Color::Black).bg(theme::warn()).bold(),
                        ),
                        Span::styled(" ", Style::default()),
                        Span::styled(
                            ui.label,
                            Style::default()
                                .fg(theme::warn())
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" ", Style::default()),
                        tool_effect_badge(&req.tool),
                        Span::styled(
                            "  awaiting approval — see popup",
                            Style::default().fg(theme::muted()),
                        ),
                    ]),
                    None,
                );
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
                let _ = w; // width still consumed in popup renderer
            }
            DisplayBlock::ApprovalResolved { tool, approved } => {
                push_section_gap(&mut lines, &mut hits);
                let ui = tool_ui::metadata(tool);
                let (label, style) = if *approved {
                    (
                        " approved ",
                        Style::default().fg(Color::Black).bg(theme::success()),
                    )
                } else {
                    (
                        " denied ",
                        Style::default().fg(Color::Black).bg(theme::error()),
                    )
                };
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(vec![
                        Span::styled(label, style.add_modifier(Modifier::BOLD)),
                        Span::styled(format!(" {tool}"), Style::default().fg(theme::text())),
                        Span::styled(
                            format!("  {}", ui.label),
                            Style::default().fg(theme::muted()),
                        ),
                    ]),
                    None,
                );
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
            }
            DisplayBlock::ToolDone {
                name,
                call_id,
                ok,
                detail,
            } => {
                push_section_gap(&mut lines, &mut hits);
                let ui = tool_ui::metadata(name);
                let collapsed = state.is_tool_block_collapsed(call_id);
                let fold_indicator = if collapsed { "▸" } else { "▾" };
                let status_chip = if *ok {
                    tool_status_chip("DONE", theme::success())
                } else {
                    tool_status_chip("FAILED", theme::error())
                };
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(vec![
                        Span::styled(
                            format!("{fold_indicator} "),
                            Style::default().fg(theme::muted()),
                        ),
                        Span::styled("◉ ", tool_dot_style(name)),
                        Span::styled(
                            ui.label,
                            Style::default()
                                .fg(theme::tool())
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw("  "),
                        tool_effect_badge(name),
                        Span::raw(" "),
                        status_chip,
                    ]),
                    None,
                );
                if !collapsed && !detail.trim().is_empty() {
                    push_tool_detail_lines(&mut lines, &mut hits, detail, w.saturating_sub(6), 80);
                }
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
            }
            DisplayBlock::System(s) => {
                // Multiline system blocks (e.g. startup logo) render as raw rows
                // without bullet prefixes so ASCII art alignment is preserved.
                if s.contains('\n') {
                    for part in s.split('\n') {
                        push_transcript_line(
                            &mut lines,
                            &mut hits,
                            Line::from(Span::styled(
                                part.to_string(),
                                Style::default().fg(theme::muted()),
                            )),
                            None,
                        );
                    }
                } else if s.is_empty() {
                    push_transcript_line(&mut lines, &mut hits, Line::default(), None);
                } else {
                    push_transcript_line(
                        &mut lines,
                        &mut hits,
                        Line::from(Span::styled(
                            format!("  • {s}"),
                            Style::default().fg(theme::muted()),
                        )),
                        None,
                    );
                }
            }
            DisplayBlock::Question(q) => {
                let selected_answer = state.answered_questions.get(&q.question_id);
                push_section_gap(&mut lines, &mut hits);
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(vec![
                        Span::styled(
                            " ? ",
                            Style::default().fg(Color::Black).bg(theme::warn()).bold(),
                        ),
                        Span::styled(
                            " question ",
                            Style::default()
                                .fg(theme::warn())
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]),
                    None,
                );
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
                for text_line in wrap_text(&q.prompt, w.saturating_sub(2)) {
                    push_transcript_line(
                        &mut lines,
                        &mut hits,
                        Line::from(vec![
                            Span::styled("  ", Style::default().fg(theme::muted())),
                            Span::styled(text_line, Style::default().fg(theme::text())),
                        ]),
                        None,
                    );
                }
                // When the modal is open, skip inline options — the popup handles selection.
                if !state.question_modal_open {
                    let suggested_selected =
                        matches!(selected_answer, Some(QuestionSelection::Suggested));
                    for (idx, row) in wrap_text(
                        &format!("suggested: {}", q.suggested_answer),
                        w.saturating_sub(12).max(20),
                    )
                    .into_iter()
                    .enumerate()
                    {
                        let row_style = if suggested_selected {
                            Style::default()
                                .fg(Color::Black)
                                .bg(theme::success())
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(theme::success())
                        };
                        let prefix_style = if suggested_selected {
                            row_style
                        } else {
                            Style::default()
                                .fg(theme::success())
                                .add_modifier(Modifier::BOLD)
                        };
                        let line = if idx == 0 {
                            Line::from(vec![
                                Span::styled("  [0] ", prefix_style),
                                Span::styled(row, row_style),
                                Span::styled(
                                    "  (click)",
                                    if suggested_selected {
                                        row_style
                                    } else {
                                        Style::default().fg(theme::muted())
                                    },
                                ),
                            ])
                        } else {
                            Line::from(vec![
                                Span::styled("      ", Style::default().fg(theme::muted())),
                                Span::styled(row, row_style),
                            ])
                        };
                        push_transcript_line(
                            &mut lines,
                            &mut hits,
                            line,
                            Some(LineClickHit::Question(QuestionSelection::Suggested)),
                        );
                    }
                    for (i, o) in q.options.iter().enumerate() {
                        let opt_selected = matches!(
                            selected_answer,
                            Some(QuestionSelection::Option { option_id }) if option_id == &o.id
                        );
                        let choice_text = format!("({}) {}", o.id, o.label);
                        for (idx, row) in wrap_text(&choice_text, w.saturating_sub(12).max(20))
                            .into_iter()
                            .enumerate()
                        {
                            let row_style = if opt_selected {
                                Style::default()
                                    .fg(Color::Black)
                                    .bg(theme::user())
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                Style::default().fg(theme::text())
                            };
                            let prefix_style = if opt_selected {
                                row_style
                            } else {
                                Style::default()
                                    .fg(theme::assistant())
                                    .add_modifier(Modifier::BOLD)
                            };
                            let line = if idx == 0 {
                                Line::from(vec![
                                    Span::styled(format!("  [{}] ", i + 1), prefix_style),
                                    Span::styled(row, row_style),
                                    Span::styled(
                                        "  (click)",
                                        if opt_selected {
                                            row_style
                                        } else {
                                            Style::default().fg(theme::muted())
                                        },
                                    ),
                                ])
                            } else {
                                Line::from(vec![
                                    Span::styled("      ", Style::default().fg(theme::muted())),
                                    Span::styled(row, row_style),
                                ])
                            };
                            push_transcript_line(
                                &mut lines,
                                &mut hits,
                                line,
                                Some(LineClickHit::Question(QuestionSelection::Option {
                                    option_id: o.id.clone(),
                                })),
                            );
                        }
                    }
                    if q.allow_custom {
                        let custom_selected =
                            matches!(selected_answer, Some(QuestionSelection::Custom { .. }));
                        push_transcript_line(
                            &mut lines,
                            &mut hits,
                            Line::from(Span::styled(
                                "  [c] type your own answer below, then Enter",
                                if custom_selected {
                                    Style::default()
                                        .fg(Color::Black)
                                        .bg(theme::assistant())
                                        .add_modifier(Modifier::BOLD)
                                } else {
                                    Style::default().fg(theme::muted())
                                },
                            )),
                            None,
                        );
                        if let Some(QuestionSelection::Custom { text }) = selected_answer {
                            push_transcript_line(
                                &mut lines,
                                &mut hits,
                                Line::from(Span::styled(
                                    format!("      selected: {}", truncate_chars(text, 80)),
                                    Style::default().fg(theme::assistant()),
                                )),
                                None,
                            );
                        }
                    }
                    push_transcript_line(
                        &mut lines,
                        &mut hits,
                        Line::from(Span::styled(
                            "  Tip: /auto-answer or Enter on empty = suggested · click an option above",
                            Style::default().fg(theme::muted()),
                        )),
                        None,
                    );
                }
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
            }
            DisplayBlock::Thinking(content) => {
                push_section_gap(&mut lines, &mut hits);
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(vec![
                        Span::styled(
                            " ✦ thinking ",
                            Style::default()
                                .fg(Color::Black)
                                .bg(theme::muted())
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled("", Style::default()),
                    ]),
                    None,
                );
                let wrapped = wrap_text(content, w.saturating_sub(2));
                let shown = wrapped.len().min(10);
                for text_line in wrapped.iter().take(shown) {
                    push_transcript_line(
                        &mut lines,
                        &mut hits,
                        Line::from(vec![
                            Span::styled(" │ ", Style::default().fg(theme::muted())),
                            Span::styled(
                                text_line.clone(),
                                Style::default()
                                    .fg(theme::muted())
                                    .add_modifier(Modifier::ITALIC),
                            ),
                        ]),
                        None,
                    );
                }
                if wrapped.len() > shown {
                    push_transcript_line(
                        &mut lines,
                        &mut hits,
                        Line::from(Span::styled(
                            format!(" … +{} more thinking lines", wrapped.len() - shown),
                            Style::default().fg(theme::muted()),
                        )),
                        None,
                    );
                }
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
            }
            DisplayBlock::ErrorLine(s) => {
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(Span::styled(
                        format!(" ✗ {s}"),
                        Style::default().fg(theme::error()),
                    )),
                    None,
                );
            }
        }
    }

    if let Some(thinking) = &state.streaming_thinking
        && !thinking.is_empty()
    {
        push_transcript_line(
            &mut lines,
            &mut hits,
            Line::from(vec![Span::styled(
                " ✦ thinking… ",
                Style::default()
                    .fg(Color::Black)
                    .bg(theme::muted())
                    .add_modifier(Modifier::BOLD),
            )]),
            None,
        );
        let wrapped = wrap_text(thinking, w.saturating_sub(2));
        let shown = wrapped.len().min(10);
        for text_line in wrapped.iter().take(shown) {
            push_transcript_line(
                &mut lines,
                &mut hits,
                Line::from(vec![
                    Span::styled(" │ ", Style::default().fg(theme::muted())),
                    Span::styled(
                        text_line.clone(),
                        Style::default()
                            .fg(theme::muted())
                            .add_modifier(Modifier::ITALIC),
                    ),
                ]),
                None,
            );
        }
        if wrapped.len() > shown {
            push_transcript_line(
                &mut lines,
                &mut hits,
                Line::from(Span::styled(
                    format!(" … +{} more thinking lines", wrapped.len() - shown),
                    Style::default().fg(theme::muted()),
                )),
                None,
            );
        }
        push_transcript_line(&mut lines, &mut hits, Line::default(), None);
    }

    if let Some(stream) = &state.streaming_assistant
        && !stream.is_empty()
    {
        push_transcript_line(
            &mut lines,
            &mut hits,
            Line::from(vec![
                Span::styled(
                    " dcode-ai ",
                    Style::default().fg(Color::Black).bg(theme::assistant()),
                ),
                Span::styled(" streaming ", Style::default().fg(theme::muted())),
            ]),
            Some(LineClickHit::CopyText(stream.clone())),
        );
        push_transcript_line(&mut lines, &mut hits, Line::default(), None);
        let (md_lines, md_hits) = render_markdown_lines_with_hits(stream, state.code_line_numbers);
        for (md_line, md_hit) in md_lines.into_iter().zip(md_hits) {
            push_transcript_line(
                &mut lines,
                &mut hits,
                prefixed_line(
                    Span::styled("▏ ", Style::default().fg(theme::assistant())),
                    md_line,
                ),
                md_hit,
            );
        }
    }

    if lines.is_empty() {
        push_transcript_line(
            &mut lines,
            &mut hits,
            Line::from(vec![
                Span::styled(
                    "dcode-ai",
                    Style::default()
                        .fg(theme::assistant())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(" — session ready", Style::default().fg(theme::muted())),
            ]),
            None,
        );
        push_transcript_line(&mut lines, &mut hits, Line::default(), None);
        push_transcript_line(
            &mut lines,
            &mut hits,
            Line::from(Span::styled(
                "Tab agent · Shift+Enter/Ctrl+I/Ctrl+J newline · Ctrl+P palette · /keymaps · Ctrl+V image",
                Style::default().fg(theme::muted()),
            )),
            None,
        );
    }

    (lines, hits)
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!(
            "{}…",
            s.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}

fn char_window(s: &str, start: usize, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    s.chars().skip(start).take(width).collect()
}

fn tool_effect_badge(name: &str) -> Span<'static> {
    use crate::tui::tool_classify::ToolEffect;
    let effect = crate::tui::tool_classify::classify_tool(name);
    let display = effect.display();
    let style = match effect {
        ToolEffect::ReadOnly => Style::default().fg(theme::success()),
        ToolEffect::RemoteAction => Style::default().fg(theme::assistant()),
        ToolEffect::LocalMutation => Style::default().fg(theme::warn()),
        ToolEffect::Destructive => Style::default().fg(theme::error()),
    }
    .add_modifier(Modifier::BOLD);
    Span::styled(format!("[{} {}]", display.badge, display.label), style)
}

fn tool_dot_style(name: &str) -> Style {
    use crate::tui::tool_classify::ToolEffect;
    match crate::tui::tool_classify::classify_tool(name) {
        ToolEffect::ReadOnly => Style::default().fg(theme::muted()),
        ToolEffect::RemoteAction => Style::default().fg(theme::assistant()),
        ToolEffect::LocalMutation => Style::default().fg(theme::warn()),
        ToolEffect::Destructive => Style::default().fg(theme::error()),
    }
}

fn tool_status_chip(label: &str, color: Color) -> Span<'static> {
    Span::styled(
        format!("[{label}]"),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )
}

fn tool_header_detail_spans(name: &str, input: &str) -> Vec<Span<'static>> {
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
fn tool_input_preview(name: &str, input: &str) -> String {
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

fn wrap_text(s: &str, width: usize) -> Vec<String> {
    if width < 8 {
        return vec![s.to_string()];
    }
    let mut out = Vec::new();
    for paragraph in s.split('\n') {
        if paragraph.is_empty() {
            out.push(String::new());
            continue;
        }
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let word_chars = word.chars().count();
            if line.is_empty() && word_chars > width {
                for chunk in wrap_preformatted_line(word, width) {
                    out.push(chunk);
                }
                continue;
            } else if line.is_empty() {
                line = word.to_string();
            } else if line.chars().count() + 1 + word_chars <= width {
                line.push(' ');
                line.push_str(word);
            } else if word_chars > width {
                out.push(std::mem::take(&mut line));
                let chunks = wrap_preformatted_line(word, width);
                let chunk_len = chunks.len();
                for (idx, chunk) in chunks.into_iter().enumerate() {
                    if idx + 1 == chunk_len {
                        line = chunk;
                        break;
                    }
                    out.push(chunk);
                }
            } else {
                out.push(std::mem::take(&mut line));
                line = word.to_string();
            }
        }
        if !line.is_empty() {
            out.push(line);
        }
    }
    if out.is_empty() && !s.is_empty() {
        out.push(s.to_string());
    }
    out
}

fn wrap_preformatted_line(line: &str, width: usize) -> Vec<String> {
    if width < 4 || line.is_empty() {
        return vec![line.to_string()];
    }
    let mut out = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;
    for ch in line.chars() {
        if current_len >= width {
            out.push(current);
            current = String::new();
            current_len = 0;
        }
        current.push(ch);
        current_len += 1;
    }
    if out.is_empty() || !current.is_empty() {
        out.push(current);
    }
    out
}

fn push_wrapped_plain_lines_limited(
    lines: &mut Vec<Line<'static>>,
    hits: &mut Vec<LineAnswerHit>,
    text: &str,
    width: usize,
    style: Style,
    max_lines: usize,
) {
    let mut rendered = 0usize;
    let mut omitted = 0usize;
    for source_line in text.lines() {
        let wrapped = wrap_preformatted_line(source_line, width);
        for line in wrapped {
            if rendered < max_lines {
                push_transcript_line(lines, hits, Line::from(Span::styled(line, style)), None);
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
                format!(" … +{omitted} lines (truncated)"),
                Style::default().fg(theme::muted()),
            )),
            None,
        );
    }
}

fn push_tool_detail_lines(
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
fn render_approval_popup(frame: &mut ratatui::Frame<'_>, area: Rect, req: &ApprovalRequest) {
    let popup_w = area.width.saturating_mul(3) / 4;
    let popup_w = popup_w.clamp(50, area.width.saturating_sub(2).max(50));
    let popup_h =
        (area.height.saturating_mul(3) / 4).clamp(14, area.height.saturating_sub(2).max(14));
    let popup_area = centered_rect(area, popup_w, popup_h);

    let inner_w = popup_area.width.saturating_sub(4) as usize;
    let ui = tool_ui::metadata(&req.tool);
    let preview = tool_input_preview(&req.tool, &req.input);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            format!(" {} ", ui.icon),
            Style::default().fg(Color::Black).bg(theme::warn()).bold(),
        ),
        Span::styled("  ", Style::default()),
        Span::styled(
            ui.label,
            Style::default()
                .fg(theme::warn())
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("  {}", ui.family),
            Style::default().fg(theme::muted()),
        ),
    ]));
    lines.push(Line::default());
    if !preview.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("Input  ", Style::default().fg(theme::muted())),
            Span::styled(
                truncate_chars(&preview, inner_w.saturating_sub(8).max(20)),
                Style::default().fg(theme::text()),
            ),
        ]));
        lines.push(Line::default());
    }
    for text_line in wrap_text(&req.description, inner_w) {
        lines.push(Line::from(Span::styled(
            text_line,
            Style::default().fg(theme::text()),
        )));
    }
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        " Full Input ",
        Style::default()
            .fg(theme::muted())
            .add_modifier(Modifier::BOLD),
    )));
    let mut full_lines: Vec<Line<'static>> = Vec::new();
    let mut full_hits: Vec<LineAnswerHit> = Vec::new();
    push_wrapped_plain_lines_limited(
        &mut full_lines,
        &mut full_hits,
        &req.input,
        inner_w,
        Style::default().fg(theme::muted()),
        12,
    );
    lines.extend(full_lines);
    lines.push(Line::default());
    lines.push(Line::from(Span::styled(
        " Decision ",
        Style::default()
            .fg(theme::warn())
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        " [y] Ctrl+Y approve   [n] Ctrl+N deny   [u] Ctrl+U always allow pattern",
        Style::default().fg(theme::text()),
    )));
    lines.push(Line::from(Span::styled(
        " /approve or /deny in input · Esc keeps request pending",
        Style::default().fg(theme::muted()),
    )));

    frame.render_widget(ClearWidget, popup_area);
    let popup = Paragraph::new(Text::from(lines))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::warn()))
                .title(Span::styled(
                    " Tool approval needed ",
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
fn parse_approval_verdict(line: &str) -> Option<bool> {
    let mut s = line.trim().to_lowercase();
    while matches!(
        s.chars().last(),
        Some('.' | '!' | '?' | ',' | ';' | ':' | '"' | '\'')
    ) {
        s.pop();
    }
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // Slash commands (handled before this in caller for passthrough; bare forms here too)
    match s {
        "/approve" | "/y" | "/yes" | "/ok" => return Some(true),
        "/deny" | "/n" | "/no" => return Some(false),
        _ => {}
    }
    let word = s.split_whitespace().next()?;
    match word {
        "y" | "yes" | "ok" | "okay" | "approve" | "approved" | "allow" | "1" | "true" => Some(true),
        "n" | "no" | "deny" | "denied" | "reject" | "rejected" | "decline" | "declined" | "0"
        | "false" => Some(false),
        _ => None,
    }
}

#[derive(Debug, Clone)]
enum MdOpenTag {
    Strong,
    Emphasis,
    Strike,
    Link,
    Heading,
    BlockQuote,
    Paragraph,
    List,
    Item,
    CodeBlock,
    Other,
}

#[derive(Debug, Clone)]
enum MdListState {
    Unordered,
    Ordered(u64),
}

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static THEME_SET: OnceLock<ThemeSet> = OnceLock::new();

fn syntect_theme() -> &'static Theme {
    let ts = THEME_SET.get_or_init(ThemeSet::load_defaults);
    ts.themes
        .get("base16-ocean.dark")
        .or_else(|| ts.themes.values().next())
        .expect("syntect theme set must not be empty")
}

fn syntect_style_to_ratatui(style: syntect::highlighting::Style) -> Style {
    let mut out = Style::default().fg(Color::Rgb(
        style.foreground.r,
        style.foreground.g,
        style.foreground.b,
    ));
    if style.font_style.contains(FontStyle::BOLD) {
        out = out.add_modifier(Modifier::BOLD);
    }
    if style.font_style.contains(FontStyle::ITALIC) {
        out = out.add_modifier(Modifier::ITALIC);
    }
    if style.font_style.contains(FontStyle::UNDERLINE) {
        out = out.add_modifier(Modifier::UNDERLINED);
    }
    out
}

fn render_code_block_lines(
    out: &mut Vec<Line<'static>>,
    hits: &mut Vec<LineAnswerHit>,
    language: Option<String>,
    code: &str,
    code_line_numbers: bool,
) {
    let ps = SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines);
    let is_diff = language
        .as_deref()
        .map(|l| matches!(l.trim().to_ascii_lowercase().as_str(), "diff" | "patch"))
        .unwrap_or(false);
    let syntax = language
        .as_deref()
        .and_then(|lang| ps.find_syntax_by_token(lang))
        .unwrap_or_else(|| ps.find_syntax_plain_text());
    let mut highlighter = HighlightLines::new(syntax, syntect_theme());
    let line_count = code.split('\n').count().max(1);
    let line_num_width = line_count.to_string().len();
    let copy_payload = code.to_string();

    for (idx, raw) in code.split('\n').enumerate() {
        let highlights = if is_diff {
            Vec::new()
        } else {
            highlighter.highlight_line(raw, ps).unwrap_or_default()
        };
        let mut spans: Vec<Span<'static>> = Vec::new();
        if code_line_numbers {
            spans.push(Span::styled(
                format!("{:>width$} │ ", idx + 1, width = line_num_width),
                Style::default().fg(theme::muted()),
            ));
        }
        if is_diff {
            let (lane, style) = if raw.starts_with("+++")
                || raw.starts_with("---")
                || raw.starts_with("@@")
                || raw.starts_with("diff ")
                || raw.starts_with("index ")
            {
                ("▌", Style::default().fg(theme::warn()).bg(theme::surface()))
            } else if raw.starts_with('+') {
                (
                    "▌",
                    Style::default().fg(theme::success()).bg(theme::surface()),
                )
            } else if raw.starts_with('-') {
                (
                    "▌",
                    Style::default().fg(theme::error()).bg(theme::surface()),
                )
            } else {
                (
                    "▌",
                    Style::default().fg(theme::muted()).bg(theme::surface()),
                )
            };
            spans.push(Span::styled(format!("{lane} "), style));
            spans.push(Span::styled(raw.to_string(), style));
        } else if highlights.is_empty() {
            spans.push(Span::styled(
                raw.to_string(),
                Style::default().fg(theme::tool()).bg(theme::surface()),
            ));
        } else {
            spans.extend(highlights.into_iter().map(|(style, text)| {
                Span::styled(
                    text.to_string(),
                    syntect_style_to_ratatui(style).bg(theme::surface()),
                )
            }));
        }
        out.push(Line::from(spans));
        hits.push(Some(LineClickHit::CopyText(copy_payload.clone())));
    }
}

fn flush_md_render_line(
    out: &mut Vec<Line<'static>>,
    hits: &mut Vec<LineAnswerHit>,
    current: &mut Vec<Span<'static>>,
) {
    if current.is_empty() {
        out.push(Line::default());
    } else {
        out.push(Line::from(std::mem::take(current)));
    }
    hits.push(None);
}

fn md_current_style(open: &[MdOpenTag], quote_depth: usize, link_url: Option<&str>) -> Style {
    let mut style = Style::default().fg(theme::text());
    if quote_depth > 0 {
        style = style.fg(theme::muted()).add_modifier(Modifier::ITALIC);
    }
    if open.iter().any(|t| matches!(t, MdOpenTag::Heading)) {
        style = style.fg(theme::assistant()).add_modifier(Modifier::BOLD);
    }
    if open.iter().any(|t| matches!(t, MdOpenTag::Strong)) {
        style = style.add_modifier(Modifier::BOLD);
    }
    if open.iter().any(|t| matches!(t, MdOpenTag::Emphasis)) {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if open.iter().any(|t| matches!(t, MdOpenTag::Strike)) {
        style = style.add_modifier(Modifier::CROSSED_OUT);
    }
    if link_url.is_some() {
        style = style
            .fg(theme::assistant())
            .add_modifier(Modifier::UNDERLINED);
    }
    style
}

#[derive(Debug, Default)]
struct MdTableState {
    alignments: Vec<Alignment>,
    in_head: bool,
    current_row: Vec<String>,
    current_cell: String,
    header_rows: Vec<Vec<String>>,
    body_rows: Vec<Vec<String>>,
}

fn align_table_cell(text: &str, width: usize, alignment: Alignment) -> String {
    let normalized = if text.chars().count() > width {
        truncate_chars(text, width)
    } else {
        text.to_string()
    };
    let cell_width = normalized.chars().count();
    if cell_width >= width {
        return normalized;
    }
    let pad = width - cell_width;
    match alignment {
        Alignment::Right => format!("{}{}", " ".repeat(pad), normalized),
        Alignment::Center => {
            let left = pad / 2;
            let right = pad - left;
            format!("{}{}{}", " ".repeat(left), normalized, " ".repeat(right))
        }
        _ => format!("{}{}", normalized, " ".repeat(pad)),
    }
}

fn render_table_lines(
    out: &mut Vec<Line<'static>>,
    hits: &mut Vec<LineAnswerHit>,
    table: &MdTableState,
) {
    let col_count = table
        .header_rows
        .iter()
        .chain(table.body_rows.iter())
        .map(|r| r.len())
        .max()
        .unwrap_or(0);
    if col_count == 0 {
        return;
    }
    const MAX_TABLE_COL_WIDTH: usize = 24;
    let render_row = |row: &[String], is_header: bool| -> Line<'static> {
        let mut spans = Vec::new();
        spans.push(Span::styled(
            if is_header { " table " } else { " row   " },
            Style::default()
                .fg(if is_header {
                    theme::assistant()
                } else {
                    theme::muted()
                })
                .add_modifier(Modifier::BOLD),
        ));
        for i in 0..col_count {
            let val = row.get(i).map(String::as_str).unwrap_or("");
            let aligned = align_table_cell(
                val,
                MAX_TABLE_COL_WIDTH,
                table.alignments.get(i).copied().unwrap_or(Alignment::None),
            );
            let style = if is_header {
                Style::default()
                    .fg(theme::assistant())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::text())
            };
            spans.push(Span::styled(aligned, style));
            if i + 1 != col_count {
                spans.push(Span::styled("  ·  ", Style::default().fg(theme::muted())));
            }
        }
        Line::from(spans)
    };

    for row in &table.header_rows {
        out.push(render_row(row, true));
        hits.push(None);
    }
    if !table.header_rows.is_empty() {
        out.push(Line::from(Span::styled(
            "────────────────────────────────────────",
            Style::default().fg(theme::muted()),
        )));
        hits.push(None);
    }
    for row in &table.body_rows {
        out.push(render_row(row, false));
        hits.push(None);
    }
}

fn render_markdown_lines_with_hits(
    markdown: &str,
    code_line_numbers: bool,
) -> (Vec<Line<'static>>, Vec<LineAnswerHit>) {
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut hits: Vec<LineAnswerHit> = Vec::new();
    let mut current: Vec<Span<'static>> = Vec::new();
    let mut open: Vec<MdOpenTag> = Vec::new();
    let mut list_stack: Vec<MdListState> = Vec::new();
    let mut quote_depth = 0usize;
    let mut pending_item_prefix: Option<String> = None;
    let mut active_link_url: Option<String> = None;
    let mut code_language: Option<String> = None;
    let mut code_buf = String::new();
    let mut table_state: Option<MdTableState> = None;

    let parser = Parser::new_ext(markdown, Options::all());

    for ev in parser {
        if let Some(table) = table_state.as_mut() {
            match ev {
                MdEvent::Start(Tag::TableHead) => table.in_head = true,
                MdEvent::Start(Tag::TableRow) => table.current_row.clear(),
                MdEvent::Start(Tag::TableCell) => table.current_cell.clear(),
                MdEvent::Text(t)
                | MdEvent::Code(t)
                | MdEvent::InlineMath(t)
                | MdEvent::DisplayMath(t) => table.current_cell.push_str(&t),
                MdEvent::SoftBreak | MdEvent::HardBreak => table.current_cell.push(' '),
                MdEvent::End(TagEnd::TableCell) => {
                    table
                        .current_row
                        .push(table.current_cell.trim().to_string());
                    table.current_cell.clear();
                }
                MdEvent::End(TagEnd::TableRow) => {
                    if table.in_head {
                        table.header_rows.push(table.current_row.clone());
                    } else {
                        table.body_rows.push(table.current_row.clone());
                    }
                    table.current_row.clear();
                }
                MdEvent::End(TagEnd::TableHead) => table.in_head = false,
                MdEvent::End(TagEnd::Table) => {
                    if !current.is_empty() {
                        flush_md_render_line(&mut out, &mut hits, &mut current);
                    }
                    if let Some(done) = table_state.take() {
                        render_table_lines(&mut out, &mut hits, &done);
                    }
                    out.push(Line::default());
                    hits.push(None);
                }
                _ => {}
            }
            continue;
        }

        if code_language.is_some() {
            match ev {
                MdEvent::Text(t)
                | MdEvent::Code(t)
                | MdEvent::InlineMath(t)
                | MdEvent::DisplayMath(t) => {
                    code_buf.push_str(&t);
                }
                MdEvent::SoftBreak | MdEvent::HardBreak => code_buf.push('\n'),
                MdEvent::End(_) => {
                    if matches!(open.pop(), Some(MdOpenTag::CodeBlock)) {
                        if !current.is_empty() {
                            flush_md_render_line(&mut out, &mut hits, &mut current);
                        }
                        render_code_block_lines(
                            &mut out,
                            &mut hits,
                            code_language.take(),
                            &code_buf,
                            code_line_numbers,
                        );
                        code_buf.clear();
                        out.push(Line::default());
                        hits.push(None);
                    }
                }
                _ => {}
            }
            continue;
        }

        match ev {
            MdEvent::Start(tag) => match tag {
                Tag::Strong => open.push(MdOpenTag::Strong),
                Tag::Emphasis => open.push(MdOpenTag::Emphasis),
                Tag::Strikethrough => open.push(MdOpenTag::Strike),
                Tag::Link { dest_url, .. } => {
                    active_link_url = Some(dest_url.to_string());
                    open.push(MdOpenTag::Link);
                }
                Tag::Paragraph => open.push(MdOpenTag::Paragraph),
                Tag::Heading { level, .. } => {
                    if !current.is_empty() {
                        flush_md_render_line(&mut out, &mut hits, &mut current);
                    }
                    let _ = level;
                    open.push(MdOpenTag::Heading);
                }
                Tag::BlockQuote(_) => {
                    quote_depth += 1;
                    open.push(MdOpenTag::BlockQuote);
                }
                Tag::List(start) => {
                    list_stack.push(match start {
                        Some(n) => MdListState::Ordered(n),
                        None => MdListState::Unordered,
                    });
                    open.push(MdOpenTag::List);
                }
                Tag::Item => {
                    if !current.is_empty() {
                        flush_md_render_line(&mut out, &mut hits, &mut current);
                    }
                    pending_item_prefix = Some(match list_stack.last_mut() {
                        Some(MdListState::Ordered(next)) => {
                            let label = format!("{}. ", *next);
                            *next += 1;
                            label
                        }
                        _ => "• ".to_string(),
                    });
                    open.push(MdOpenTag::Item);
                }
                Tag::CodeBlock(kind) => {
                    if !current.is_empty() {
                        flush_md_render_line(&mut out, &mut hits, &mut current);
                    }
                    code_language = Some(match kind {
                        CodeBlockKind::Indented => String::new(),
                        CodeBlockKind::Fenced(info) => info
                            .split_whitespace()
                            .next()
                            .unwrap_or_default()
                            .to_string(),
                    });
                    open.push(MdOpenTag::CodeBlock);
                }
                Tag::Table(alignments) => {
                    if !current.is_empty() {
                        flush_md_render_line(&mut out, &mut hits, &mut current);
                    }
                    table_state = Some(MdTableState {
                        alignments,
                        ..Default::default()
                    });
                }
                _ => open.push(MdOpenTag::Other),
            },
            MdEvent::End(_) => {
                let ended = open.pop();
                match ended {
                    Some(MdOpenTag::Paragraph) => {
                        if !current.is_empty() {
                            flush_md_render_line(&mut out, &mut hits, &mut current);
                        }
                        // Add extra vertical rhythm between top-level paragraphs.
                        if list_stack.is_empty() && quote_depth == 0 {
                            push_markdown_blank_line_if_needed(&mut out, &mut hits);
                        }
                    }
                    Some(MdOpenTag::Heading) | Some(MdOpenTag::Item) => {
                        if !current.is_empty() {
                            flush_md_render_line(&mut out, &mut hits, &mut current);
                        }
                    }
                    Some(MdOpenTag::BlockQuote) => {
                        quote_depth = quote_depth.saturating_sub(1);
                        if !current.is_empty() {
                            flush_md_render_line(&mut out, &mut hits, &mut current);
                        }
                    }
                    Some(MdOpenTag::Link) => {
                        active_link_url = None;
                    }
                    _ => {}
                }
            }
            MdEvent::Text(t) => {
                let style = md_current_style(&open, quote_depth, active_link_url.as_deref());
                let text = t.to_string();
                for (idx, seg) in text.split('\n').enumerate() {
                    if idx > 0 {
                        flush_md_render_line(&mut out, &mut hits, &mut current);
                    }
                    if current.is_empty() {
                        if quote_depth > 0 {
                            current.push(Span::styled(
                                "▎ ".repeat(quote_depth),
                                Style::default().fg(theme::warn()),
                            ));
                        }
                        if let Some(prefix) = pending_item_prefix.take() {
                            current.push(Span::styled(
                                prefix,
                                Style::default().fg(theme::assistant()),
                            ));
                        }
                    }
                    if !seg.is_empty() {
                        current.push(Span::styled(seg.to_string(), style));
                    }
                }
            }
            MdEvent::Code(code) => {
                let style = Style::default().fg(theme::tool()).bg(theme::surface());
                current.push(Span::styled(code.to_string(), style));
            }
            MdEvent::SoftBreak | MdEvent::HardBreak => {
                flush_md_render_line(&mut out, &mut hits, &mut current);
            }
            MdEvent::Rule => {
                if !current.is_empty() {
                    flush_md_render_line(&mut out, &mut hits, &mut current);
                }
                out.push(Line::from(Span::styled(
                    "────────────────",
                    Style::default().fg(theme::muted()),
                )));
                hits.push(None);
            }
            MdEvent::Html(html) | MdEvent::InlineHtml(html) => {
                current.push(Span::styled(
                    html.to_string(),
                    Style::default().fg(theme::muted()),
                ));
            }
            MdEvent::FootnoteReference(name) => {
                current.push(Span::styled(
                    format!("[^{}]", name),
                    Style::default().fg(theme::muted()),
                ));
            }
            MdEvent::TaskListMarker(done) => {
                let marker = if done { "[x] " } else { "[ ] " };
                current.push(Span::styled(
                    marker,
                    Style::default().fg(theme::assistant()),
                ));
            }
            _ => {}
        }
    }

    if !current.is_empty() {
        flush_md_render_line(&mut out, &mut hits, &mut current);
    }

    if out.is_empty() {
        (vec![Line::default()], vec![None])
    } else {
        (out, hits)
    }
}

#[cfg(test)]
fn render_markdown_lines(markdown: &str) -> Vec<Line<'static>> {
    render_markdown_lines_with_hits(markdown, false).0
}

fn push_markdown_blank_line_if_needed(out: &mut Vec<Line<'static>>, hits: &mut Vec<LineAnswerHit>) {
    if out.last().is_some_and(|line| line.spans.is_empty()) {
        return;
    }
    out.push(Line::default());
    hits.push(None);
}

#[allow(dead_code)]
fn flush_md_plain(spans: &mut Vec<Span<'static>>, plain: &mut String, base: Style) {
    if !plain.is_empty() {
        spans.push(Span::styled(std::mem::take(plain), base));
    }
}

#[allow(dead_code)]
fn parse_md_inline(text: &str, base: Style) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut plain = String::new();
    let mut i = 0usize;

    while i < text.len() {
        let rest = &text[i..];

        if let Some(inner) = rest.strip_prefix("**")
            && let Some(end) = inner.find("**")
        {
            flush_md_plain(&mut spans, &mut plain, base);
            spans.push(Span::styled(
                inner[..end].to_string(),
                base.add_modifier(Modifier::BOLD),
            ));
            i += 2 + end + 2;
            continue;
        }

        if let Some(inner) = rest.strip_prefix("~~")
            && let Some(end) = inner.find("~~")
        {
            flush_md_plain(&mut spans, &mut plain, base);
            spans.push(Span::styled(
                inner[..end].to_string(),
                base.add_modifier(Modifier::CROSSED_OUT),
            ));
            i += 2 + end + 2;
            continue;
        }

        if let Some(inner) = rest.strip_prefix('`')
            && let Some(end) = inner.find('`')
        {
            flush_md_plain(&mut spans, &mut plain, base);
            spans.push(Span::styled(
                inner[..end].to_string(),
                base.fg(theme::tool()).bg(theme::surface()),
            ));
            i += 1 + end + 1;
            continue;
        }

        if let Some(inner) = rest.strip_prefix('*')
            && !rest.starts_with("**")
            && let Some(end) = inner.find('*')
        {
            flush_md_plain(&mut spans, &mut plain, base);
            spans.push(Span::styled(
                inner[..end].to_string(),
                base.add_modifier(Modifier::ITALIC),
            ));
            i += 1 + end + 1;
            continue;
        }

        if let Some(inner) = rest.strip_prefix('_')
            && !rest.starts_with("__")
            && let Some(end) = inner.find('_')
        {
            flush_md_plain(&mut spans, &mut plain, base);
            spans.push(Span::styled(
                inner[..end].to_string(),
                base.add_modifier(Modifier::ITALIC),
            ));
            i += 1 + end + 1;
            continue;
        }

        if let Some(inner) = rest.strip_prefix('[')
            && let Some(label_end) = inner.find("](")
        {
            let after = &inner[label_end + 2..];
            if let Some(url_end) = after.find(')') {
                flush_md_plain(&mut spans, &mut plain, base);
                let label = &inner[..label_end];
                let url = &after[..url_end];
                spans.push(Span::styled(
                    label.to_string(),
                    base.fg(theme::assistant())
                        .add_modifier(Modifier::UNDERLINED),
                ));
                if !url.is_empty() {
                    spans.push(Span::styled(
                        format!(" ({url})"),
                        base.fg(theme::muted()).add_modifier(Modifier::ITALIC),
                    ));
                }
                i += 1 + label_end + 2 + url_end + 1;
                continue;
            }
        }

        let mut iter = rest.char_indices();
        if let Some((_, ch)) = iter.next() {
            plain.push(ch);
            i += ch.len_utf8();
        } else {
            break;
        }
    }

    flush_md_plain(&mut spans, &mut plain, base);
    if spans.is_empty() {
        spans.push(Span::styled(String::new(), base));
    }
    spans
}

#[allow(dead_code)]
fn split_markdown_heading(text: &str) -> Option<(usize, &str)> {
    let level = text.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&level) && text.chars().nth(level) == Some(' ') {
        Some((level, &text[level + 1..]))
    } else {
        None
    }
}

#[allow(dead_code)]
fn parse_md_line(line: &str) -> Line<'static> {
    let base = Style::default().fg(theme::text());
    let trimmed = line.trim_start();
    let leading = &line[..line.len().saturating_sub(trimmed.len())];

    if trimmed.starts_with("```") {
        return Line::from(Span::styled(
            line.to_string(),
            Style::default().fg(theme::muted()),
        ));
    }

    if trimmed.len() >= 3 && trimmed.chars().all(|c| c == '-') {
        return Line::from(Span::styled(
            "─".repeat(trimmed.len()),
            Style::default().fg(theme::muted()),
        ));
    }

    if let Some((_heading_level, text)) = split_markdown_heading(trimmed) {
        let mut spans = Vec::new();
        if !leading.is_empty() {
            spans.push(Span::styled(leading.to_string(), base));
        }
        spans.extend(parse_md_inline(
            text,
            Style::default()
                .fg(theme::assistant())
                .add_modifier(Modifier::BOLD),
        ));
        return Line::from(spans);
    }

    if let Some(text) = trimmed.strip_prefix("> ") {
        let mut spans = Vec::new();
        if !leading.is_empty() {
            spans.push(Span::styled(leading.to_string(), base));
        }
        spans.push(Span::styled("▎ ", Style::default().fg(theme::warn())));
        let quoted = if let Some((_level, content)) = split_markdown_heading(text) {
            content
        } else {
            text
        };
        spans.extend(parse_md_inline(
            quoted,
            Style::default()
                .fg(theme::muted())
                .add_modifier(Modifier::ITALIC),
        ));
        return Line::from(spans);
    }

    if let Some(text) = trimmed
        .strip_prefix("- ")
        .or_else(|| trimmed.strip_prefix("* "))
        .or_else(|| trimmed.strip_prefix("+ "))
    {
        let mut spans = Vec::new();
        if !leading.is_empty() {
            spans.push(Span::styled(leading.to_string(), base));
        }
        spans.push(Span::styled("• ", Style::default().fg(theme::assistant())));
        spans.extend(parse_md_inline(text, base));
        return Line::from(spans);
    }

    if let Some(dot) = trimmed.find(". ")
        && !trimmed[..dot].is_empty()
        && trimmed[..dot].chars().all(|c| c.is_ascii_digit())
    {
        let mut spans = Vec::new();
        if !leading.is_empty() {
            spans.push(Span::styled(leading.to_string(), base));
        }
        spans.push(Span::styled(
            format!("{}. ", &trimmed[..dot]),
            Style::default().fg(theme::assistant()),
        ));
        spans.extend(parse_md_inline(&trimmed[dot + 2..], base));
        return Line::from(spans);
    }

    Line::from(parse_md_inline(line, base))
}

fn parse_tui_question_answer(
    raw: &str,
    q: &dcode_ai_common::event::InteractiveQuestionPayload,
) -> Option<QuestionSelection> {
    let t = raw.trim();
    if t.is_empty() || t == "0" || t.eq_ignore_ascii_case("s") {
        return Some(QuestionSelection::Suggested);
    }
    if let Ok(n) = t.parse::<usize>()
        && n >= 1
        && n <= q.options.len()
    {
        return Some(QuestionSelection::Option {
            option_id: q.options[n - 1].id.clone(),
        });
    }
    if q.allow_custom && !t.is_empty() {
        return Some(QuestionSelection::Custom {
            text: t.to_string(),
        });
    }
    None
}

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
                        "docs/research/",
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
                let tokens_total = g.input_tokens.saturating_add(g.output_tokens);
                let context_pct = ((tokens_total.saturating_mul(100)) / 128_000).min(100) as u32;
                let status_bar = crate::tui::widgets::status_bar::StatusBar {
                    model: &g.model,
                    agent: &g.agent_profile,
                    busy_label: &indicator_text,
                    context_pct,
                    elapsed_secs: elapsed,
                    mcp_servers: g.mcp_server_count,
                    sandbox_status: None,
                    last_turn: None,
                };

                crate::tui::tui_viewport::render_status_bar(frame, status_top_row, status_bar);
                if toolbar_permission_is_bypass(&g.permission_mode) {
                    let warn_text = " BYPASS ";
                    let warn = Paragraph::new(Line::from(Span::styled(
                        warn_text,
                        Style::default()
                            .fg(Color::Black)
                            .bg(theme::error())
                            .add_modifier(Modifier::BOLD),
                    )))
                    .style(Style::default().bg(theme::surface()));
                    let warn_rect = Rect {
                        x: status_top_row.x,
                        y: status_top_row.y,
                        width: warn_text.chars().count() as u16,
                        height: 1,
                    };
                    frame.render_widget(warn, warn_rect);
                }
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
                let input_block = Paragraph::new(Text::from(input_lines))
                    .block(
                        Block::default()
                            .borders(Borders::TOP)
                            .border_style(Style::default().fg(theme::border()))
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
                    let total_vis = filtered.len().clamp(1, COMMAND_PALETTE_MAX_ROWS);
                    let popup_area = centered_rect(area, COMMAND_PALETTE_WIDTH, (total_vis as u16).saturating_add(6));
                    let list_scroll = pick_abs.saturating_sub(COMMAND_PALETTE_MAX_ROWS / 2);
                    let list_end = (list_scroll + COMMAND_PALETTE_MAX_ROWS).min(filtered.len());
                    let mut popup_lines = vec![
                        Line::from(vec![
                            Span::styled(
                                "  Search ",
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
                        ]),
                        Line::default(),
                    ];
                    if selectable.is_empty() {
                        popup_lines.push(Line::from(Span::styled(
                            " No matching commands",
                            Style::default().fg(theme::muted()),
                        )));
                    } else {
                        for &idx in &filtered[list_scroll..list_end] {
                            match idx {
                                PaletteRow::Section(name) => {
                                    popup_lines.push(Line::from(Span::styled(
                                        format!("  {name}"),
                                        Style::default().fg(theme::muted()).add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
                                    )));
                                }
                                PaletteRow::Entry { label, shortcut } => {
                                    let global = filtered.iter().position(|r| std::ptr::eq(*r, idx)).unwrap_or(0);
                                    let is_selected = global == pick_abs;
                                    let label_style = if is_selected {
                                        Style::default().fg(Color::Black).bg(theme::user()).add_modifier(Modifier::BOLD)
                                    } else {
                                        Style::default().fg(theme::text())
                                    };
                                    let shortcut_style = if is_selected {
                                        Style::default().fg(Color::Black).bg(theme::user())
                                    } else {
                                        Style::default().fg(theme::muted())
                                    };
                                    let pad = 36usize.saturating_sub(label.len()).saturating_sub(2);
                                    let mut spans = vec![Span::styled(format!("  {label}"), label_style)];
                                    if !shortcut.is_empty() {
                                        spans.push(Span::styled(format!("{:>pad$}", shortcut, pad = pad), shortcut_style));
                                    }
                                    popup_lines.push(Line::from(spans));
                                }
                            }
                        }
                    }
                    popup_lines.push(Line::default());
                    popup_lines.push(Line::from(Span::styled(
                        " Enter apply · Esc close ",
                        Style::default().fg(theme::muted()),
                    )));
                    frame.render_widget(ClearWidget, popup_area);
                    let popup = Paragraph::new(Text::from(popup_lines))
                        .block(
                            Block::default()
                                .borders(Borders::ALL)
                                .border_style(Style::default().fg(theme::border()))
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
                    let sel = clamp_selection(g.connect_menu_index, &rows);
                    let selected_row = row_index_for_selection(&rows, sel);
                    let anim_ms = g.started.elapsed().as_millis();
                    let cursor = selection_pulse(anim_ms);
                    let sparkle = title_sparkle(anim_ms);
                    let dots = status_dots(anim_ms);
                    let body_lines = rows.len().max(1);
                    let popup_h = (body_lines as u16).saturating_add(9).clamp(11, 24);
                    let popup_area = centered_rect(area, 66, popup_h);
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
                                "Search ",
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
                            let ConnectRow::Provider {
                                kind,
                                title,
                                subtitle,
                                oauth_login_slug,
                            } = row;
                            let is_sel = selected_row == Some(i);
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
                            let logged = if matches!(*title, "Copilot") {
                                auth_store.copilot.is_some()
                            } else {
                                match kind {
                                    ProviderKind::OpenAi => auth_store.openai_oauth.is_some(),
                                    ProviderKind::Anthropic => auth_store.anthropic.is_some(),
                                    ProviderKind::Antigravity => auth_store.antigravity.is_some(),
                                    ProviderKind::OpenRouter => false,
                                    ProviderKind::OpenCodeZen => auth_store.opencodezen_oauth.is_some(),
                                }
                            };
                            let status = if logged {
                                " · logged in".to_string()
                            } else {
                                match oauth_login_slug {
                                    Some(_) => format!(" · not logged in{dots}"),
                                    None => String::new(),
                                }
                            };
                            let prefix = if is_sel {
                                cursor.to_string()
                            } else {
                                "  ".to_string()
                            };
                            lines.push(Line::from(vec![
                                Span::styled(format!(" {prefix}{title}"), main_st),
                                Span::styled(format!(" — {subtitle}{status}"), sub_st),
                            ]));
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
                        " ↑↓ select · Enter connect · Esc close ",
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
                    render_approval_popup(frame, area, &req);
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
                                                        if let Some(ref tx) = question_answer_tx {
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
                    if !matches!(key.kind, KeyEventKind::Press) {
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
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) | (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                                g.command_palette_open = false;
                                g.command_palette_query.clear();
                                g.palette_index = 0;
                            }
                            (KeyCode::Up, _) => {
                                if g.palette_index > 0 {
                                    g.palette_index -= 1;
                                }
                            }
                            (KeyCode::Down, _) => {
                                let filtered = filter_palette_rows(&g.command_palette_query);
                                let selectable = palette_selectable_indices(&filtered);
                                if !selectable.is_empty() {
                                    g.palette_index = (g.palette_index + 1)
                                        .min(selectable.len().saturating_sub(1));
                                }
                            }
                            (KeyCode::Enter, _) => {
                                let filtered = filter_palette_rows(&g.command_palette_query);
                                let selectable = palette_selectable_indices(&filtered);
                                let pick = g.palette_index.min(selectable.len().saturating_sub(1));
                                if let Some(&abs_idx) = selectable.get(pick)
                                    && let PaletteRow::Entry { label, .. } = filtered[abs_idx]
                                {
                                    let cmd = palette_command_for_label(label);
                                    g.set_input_text(cmd.to_string());
                                }
                                g.command_palette_open = false;
                                g.command_palette_query.clear();
                                g.palette_index = 0;
                            }
                            (KeyCode::Backspace, _) => {
                                g.command_palette_query.pop();
                                let filtered = filter_palette_rows(&g.command_palette_query);
                                let selectable = palette_selectable_indices(&filtered);
                                g.palette_index =
                                    g.palette_index.min(selectable.len().saturating_sub(1));
                            }
                            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                                g.command_palette_query.push(c);
                                let filtered = filter_palette_rows(&g.command_palette_query);
                                let selectable = palette_selectable_indices(&filtered);
                                g.palette_index =
                                    g.palette_index.min(selectable.len().saturating_sub(1));
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Info modal (read-only scrollable popup).
                    if g.info_modal_open {
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) | (KeyCode::Char('q'), KeyModifiers::NONE) => {
                                g.close_info_modal();
                            }
                            (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                                g.info_modal_scroll = g.info_modal_scroll.saturating_sub(1);
                            }
                            (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                                let max_vis = g.info_modal_view_rows.max(1);
                                let max_scroll = g.info_modal_lines.len().saturating_sub(max_vis);
                                g.info_modal_scroll = (g.info_modal_scroll + 1).min(max_scroll);
                            }
                            (KeyCode::Left, _) | (KeyCode::Char('h'), KeyModifiers::NONE) => {
                                g.info_modal_hscroll = g.info_modal_hscroll.saturating_sub(4);
                            }
                            (KeyCode::Right, _) | (KeyCode::Char('l'), KeyModifiers::NONE) => {
                                g.info_modal_hscroll = g.info_modal_hscroll.saturating_add(4);
                            }
                            (KeyCode::Home, _) => {
                                g.info_modal_scroll = 0;
                                g.info_modal_hscroll = 0;
                            }
                            (KeyCode::End, _) => {
                                let max_vis = g.info_modal_view_rows.max(1);
                                g.info_modal_scroll =
                                    g.info_modal_lines.len().saturating_sub(max_vis);
                                let max_line_chars = g
                                    .info_modal_lines
                                    .iter()
                                    .map(|line| line.chars().count())
                                    .max()
                                    .unwrap_or(0);
                                g.info_modal_hscroll =
                                    max_line_chars.saturating_sub(g.info_modal_view_cols.max(1));
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Model picker popup.
                    if g.model_picker_open {
                        let filter = g.model_picker_search.to_ascii_lowercase();
                        let selectable_count = g
                            .model_picker_entries
                            .iter()
                            .filter(|e| {
                                !e.is_header
                                    && (filter.is_empty()
                                        || e.label.to_ascii_lowercase().contains(&filter)
                                        || e.detail.to_ascii_lowercase().contains(&filter))
                            })
                            .count();
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => {
                                g.close_model_picker();
                            }
                            (KeyCode::Up, _) => {
                                if selectable_count > 0 {
                                    g.model_picker_index = g
                                        .model_picker_index
                                        .saturating_sub(1)
                                        .min(selectable_count - 1);
                                }
                            }
                            (KeyCode::Down, _) => {
                                if selectable_count > 0 {
                                    g.model_picker_index =
                                        (g.model_picker_index + 1).min(selectable_count - 1);
                                }
                            }
                            (KeyCode::Enter, _) => {
                                let selectable: Vec<&ModelPickerEntry> = g
                                    .model_picker_entries
                                    .iter()
                                    .filter(|e| {
                                        !e.is_header
                                            && (filter.is_empty()
                                                || e.label.to_ascii_lowercase().contains(&filter)
                                                || e.detail.to_ascii_lowercase().contains(&filter))
                                    })
                                    .collect();
                                let pick =
                                    g.model_picker_index.min(selectable.len().saturating_sub(1));
                                if let Some(entry) = selectable.get(pick) {
                                    let action = entry.action.clone();
                                    g.close_model_picker();
                                    drop(g);
                                    match action {
                                        ModelPickerAction::SwitchProvider(p) => {
                                            let _ = cmd_tx.send(TuiCmd::ApplyModelProvider(p));
                                        }
                                        ModelPickerAction::SwitchCopilot => {
                                            let _ = cmd_tx.send(TuiCmd::Submit(
                                                "/provider copilot".to_string(),
                                            ));
                                        }
                                        ModelPickerAction::ApplyModel(m) => {
                                            let _ = cmd_tx.send(TuiCmd::ApplyModel(m));
                                        }
                                    }
                                }
                            }
                            (KeyCode::Backspace, _) => {
                                g.model_picker_search.pop();
                                g.model_picker_index = 0;
                                g.model_picker_scroll = 0;
                            }
                            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                                g.model_picker_search.push(c);
                                g.model_picker_index = 0;
                                g.model_picker_scroll = 0;
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Connect provider (OpenCode-style `/connect`).
                    if g.connect_modal_open {
                        let rows = build_connect_rows(&g.connect_search);
                        let n_sel = selectable_row_indices(&rows).len();
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => {
                                if !g.onboarding_mode {
                                    g.close_connect_modal();
                                }
                            }
                            (KeyCode::Enter, _) if g.connect_modal_ignore_enter_once => {
                                g.connect_modal_ignore_enter_once = false;
                            }
                            (KeyCode::Up, _) => {
                                g.connect_modal_ignore_enter_once = false;
                                if n_sel > 0 {
                                    g.connect_menu_index =
                                        g.connect_menu_index.saturating_sub(1).min(n_sel - 1);
                                }
                            }
                            (KeyCode::Down, _) => {
                                g.connect_modal_ignore_enter_once = false;
                                if n_sel > 0 {
                                    g.connect_menu_index =
                                        (g.connect_menu_index + 1).min(n_sel - 1);
                                }
                            }
                            (KeyCode::Enter, _) => {
                                g.connect_modal_ignore_enter_once = false;
                                if let Some((p, _title, oauth_login_slug)) =
                                    provider_at_selection(&rows, g.connect_menu_index)
                                {
                                    g.close_connect_modal();
                                    drop(g);
                                    if let Some(slug) =
                                        oauth_login_slug.or_else(|| oauth_login_provider_slug(p))
                                    {
                                        let _ =
                                            cmd_tx.send(TuiCmd::Submit(format!("/login {slug}")));
                                    } else {
                                        let _ = cmd_tx.send(TuiCmd::PromptApiKey(p, true));
                                    }
                                }
                            }
                            (KeyCode::Backspace, _) => {
                                g.connect_modal_ignore_enter_once = false;
                                g.connect_search.pop();
                                g.connect_menu_index = 0;
                                g.connect_modal_scroll = 0;
                                let rows2 = build_connect_rows(&g.connect_search);
                                g.connect_menu_index =
                                    clamp_selection(g.connect_menu_index, &rows2);
                            }
                            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                                g.connect_modal_ignore_enter_once = false;
                                g.connect_search.push(c);
                                g.connect_menu_index = 0;
                                g.connect_modal_scroll = 0;
                                let rows2 = build_connect_rows(&g.connect_search);
                                g.connect_menu_index =
                                    clamp_selection(g.connect_menu_index, &rows2);
                            }
                            _ => {}
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
                        let n = ProviderKind::ALL.len();
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => {
                                g.close_provider_picker();
                            }
                            (KeyCode::Up, _) => {
                                g.provider_picker_index = g.provider_picker_index.saturating_sub(1);
                            }
                            (KeyCode::Down, _) => {
                                if n > 0 {
                                    g.provider_picker_index = (g.provider_picker_index + 1) % n;
                                }
                            }
                            (KeyCode::Enter, _) => {
                                if n == 0 {
                                    g.close_provider_picker();
                                    continue;
                                }
                                let p = ProviderKind::ALL[g.provider_picker_index.min(n - 1)];
                                let for_key = g.provider_picker_for_api_key;
                                g.close_provider_picker();
                                if for_key {
                                    drop(g);
                                    if let Some(slug) = oauth_login_provider_slug(p) {
                                        let _ =
                                            cmd_tx.send(TuiCmd::Submit(format!("/login {slug}")));
                                    } else {
                                        let _ = cmd_tx.send(TuiCmd::PromptApiKey(p, false));
                                    }
                                } else {
                                    drop(g);
                                    let _ = cmd_tx.send(TuiCmd::ApplyDefaultProvider(p));
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Branch picker keyboard handling.
                    if g.branch_picker_open {
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => {
                                g.close_branch_picker();
                            }
                            (KeyCode::Up, _) => {
                                if !filtered_branch_indices(
                                    &g.branch_picker_branches,
                                    &g.branch_picker_query,
                                )
                                .is_empty()
                                {
                                    g.branch_picker_index = g.branch_picker_index.saturating_sub(1);
                                }
                            }
                            (KeyCode::Down, _) => {
                                let n = filtered_branch_indices(
                                    &g.branch_picker_branches,
                                    &g.branch_picker_query,
                                )
                                .len();
                                if n > 0 {
                                    g.branch_picker_index = (g.branch_picker_index + 1).min(n - 1);
                                }
                            }
                            (KeyCode::Enter, _) => {
                                let cmd = branch_picker_enter_command(
                                    &g.branch_picker_branches,
                                    &g.branch_picker_query,
                                    g.branch_picker_index,
                                );
                                g.close_branch_picker();
                                if let Some(c) = cmd {
                                    drop(g);
                                    let _ = cmd_tx.send(c);
                                }
                            }
                            (KeyCode::Backspace, _) => {
                                g.branch_picker_query.pop();
                                let filtered = filtered_branch_indices(
                                    &g.branch_picker_branches,
                                    &g.branch_picker_query,
                                );
                                g.branch_picker_index =
                                    g.branch_picker_index.min(filtered.len().saturating_sub(1));
                            }
                            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                                g.branch_picker_query.push(c);
                                g.branch_picker_index = 0;
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Question modal keyboard handling.
                    if g.question_modal_open {
                        if let Some(ref q) = g.active_question.clone() {
                            // Total items: 1 (suggested) + options.len() + (1 if allow_custom for "Chat about this")
                            let total = 1 + q.options.len() + if q.allow_custom { 1 } else { 0 };
                            match (key.code, key.modifiers) {
                                (KeyCode::Esc, _) => {
                                    if q.allow_custom {
                                        // Fall back to inline text input
                                        g.close_question_modal();
                                    }
                                    // If !allow_custom, Esc is a no-op
                                }
                                (KeyCode::Up, _) | (KeyCode::Char('k'), KeyModifiers::NONE) => {
                                    g.question_modal_index =
                                        g.question_modal_index.saturating_sub(1);
                                }
                                (KeyCode::Down, _) | (KeyCode::Char('j'), KeyModifiers::NONE) => {
                                    g.question_modal_index =
                                        (g.question_modal_index + 1).min(total - 1);
                                }
                                (KeyCode::Enter, _) => {
                                    let idx = g.question_modal_index;
                                    let sel = if idx == 0 {
                                        // Suggested answer
                                        Some(QuestionSelection::Suggested)
                                    } else if idx <= q.options.len() {
                                        // Regular option (1-based → 0-based)
                                        Some(QuestionSelection::Option {
                                            option_id: q.options[idx - 1].id.clone(),
                                        })
                                    } else {
                                        // "Chat about this" — fall back to inline text input
                                        None
                                    };

                                    if let Some(sel) = sel {
                                        let qid = q.question_id.clone();
                                        g.close_question_modal();
                                        g.active_question = None;
                                        drop(g);
                                        if let Some(ref tx) = question_answer_tx {
                                            let _ = tx.send((qid, sel));
                                        } else {
                                            let _ = cmd_tx.send(TuiCmd::QuestionAnswer(sel));
                                        }
                                    } else {
                                        // "Chat about this" — close modal, keep active_question
                                        g.close_question_modal();
                                    }
                                }
                                _ => {}
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
                        let filter = g.session_picker_search.to_ascii_lowercase();
                        let count = g
                            .session_picker_entries
                            .iter()
                            .filter(|entry| {
                                filter.is_empty()
                                    || entry.search_text.to_ascii_lowercase().contains(&filter)
                            })
                            .count();
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) => {
                                g.close_session_picker();
                            }
                            (KeyCode::Up, _) => {
                                g.session_picker_index = g.session_picker_index.saturating_sub(1);
                            }
                            (KeyCode::Down, _) => {
                                if count > 0 {
                                    g.session_picker_index =
                                        (g.session_picker_index + 1).min(count.saturating_sub(1));
                                }
                            }
                            (KeyCode::Enter, _) => {
                                let filtered: Vec<_> = g
                                    .session_picker_entries
                                    .iter()
                                    .filter(|entry| {
                                        filter.is_empty()
                                            || entry
                                                .search_text
                                                .to_ascii_lowercase()
                                                .contains(&filter)
                                    })
                                    .collect();
                                let pick =
                                    g.session_picker_index.min(filtered.len().saturating_sub(1));
                                if let Some(entry) = filtered.get(pick) {
                                    let id = entry.id.clone();
                                    g.close_session_picker();
                                    drop(g);
                                    let _ = cmd_tx.send(TuiCmd::ResumeSession(id));
                                }
                            }
                            (KeyCode::Backspace, _) => {
                                g.session_picker_search.pop();
                                g.session_picker_index = 0;
                                g.session_picker_scroll = 0;
                            }
                            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                                g.session_picker_search.push(c);
                                g.session_picker_index = 0;
                                g.session_picker_scroll = 0;
                            }
                            _ => {}
                        }
                        continue;
                    }

                    if g.pins_modal_open {
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) | (KeyCode::Char('o'), KeyModifiers::CONTROL) => {
                                g.close_pins_modal();
                            }
                            (KeyCode::Up, _) => {
                                g.pins_modal_index = g.pins_modal_index.saturating_sub(1);
                            }
                            (KeyCode::Down, _) => {
                                if !g.pinned_notes.is_empty() {
                                    g.pins_modal_index = (g.pins_modal_index + 1)
                                        .min(g.pinned_notes.len().saturating_sub(1));
                                }
                            }
                            (KeyCode::Backspace, _) => {
                                let idx = g.pins_modal_index;
                                if idx < g.pinned_notes.len() {
                                    g.pinned_notes.remove(idx);
                                    g.pins_modal_index = g
                                        .pins_modal_index
                                        .min(g.pinned_notes.len().saturating_sub(1));
                                }
                                if g.pinned_notes.is_empty() {
                                    g.close_pins_modal();
                                }
                            }
                            (KeyCode::Enter, _) => {
                                g.scroll_lines = 0;
                                g.transcript_follow_tail = false;
                                g.close_pins_modal();
                            }
                            (KeyCode::F(6), _) => {
                                let msg = if let Some(note) = g.pinned_notes.get(g.pins_modal_index)
                                {
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
                            _ => {}
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
                        let needle = g.composer_history_search_query.to_ascii_lowercase();
                        let matches: Vec<String> = composer_history
                            .iter()
                            .rev()
                            .filter(|entry| {
                                needle.is_empty() || entry.to_ascii_lowercase().contains(&needle)
                            })
                            .take(64)
                            .cloned()
                            .collect();
                        match (key.code, key.modifiers) {
                            (KeyCode::Esc, _) | (KeyCode::Char('r'), KeyModifiers::CONTROL) => {
                                g.composer_history_search_open = false;
                                g.composer_history_search_query.clear();
                                g.composer_history_search_index = 0;
                            }
                            (KeyCode::Backspace, _) => {
                                g.composer_history_search_query.pop();
                                g.composer_history_search_index = 0;
                            }
                            (KeyCode::Up, _) => {
                                if !matches.is_empty() {
                                    g.composer_history_search_index =
                                        g.composer_history_search_index.saturating_sub(1);
                                }
                            }
                            (KeyCode::Down, _) => {
                                if !matches.is_empty() {
                                    g.composer_history_search_index =
                                        (g.composer_history_search_index + 1)
                                            .min(matches.len().saturating_sub(1));
                                }
                            }
                            (KeyCode::Enter, _) => {
                                if !matches.is_empty() {
                                    let pick = g
                                        .composer_history_search_index
                                        .min(matches.len().saturating_sub(1));
                                    if let Some(entry) = matches.get(pick) {
                                        g.set_input_text(entry.clone());
                                    }
                                }
                                g.composer_history_search_open = false;
                                g.composer_history_search_query.clear();
                                g.composer_history_search_index = 0;
                            }
                            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                                g.composer_history_search_query.push(c);
                                g.composer_history_search_index = 0;
                            }
                            _ => {}
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
                                let input_json: serde_json::Value =
                                    serde_json::from_str(&req.input).unwrap_or_default();
                                let pattern = suggest_allow_pattern(&req.tool, &input_json);
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
                                    g.blocks.push(DisplayBlock::System(
                                        "Empty line — type y or n (or yes/no, ok, deny). Ctrl+Y = approve, Ctrl+N = deny."
                                            .into(),
                                    ));
                                    g.touch_transcript();
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
                                    if let Some(ref tx) = question_answer_tx {
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
                                    if let Some(ref tx) = question_answer_tx {
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
        TuiCmd, apply_selected_at_completion, branch_picker_enter_command,
        completed_at_mention_range_before_cursor, composer_line, delete_completed_at_mention,
        escape_cancels_active_turn, extract_embedded_path_fragments, filtered_branch_indices,
        is_click_jitter, mouse_scroll_step, parse_approval_verdict, parse_candidate_image_path,
        parse_md_line, pasted_lines_token, render_markdown_lines, render_markdown_lines_with_hits,
        request_turn_cancel, stage_pasted_image_paths, transcript_lines_and_hits,
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
        let (lines, hits) = render_markdown_lines_with_hits("```rs\nlet x = 1;\n```", true);
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
        let (lines, _hits) =
            render_markdown_lines_with_hits("```diff\n+add line\n-del line\n@@ hunk\n```", false);
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

    #[test]
    fn pasted_lines_token_counts_multiline_input() {
        assert_eq!(
            pasted_lines_token("a\nb\nc", 1),
            Some("[pasted 3 lines #1]".into())
        );
        assert_eq!(pasted_lines_token("single line", 1), None);
    }

    #[test]
    fn parse_candidate_image_path_supports_quoted_and_escaped_spaces() {
        let parsed = parse_candidate_image_path("\"./assets/Screenshot\\ 2026-05-05.png\"")
            .expect("image path should parse");
        assert_eq!(
            parsed.to_string_lossy(),
            "./assets/Screenshot 2026-05-05.png"
        );
    }

    #[test]
    fn parse_candidate_image_path_supports_file_url_and_percent_decoding() {
        let parsed = parse_candidate_image_path("file:///tmp/Screenshot%202026-05-05.png")
            .expect("file URL should parse");
        assert_eq!(parsed.to_string_lossy(), "/tmp/Screenshot 2026-05-05.png");
    }

    #[test]
    fn parse_candidate_image_path_ignores_non_image_text() {
        assert!(parse_candidate_image_path("just some notes").is_none());
        assert!(parse_candidate_image_path("README.md").is_none());
    }

    #[test]
    fn extract_embedded_path_fragments_finds_path_inside_sentence() {
        let line = "please inspect this image: /tmp/Screenshot 2026-05-05 125529.png thanks";
        let fragments = extract_embedded_path_fragments(line);
        assert!(fragments.iter().any(|f| {
            f == "/tmp/Screenshot 2026-05-05 125529.png"
                || f == "/tmp/Screenshot 2026-05-05 125529.png thanks"
        }));
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
