//! Full-screen session TUI: transcript, streaming assistant, composer.

use std::path::{Path, PathBuf};

use crate::file_mentions;
use crate::slash_commands::SLASH_COMMANDS;
use crate::tui::connect_modal::{
    ConnectRow, build_connect_rows, clamp_selection, provider_at_selection,
    row_index_for_selection, selectable_row_indices,
};
use crate::tui::state::{
    ApprovalRequest, DisplayBlock, ModelPickerAction, ModelPickerEntry, TuiSessionState,
};
use arboard::Clipboard;
use crossterm::{
    cursor::{Hide, MoveToColumn, Show},
    event::{
        DisableBracketedPaste, EnableBracketedPaste,
        Event, KeyCode, KeyModifiers, MouseButton, MouseEventKind, poll, read,
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
use pulldown_cmark::{
    Alignment, CodeBlockKind, Event as MdEvent, HeadingLevel, Options, Parser, Tag, TagEnd,
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear as ClearWidget, Paragraph, Wrap},
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
const SIDEBAR_WIDTH: u16 = 32;
const SIDEBAR_MIN_TOTAL_WIDTH: u16 = 110;
const COMMAND_PALETTE_WIDTH: u16 = 48;
const COMMAND_PALETTE_MAX_ROWS: usize = 10;

fn slash_panel_visible(buffer: &str) -> bool {
    buffer.starts_with('/') && !buffer.contains(' ')
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
    let inner_w = area_width.saturating_sub(2).max(1) as usize;
    let prompt_w = 2usize; // "❯ "
    let input_chars = state.input_buffer.chars().count().saturating_add(1); // cursor cell
    let first_line_cells = prompt_w.saturating_add(input_chars).max(1);
    let wrapped_input_lines = first_line_cells.div_ceil(inner_w).max(1) as u16;

    let mut content_lines = wrapped_input_lines;
    if !state.staged_image_attachments.is_empty() {
        content_lines = content_lines.saturating_add(1);
    }
    let show_hint = state.active_approval.is_some()
        || (state.active_question.is_some() && !state.question_modal_open)
        || state.input_buffer.is_empty();
    if show_hint {
        content_lines = content_lines.saturating_add(1);
    }

    content_lines.saturating_add(2).clamp(3, 10)
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
    let prompt = Span::styled("❯ ", Style::default().fg(theme::user()).bold());
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
        label: "Toggle sidebar",
        shortcut: "ctrl+x b",
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
        "Toggle sidebar" => "/sidebar toggle",
        "Config" => "/config",
        "Doctor" => "/doctor",
        "Help" => "/help",
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

fn layout_chunks(area: Rect, slash_h: u16, input_h: u16) -> (Rect, Rect, Option<Rect>, Rect) {
    let input_h = input_h.max(3);
    if slash_h > 0 {
        let c = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(4),
                Constraint::Length(2),
                Constraint::Length(slash_h),
                Constraint::Length(input_h),
            ])
            .split(area);
        (c[0], c[1], Some(c[2]), c[3])
    } else {
        let c = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(4),
                Constraint::Length(2),
                Constraint::Length(input_h),
            ])
            .split(area);
        (c[0], c[1], None, c[2])
    }
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

fn layout_with_sidebar(area: Rect, sidebar_open: bool) -> (Rect, Option<Rect>) {
    if !sidebar_open || area.width < SIDEBAR_MIN_TOTAL_WIDTH {
        return (area, None);
    }
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(60), Constraint::Length(SIDEBAR_WIDTH)])
        .split(area);
    (chunks[0], Some(chunks[1]))
}

fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    let popup_w = width
        .min(area.width.saturating_sub(2).max(20))
        .min(area.width);
    let popup_h = height
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

fn escape_cancels_active_turn(state: &TuiSessionState) -> bool {
    matches!(
        state.current_busy_state,
        BusyState::Thinking | BusyState::Streaming | BusyState::ToolRunning
    )
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

fn stage_pasted_image_paths(state: &mut TuiSessionState, pasted: &str) -> Result<usize, String> {
    let normalized = pasted.replace("\r\n", "\n").replace('\r', "\n");
    let trimmed = normalized.trim();
    if trimmed.is_empty() {
        return Ok(0);
    }

    let mut staged = 0usize;
    let mut tried_any = false;
    let mut first_error: Option<String> = None;

    for line in trimmed.lines() {
        let raw = strip_outer_quotes(line);
        if raw.is_empty() {
            continue;
        }
        let raw = raw.strip_prefix("file://").unwrap_or(raw);
        let src = Path::new(raw);
        if !looks_like_image_path(src) || !path_looks_explicit(raw, src) {
            continue;
        }

        tried_any = true;
        match crate::image_attach::import_image_file(&state.workspace_root, &state.session_id, src)
        {
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

    if staged > 0 {
        state.blocks.push(DisplayBlock::System(format!(
            "[image] staged {staged} image(s) — Enter to send"
        )));
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
    let idx = state.cursor_char_idx;
    let mut cs: Vec<char> = state.input_buffer.chars().collect();
    let ins: Vec<char> = insert_text.chars().collect();
    let ins_len = ins.len();
    cs.splice(idx..idx, ins);
    state.input_buffer = cs.into_iter().collect();
    state.cursor_char_idx += ins_len;
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
    matches!(
        kind,
        MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
    )
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

pub fn setup_terminal(_mouse_capture: bool) -> anyhow::Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().map_err(|e| anyhow::anyhow!("enable_raw_mode: {e}"))?;
    let res: anyhow::Result<Terminal<CrosstermBackend<Stdout>>> = (|| {
        let mut out = stdout();
        execute!(out, EnterAlternateScreen)?;
        let _ = execute!(out, EnableBracketedPaste);
        use std::io::Write;
        // Enable click+scroll tracking (?1000h) with SGR extended coords (?1006h).
        // Omit ?1002h/?1003h (motion tracking) so Shift+drag bypasses capture for
        // native text selection in all modern terminals (Windows Terminal, iTerm2, etc.).
        out.write_all(b"\x1b[?1000h\x1b[?1006h")
            .map_err(|e| anyhow::anyhow!("mouse enable: {e}"))?;
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
    let _ = out.write_all(b"\x1b[?1000l\x1b[?1006l");
    let _ = out.flush();
    let _ = execute!(out, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}

/// Runtime toggle for mouse capture (button+wheel tracking).
/// When `on`, scroll-wheel works but native terminal selection is intercepted.
/// When `off`, terminal owns mouse → user can click-drag to select + copy.
pub fn set_mouse_capture(on: bool) {
    use std::io::Write;
    let mut out = stdout();
    let bytes: &[u8] = if on {
        b"\x1b[?1000h\x1b[?1006h"
    } else {
        b"\x1b[?1000l\x1b[?1006l"
    };
    let _ = out.write_all(bytes);
    let _ = out.flush();
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

/// Build scrollable transcript lines + optional mouse/click targets per line.
fn transcript_lines_and_hits(
    state: &TuiSessionState,
    width: u16,
) -> (Vec<Line<'static>>, Vec<LineAnswerHit>) {
    let w = width.max(20) as usize;
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut hits: Vec<LineAnswerHit> = Vec::new();

    for block in &state.blocks {
        match block {
            DisplayBlock::User(content) => {
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(vec![Span::styled(
                        " YOU ",
                        Style::default()
                            .fg(Color::Black)
                            .bg(theme::user())
                            .add_modifier(Modifier::BOLD),
                    )]),
                    None,
                );
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
                for text_line in wrap_text(content, w) {
                    push_transcript_line(
                        &mut lines,
                        &mut hits,
                        Line::from(Span::styled(text_line, Style::default().fg(theme::text()))),
                        None,
                    );
                }
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
            }
            DisplayBlock::Assistant(content) => {
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(vec![Span::styled(
                        " dcode-ai ",
                        Style::default()
                            .fg(Color::Black)
                            .bg(theme::assistant())
                            .add_modifier(Modifier::BOLD),
                    )]),
                    None,
                );
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
                let (md_lines, md_hits) =
                    render_markdown_lines_with_hits(content, state.code_line_numbers);
                for (md_line, md_hit) in md_lines.into_iter().zip(md_hits.into_iter()) {
                    push_transcript_line(&mut lines, &mut hits, md_line, md_hit);
                }
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
            }
            DisplayBlock::ToolRunning { name, input, .. } => {
                let preview = tool_input_preview(name, input);
                let mut spans = vec![
                    Span::styled(" ⚡ ", Style::default().fg(theme::tool())),
                    Span::styled(
                        name.to_string(),
                        Style::default()
                            .fg(theme::tool())
                            .add_modifier(Modifier::BOLD),
                    ),
                ];
                if !preview.is_empty() {
                    spans.push(Span::styled(
                        format!("  {}", truncate_chars(&preview, 96)),
                        Style::default().fg(theme::muted()),
                    ));
                }
                spans.push(Span::styled(
                    "  running…",
                    Style::default()
                        .fg(theme::muted())
                        .add_modifier(Modifier::ITALIC),
                ));
                push_transcript_line(&mut lines, &mut hits, Line::from(spans), None);
            }
            DisplayBlock::ApprovalPending(req) => {
                render_approval_block(&mut lines, &mut hits, req, w);
            }
            DisplayBlock::ApprovalResolved { tool, approved } => {
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
                    ]),
                    None,
                );
                push_transcript_line(&mut lines, &mut hits, Line::default(), None);
            }
            DisplayBlock::ToolDone { name, ok, detail } => {
                let (icon, st) = if *ok {
                    ("✓", Style::default().fg(theme::success()))
                } else {
                    ("✗", Style::default().fg(theme::error()))
                };
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(vec![
                        Span::styled(format!(" {icon} "), st),
                        Span::styled(
                            name.to_string(),
                            Style::default()
                                .fg(theme::tool())
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!(" — {}", truncate_chars(detail, 100)),
                            Style::default().fg(theme::muted()),
                        ),
                    ]),
                    None,
                );
            }
            DisplayBlock::System(s) => {
                push_transcript_line(
                    &mut lines,
                    &mut hits,
                    Line::from(Span::styled(
                        format!(" ‣ {s}"),
                        Style::default().fg(theme::warn()),
                    )),
                    None,
                );
            }
            DisplayBlock::Question(q) => {
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
                for text_line in wrap_text(&q.prompt, w) {
                    push_transcript_line(
                        &mut lines,
                        &mut hits,
                        Line::from(Span::styled(text_line, Style::default().fg(theme::text()))),
                        None,
                    );
                }
                // When the modal is open, skip inline options — the popup handles selection.
                if !state.question_modal_open {
                    push_transcript_line(
                        &mut lines,
                        &mut hits,
                        Line::from(vec![
                            Span::styled(
                                format!("  [0] suggested: {} ", q.suggested_answer),
                                Style::default()
                                    .fg(theme::success())
                                    .add_modifier(Modifier::UNDERLINED),
                            ),
                            Span::styled("(click)", Style::default().fg(theme::muted())),
                        ]),
                        Some(LineClickHit::Question(QuestionSelection::Suggested)),
                    );
                    for (i, o) in q.options.iter().enumerate() {
                        push_transcript_line(
                            &mut lines,
                            &mut hits,
                            Line::from(vec![
                                Span::styled(
                                    format!("  [{}] ({}) {} ", i + 1, o.id, o.label),
                                    Style::default()
                                        .fg(theme::text())
                                        .add_modifier(Modifier::UNDERLINED),
                                ),
                                Span::styled("(click)", Style::default().fg(theme::muted())),
                            ]),
                            Some(LineClickHit::Question(QuestionSelection::Option {
                                option_id: o.id.clone(),
                            })),
                        );
                    }
                    if q.allow_custom {
                        push_transcript_line(
                            &mut lines,
                            &mut hits,
                            Line::from(Span::styled(
                                "  [c] type your own answer below, then Enter",
                                Style::default().fg(theme::muted()),
                            )),
                            None,
                        );
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
                for text_line in wrap_text(content, w.saturating_sub(2)) {
                    push_transcript_line(
                        &mut lines,
                        &mut hits,
                        Line::from(vec![
                            Span::styled(
                                " │ ",
                                Style::default().fg(theme::muted()),
                            ),
                            Span::styled(
                                text_line,
                                Style::default()
                                    .fg(theme::muted())
                                    .add_modifier(Modifier::ITALIC),
                            ),
                        ]),
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
        for text_line in wrap_text(thinking, w.saturating_sub(2)) {
            push_transcript_line(
                &mut lines,
                &mut hits,
                Line::from(vec![
                    Span::styled(" │ ", Style::default().fg(theme::muted())),
                    Span::styled(
                        text_line,
                        Style::default()
                            .fg(theme::muted())
                            .add_modifier(Modifier::ITALIC),
                    ),
                ]),
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
                Span::styled(" streaming", Style::default().fg(theme::muted())),
            ]),
            None,
        );
        push_transcript_line(&mut lines, &mut hits, Line::default(), None);
        let (md_lines, md_hits) = render_markdown_lines_with_hits(stream, state.code_line_numbers);
        for (md_line, md_hit) in md_lines.into_iter().zip(md_hits.into_iter()) {
            push_transcript_line(&mut lines, &mut hits, md_line, md_hit);
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
                "Tab  agent   Ctrl+P  palette   Ctrl+V  image   /  slash   @  files   !  shell   F12  toggle mouse (select text)",
                Style::default().fg(theme::muted()),
            )),
            None,
        );
    }

    (lines, hits)
}

fn transcript_lines(state: &TuiSessionState, width: u16) -> Vec<Line<'static>> {
    transcript_lines_and_hits(state, width).0
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

/// Extract a compact single-line preview of a tool call input for the transcript.
/// Tries common argument keys (path/file_path/command/pattern) from JSON first.
fn tool_input_preview(name: &str, input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed)
        && let Some(obj) = v.as_object()
    {
        let keys = [
            "file_path",
            "path",
            "filename",
            "command",
            "cmd",
            "pattern",
            "query",
            "url",
            "target_file",
            "notebook_path",
        ];
        for k in keys {
            if let Some(val) = obj.get(k).and_then(|x| x.as_str())
                && !val.is_empty()
            {
                return val.replace('\n', " ");
            }
        }
        let lowname = name.to_ascii_lowercase();
        if lowname.contains("bash") || lowname.contains("shell") {
            if let Some(val) = obj.get("command").and_then(|x| x.as_str()) {
                return val.replace('\n', " ");
            }
        }
    }
    trimmed.replace('\n', " ")
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
            if line.is_empty() {
                line = word.to_string();
            } else if line.len() + 1 + word.len() <= width {
                line.push(' ');
                line.push_str(word);
            } else {
                out.push(line);
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

fn push_wrapped_plain_lines(
    lines: &mut Vec<Line<'static>>,
    hits: &mut Vec<LineAnswerHit>,
    text: &str,
    width: usize,
    style: Style,
) {
    for source_line in text.lines() {
        let wrapped = wrap_preformatted_line(source_line, width);
        for line in wrapped {
            push_transcript_line(lines, hits, Line::from(Span::styled(line, style)), None);
        }
        if source_line.is_empty() {
            push_transcript_line(lines, hits, Line::default(), None);
        }
    }
}

fn render_approval_block(
    lines: &mut Vec<Line<'static>>,
    hits: &mut Vec<LineAnswerHit>,
    req: &ApprovalRequest,
    width: usize,
) {
    push_transcript_line(
        lines,
        hits,
        Line::from(vec![
            Span::styled(
                " ? ",
                Style::default().fg(Color::Black).bg(theme::warn()).bold(),
            ),
            Span::styled(
                format!(" approval required: {}", req.tool),
                Style::default()
                    .fg(theme::warn())
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        None,
    );
    push_transcript_line(lines, hits, Line::default(), None);
    for text_line in wrap_text(&req.description, width) {
        push_transcript_line(
            lines,
            hits,
            Line::from(Span::styled(text_line, Style::default().fg(theme::text()))),
            None,
        );
    }
    push_transcript_line(lines, hits, Line::default(), None);
    push_transcript_line(
        lines,
        hits,
        Line::from(Span::styled(
            " Input ",
            Style::default()
                .fg(theme::muted())
                .add_modifier(Modifier::BOLD),
        )),
        None,
    );
    push_wrapped_plain_lines(
        lines,
        hits,
        &req.input,
        width,
        Style::default().fg(theme::muted()),
    );
    push_transcript_line(
        lines,
        hits,
        Line::from(Span::styled(
            " Reply: y/n · Ctrl+Y approve · Ctrl+N deny · Ctrl+U always allow · /approve · /deny",
            Style::default().fg(theme::muted()),
        )),
        None,
    );
    push_transcript_line(lines, hits, Line::default(), None);
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
    let syntax = language
        .as_deref()
        .and_then(|lang| ps.find_syntax_by_token(lang))
        .unwrap_or_else(|| ps.find_syntax_plain_text());
    let mut highlighter = HighlightLines::new(syntax, syntect_theme());
    let line_count = code.split('\n').count().max(1);
    let line_num_width = line_count.to_string().len();
    let copy_payload = code.to_string();

    for (idx, raw) in code.split('\n').enumerate() {
        let highlights = highlighter.highlight_line(raw, ps).unwrap_or_default();
        let mut spans: Vec<Span<'static>> = Vec::new();
        if code_line_numbers {
            spans.push(Span::styled(
                format!("{:>width$} │ ", idx + 1, width = line_num_width),
                Style::default().fg(theme::muted()),
            ));
        }
        if highlights.is_empty() {
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

fn push_table_border(out: &mut Vec<Line<'static>>, hits: &mut Vec<LineAnswerHit>, s: String) {
    out.push(Line::from(Span::styled(
        s,
        Style::default().fg(theme::muted()),
    )));
    hits.push(None);
}

fn align_table_cell(text: &str, width: usize, alignment: Alignment) -> String {
    let cell_width = text.chars().count();
    if cell_width >= width {
        return text.to_string();
    }
    let pad = width - cell_width;
    match alignment {
        Alignment::Right => format!("{}{}", " ".repeat(pad), text),
        Alignment::Center => {
            let left = pad / 2;
            let right = pad - left;
            format!("{}{}{}", " ".repeat(left), text, " ".repeat(right))
        }
        _ => format!("{}{}", text, " ".repeat(pad)),
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

    let mut widths = vec![3usize; col_count];
    for row in table.header_rows.iter().chain(table.body_rows.iter()) {
        for (i, cell) in row.iter().enumerate() {
            widths[i] = widths[i].max(cell.chars().count());
        }
    }

    let top = format!(
        "┌{}┐",
        widths
            .iter()
            .map(|w| "─".repeat(w + 2))
            .collect::<Vec<_>>()
            .join("┬")
    );
    push_table_border(out, hits, top);

    let render_row = |row: &[String], is_header: bool| -> Line<'static> {
        let mut spans = vec![Span::styled("│ ", Style::default().fg(theme::muted()))];
        for i in 0..col_count {
            let val = row.get(i).map(String::as_str).unwrap_or("");
            let aligned = align_table_cell(
                val,
                widths[i],
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
            spans.push(Span::styled(
                if i + 1 == col_count { " │" } else { " │ " },
                Style::default().fg(theme::muted()),
            ));
        }
        Line::from(spans)
    };

    for row in &table.header_rows {
        out.push(render_row(row, true));
        hits.push(None);
    }

    if !table.header_rows.is_empty() {
        let header_sep = format!(
            "├{}┤",
            widths
                .iter()
                .map(|w| "─".repeat(w + 2))
                .collect::<Vec<_>>()
                .join("┼")
        );
        push_table_border(out, hits, header_sep);
    }

    for row in &table.body_rows {
        out.push(render_row(row, false));
        hits.push(None);
    }

    let bottom = format!(
        "└{}┘",
        widths
            .iter()
            .map(|w| "─".repeat(w + 2))
            .collect::<Vec<_>>()
            .join("┴")
    );
    push_table_border(out, hits, bottom);
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
                    let hashes = match level {
                        HeadingLevel::H1 => 1,
                        HeadingLevel::H2 => 2,
                        HeadingLevel::H3 => 3,
                        HeadingLevel::H4 => 4,
                        HeadingLevel::H5 => 5,
                        HeadingLevel::H6 => 6,
                    };
                    current.push(Span::styled(
                        format!("{} ", "#".repeat(hashes)),
                        Style::default()
                            .fg(theme::assistant())
                            .add_modifier(Modifier::BOLD),
                    ));
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
                    Some(MdOpenTag::Paragraph)
                    | Some(MdOpenTag::Heading)
                    | Some(MdOpenTag::Item) => {
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

    let heading_level = trimmed.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&heading_level) && trimmed.chars().nth(heading_level) == Some(' ') {
        let mut spans = Vec::new();
        if !leading.is_empty() {
            spans.push(Span::styled(leading.to_string(), base));
        }
        let marker = "#".repeat(heading_level);
        spans.push(Span::styled(
            format!("{marker} "),
            Style::default()
                .fg(theme::assistant())
                .add_modifier(Modifier::BOLD),
        ));
        let text = &trimmed[heading_level + 1..];
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
        spans.extend(parse_md_inline(
            text,
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
/// `mouse_capture`: enables terminal mouse capture for scroll and click events.
/// `scroll_speed`: lines per scroll event.
pub fn run_blocking(
    state: Arc<Mutex<TuiSessionState>>,
    cmd_tx: UnboundedSender<TuiCmd>,
    question_answer_tx: Option<UnboundedSender<(String, QuestionSelection)>>,
    approval_answer_tx: Option<UnboundedSender<ApprovalAnswer>>,
    show_run_banner: bool,
    cancel_flag: Option<Arc<std::sync::atomic::AtomicBool>>,
    mouse_capture: bool,
    scroll_speed: u16,
) -> anyhow::Result<()> {
    let mut terminal = setup_terminal(mouse_capture)?;

    // Load slash entries once: hardcoded commands + discovered skills
    let skill_dirs = vec![PathBuf::from(".dcode-ai/skills")];
    let workspace_root = {
        let g = state.lock().map_err(|e| anyhow::anyhow!("lock: {e}"))?;
        g.workspace_root.clone()
    };
    let slash_entries = load_slash_entries(&workspace_root, &skill_dirs);
    let workspace_files = file_mentions::discover_workspace_files(&workspace_root);

    if show_run_banner && let Ok(mut g) = state.lock() {
        g.blocks.push(DisplayBlock::System(
            "Interactive run — type a message, Tab cycles agent profile, Ctrl+P opens commands."
                .into(),
        ));
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

            terminal.draw(|frame| {
                let area = frame.area();
                let (main_area, sidebar_opt) = layout_with_sidebar(area, g.sidebar_open);
                let input_h = composer_input_height(&g, main_area.width);
                let (tr, st_r, slash_opt, inp_r) = layout_chunks(main_area, chrome_h, input_h);

                let transcript_h = tr.height.saturating_sub(2) as usize;
                let inner_w = tr.width.saturating_sub(2);
                let (lines, _hits) = transcript_lines_and_hits(&g, inner_w);
                let total = lines.len();
                let max_scroll = total.saturating_sub(transcript_h);
                if g.transcript_follow_tail {
                    g.scroll_lines = max_scroll;
                } else {
                    g.scroll_lines = g.scroll_lines.min(max_scroll);
                }
                let start = g.scroll_lines;
                let end = (start + transcript_h).min(total);
                let visible: Vec<Line> = if start < end {
                    lines[start..end].to_vec()
                } else {
                    vec![]
                };

                let scroll_info = if g.scroll_lines > 0 || !g.transcript_follow_tail {
                    format!(" lines {}–{} of {} ", start + 1, end.min(total), total)
                } else {
                    format!(" {} lines ", total)
                };
                let title = format!(
                    " transcript —{scroll_info}(↑↓ wheel · End bottom) ",
                );
                let main = Paragraph::new(Text::from(visible))
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(theme::border()))
                            .title(Span::styled(title, Style::default().fg(theme::muted()))),
                    )
                    .wrap(Wrap { trim: false })
                    .style(Style::default().bg(theme::bg()));

                frame.render_widget(main, tr);

                if let Some(sidebar) = sidebar_opt {
                    let sections = Layout::default()
                        .direction(Direction::Vertical)
                        .constraints([
                            Constraint::Length(12),
                            Constraint::Length(8),
                            Constraint::Min(10),
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
                let indicator_color =
                    crate::tui::busy_indicator::color_for_state(g.current_busy_state);
                let busy = Span::styled(indicator_text, Style::default().fg(indicator_color));
                let approval_hint = if g.active_approval.is_some() {
                    Span::styled(" !approve ", Style::default().fg(theme::error()))
                } else {
                    Span::raw("")
                };
                let q_hint = if g.active_question.is_some() {
                    Span::styled(" ?answer ", Style::default().fg(theme::warn()))
                } else {
                    Span::raw("")
                };
                // Session / tokens / cost live in the sidebar; keep the bar short and obvious about bypass.
                let perm_span = if toolbar_permission_is_bypass(&g.permission_mode) {
                    Span::styled(
                        " BYPASS — tools run without approval ",
                        Style::default()
                            .fg(Color::Black)
                            .bg(theme::error())
                            .add_modifier(Modifier::BOLD),
                    )
                } else {
                    Span::styled(
                        format!(" perm:{} ", g.permission_mode),
                        Style::default().fg(theme::muted()),
                    )
                };
                let time_span = Span::styled(
                    format!("{:02}:{:02}", elapsed / 60, elapsed % 60),
                    Style::default().fg(theme::muted()),
                );

                let cancel_hint_text = " Esc cancel ";
                let cancel_hint = escape_cancels_active_turn(&g).then(|| {
                    Span::styled(
                        cancel_hint_text,
                        Style::default()
                            .fg(Color::Black)
                            .bg(theme::warn())
                            .add_modifier(Modifier::BOLD),
                    )
                });
                let status_rect = if cancel_hint.is_some() && st_r.width > cancel_hint_text.len() as u16 {
                    Layout::default()
                        .direction(Direction::Horizontal)
                        .constraints([
                            Constraint::Min(0),
                            Constraint::Length(cancel_hint_text.len() as u16),
                        ])
                        .split(st_r)[0]
                } else {
                    st_r
                };

                // Compute the character-cell x-offset before any borrow of `g` escapes into `status_spans`.
                let branch_char_offset = 4 + g.model.len() + 4 + g.agent_profile.len() + 4;
                let branch_text = if g.current_branch.is_empty() {
                    String::new()
                } else {
                    format!("⎇ {}", g.current_branch)
                };
                let branch_span_style = Style::default()
                    .fg(theme::tool())
                    .add_modifier(Modifier::UNDERLINED);

                // Store the branch chip bounds for click hit-testing.
                if status_rect.width > branch_char_offset as u16 && !branch_text.is_empty() {
                    let chip_len = branch_text.len() as u16;
                    g.branch_chip_bounds = Some(Rect::new(
                        status_rect.x + branch_char_offset as u16,
                        status_rect.y,
                        chip_len.min(status_rect.width - branch_char_offset as u16),
                        1,
                    ));
                } else {
                    g.branch_chip_bounds = None;
                }

                let mut status_spans = vec![
                    busy,
                    approval_hint,
                    q_hint,
                    Span::raw(" │ "),
                    Span::styled(&g.model, Style::default().fg(theme::user())),
                    Span::raw(" │ "),
                    Span::styled(&g.agent_profile, Style::default().fg(theme::assistant())),
                    Span::raw(" │ "),
                    // branch_text borrow ends before next mutable use of `g` below
                    Span::styled(branch_text, branch_span_style),
                    Span::raw(" │ "),
                    perm_span,
                ];
                // Sidebar is hidden on narrow terminals — put session/tokens/cost back on the bar.
                if sidebar_opt.is_none() {
                    status_spans.push(Span::raw(" │ "));
                    status_spans.push(Span::styled(
                        g.session_id[..8.min(g.session_id.len())].to_string(),
                        Style::default().fg(theme::muted()),
                    ));
                    status_spans.extend([
                        Span::raw(" │ in:"),
                        Span::styled(
                            format!("{}", g.input_tokens),
                            Style::default().fg(theme::text()),
                        ),
                        Span::raw(" out:"),
                        Span::styled(
                            format!("{}", g.output_tokens),
                            Style::default().fg(theme::text()),
                        ),
                        Span::raw(" │ $"),
                        Span::styled(
                            format!("{:.4}", g.cost_usd),
                            Style::default().fg(theme::success()),
                        ),
                    ]);
                }
                status_spans.push(Span::raw(" │ "));
                status_spans.push(time_span);
                let status = Line::from(status_spans);
                let bar = Paragraph::new(status).style(Style::default().bg(theme::surface()));
                frame.render_widget(bar, status_rect);

                let sidebar_chip = if g.sidebar_open {
                    " sidebar:on [^B] "
                } else {
                    " sidebar:off [^B] "
                };
                let sidebar_chip_w = sidebar_chip.len() as u16;
                // Keep this chip near the right edge and leave room for the clock/cancel hint.
                if status_rect.width > sidebar_chip_w.saturating_add(12) {
                    let sidebar_rect = Rect::new(
                        status_rect
                            .x
                            .saturating_add(status_rect.width.saturating_sub(sidebar_chip_w + 8)),
                        status_rect.y,
                        sidebar_chip_w,
                        1,
                    );
                    g.sidebar_toggle_bounds = Some(sidebar_rect);
                    let sidebar_style = if g.sidebar_open {
                        Style::default()
                            .fg(Color::Black)
                            .bg(theme::success())
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default()
                            .fg(Color::Black)
                            .bg(theme::muted())
                            .add_modifier(Modifier::BOLD)
                    };
                    frame.render_widget(
                        Paragraph::new(Line::from(Span::styled(sidebar_chip, sidebar_style)))
                            .style(Style::default().bg(theme::surface())),
                        sidebar_rect,
                    );
                } else {
                    g.sidebar_toggle_bounds = None;
                }

                if let Some(cancel_hint) = cancel_hint {
                    let hint_width = cancel_hint_text.len() as u16;
                    if st_r.width > hint_width {
                        let hint_rect = Rect::new(
                            st_r.x + st_r.width.saturating_sub(hint_width),
                            st_r.y,
                            hint_width,
                            1,
                        );
                        let hint_bar = Paragraph::new(Line::from(cancel_hint))
                            .style(Style::default().bg(theme::surface()));
                        frame.render_widget(hint_bar, hint_rect);
                    }
                }

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
                    Line::from(Span::styled(
                        "Approval: y/n · Ctrl+Y approve · Ctrl+N deny · Ctrl+U always allow · /approve · /deny · other /commands still work",
                        Style::default().fg(theme::error()),
                    ))
                } else if g.active_question.is_some() && !g.question_modal_open {
                    Line::from(Span::styled(
                        "Enter / 0 = suggested · 1–n = option · click underlined line · /auto-answer · End = transcript bottom (empty input)",
                        Style::default().fg(theme::warn()),
                    ))
                } else if g.input_buffer.is_empty() {
                    Line::from(Span::styled(
                        "Enter send · Tab agent · Ctrl+V image · /image · Ctrl+P palette · Ctrl+Q exit · Ctrl+L clear",
                        Style::default().fg(theme::muted()),
                    ))
                } else {
                    Line::default()
                };

                let input_title = if g.active_approval.is_some() {
                    " approval "
                } else if g.active_question.is_some() {
                    " answer "
                } else {
                    " message "
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
                input_lines.push(hint);
                let input_block = Paragraph::new(Text::from(input_lines))
                    .block(
                        Block::default()
                            .borders(Borders::ALL)
                            .border_style(Style::default().fg(theme::border()))
                            .title(Span::styled(input_title, Style::default().fg(theme::muted()))),
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
                    const PERM_LABELS: &[&str] = &["Default", "Plan", "AcceptEdits", "DontAsk", "BypassPermissions"];
                    let rows = (PERM_LABELS.len() as u16).saturating_add(6).max(8);
                    let popup_area = centered_rect(area, 40, rows);
                    let mut lines: Vec<Line> = vec![
                        Line::from(Span::styled(
                            " Permission mode ",
                            Style::default().fg(theme::muted()).add_modifier(Modifier::BOLD),
                        )),
                        Line::default(),
                    ];
                    for (i, name) in PERM_LABELS.iter().enumerate() {
                        let st = if i == g.permission_picker_index {
                            Style::default().fg(Color::Black).bg(theme::user()).add_modifier(Modifier::BOLD)
                        } else {
                            Style::default().fg(theme::text())
                        };
                        lines.push(Line::from(Span::styled(format!(" {name}"), st)));
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
                                .title(Span::styled(" permissions ", Style::default().fg(theme::muted()))),
                        )
                        .style(Style::default().bg(theme::surface()))
                        .wrap(Wrap { trim: false });
                    frame.render_widget(popup, popup_area);
                }

                if g.theme_picker_open {
                    let entries = g.theme_picker_entries.clone();
                    let rows = (entries.len() as u16).saturating_add(6).max(8).min(20);
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
                        // +4 for: title line, blank, blank before footer, footer
                        let rows = (total_items as u16).saturating_add(6).max(8);
                        let popup_w = 60u16.min(area.width.saturating_sub(4));
                        let popup_area = centered_rect(area, popup_w, rows);

                        let mut lines: Vec<Line> = vec![
                            Line::from(Span::styled(
                                format!(" {} ", q.prompt),
                                Style::default()
                                    .fg(theme::assistant())
                                    .add_modifier(Modifier::BOLD),
                            )),
                            Line::default(),
                        ];

                        // Suggested answer (index 0)
                        let suggested_label = format!(" Suggested: {} ", q.suggested_answer);
                        if g.question_modal_index == 0 {
                            lines.push(Line::from(Span::styled(
                                format!(" ► {}", suggested_label.trim()),
                                Style::default()
                                    .fg(Color::Black)
                                    .bg(theme::user())
                                    .add_modifier(Modifier::BOLD),
                            )));
                        } else {
                            lines.push(Line::from(Span::styled(
                                format!("   {}", suggested_label.trim()),
                                Style::default().fg(theme::text()),
                            )));
                        }

                        // Options (index 1..n)
                        for (i, o) in q.options.iter().enumerate() {
                            let item_idx = i + 1;
                            let label = format!("{} ", o.label);
                            if g.question_modal_index == item_idx {
                                lines.push(Line::from(Span::styled(
                                    format!(" ► {}", label.trim()),
                                    Style::default()
                                        .fg(Color::Black)
                                        .bg(theme::user())
                                        .add_modifier(Modifier::BOLD),
                                )));
                            } else {
                                lines.push(Line::from(Span::styled(
                                    format!("   {}", label.trim()),
                                    Style::default().fg(theme::text()),
                                )));
                            }
                        }

                        // "Chat about this" (last item, only if allow_custom)
                        if has_chat_option {
                            let chat_idx = 1 + q.options.len();
                            if g.question_modal_index == chat_idx {
                                lines.push(Line::from(Span::styled(
                                    " ► Chat about this",
                                    Style::default()
                                        .fg(Color::Black)
                                        .bg(theme::user())
                                        .add_modifier(Modifier::BOLD),
                                )));
                            } else {
                                lines.push(Line::from(Span::styled(
                                    "   Chat about this",
                                    Style::default()
                                        .fg(theme::muted())
                                        .add_modifier(Modifier::ITALIC),
                                )));
                            }
                        }

                        // Footer
                        lines.push(Line::default());
                        let footer_text = if has_chat_option {
                            " ↑↓ select · Enter confirm · Esc chat "
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

                if g.session_picker_open {
                    let filter = g.session_picker_search.to_ascii_lowercase();
                    let filtered_indices: Vec<usize> = g.session_picker_entries.iter().enumerate()
                        .filter(|(_, s)| filter.is_empty() || s.to_ascii_lowercase().contains(&filter))
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
                            let id = &g.session_picker_entries[filt_idx];
                            let is_current = id == &current_session_id;
                            let marker = if is_current { " *" } else { "" };
                            let st = if vis_idx == pick {
                                Style::default().fg(Color::Black).bg(theme::user()).add_modifier(Modifier::BOLD)
                            } else {
                                Style::default().fg(theme::text())
                            };
                            lines.push(Line::from(Span::styled(format!(" {id}{marker}"), st)));
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
                    let max_vis = 16usize;
                    let n_lines = g.info_modal_lines.len();
                    let popup_h = (n_lines.min(max_vis) as u16).saturating_add(6).max(8);
                    let popup_area = centered_rect(area, 70, popup_h);
                    let n_show = n_lines.min(max_vis);
                    let max_scroll = n_lines.saturating_sub(n_show);
                    g.info_modal_scroll = g.info_modal_scroll.min(max_scroll);
                    let start = g.info_modal_scroll;
                    let end = (start + n_show).min(n_lines);
                    let mut lines: Vec<Line> = Vec::new();
                    for line in &g.info_modal_lines[start..end] {
                        lines.push(Line::from(Span::styled(
                            format!(" {line}"),
                            Style::default().fg(theme::text()),
                        )));
                    }
                    if n_lines > max_vis {
                        lines.push(Line::from(Span::styled(
                            format!(" ─ {}/{} · ↑↓ scroll", start + 1, n_lines),
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
                    let body_lines = rows.len().max(1);
                    let popup_h = (body_lines as u16).saturating_add(9).clamp(11, 24);
                    let popup_area = centered_rect(area, 58, popup_h);
                    let auth_store = AuthStore::load().unwrap_or_default();
                    let is_logged = |kind: ProviderKind| match kind {
                        ProviderKind::OpenAi => auth_store.openai_oauth.is_some(),
                        ProviderKind::Anthropic => auth_store.anthropic.is_some(),
                        ProviderKind::Antigravity => auth_store.antigravity.is_some(),
                        ProviderKind::OpenRouter => false,
                        ProviderKind::OpenCodeZen => auth_store.opencodezen_oauth.is_some(),
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
                        for (i, row) in rows.iter().enumerate() {
                            match row {
                                ConnectRow::SectionHeader(h) => {
                                    lines.push(Line::from(Span::styled(
                                        format!(" {h}"),
                                        Style::default()
                                            .fg(theme::assistant())
                                            .add_modifier(Modifier::BOLD),
                                    )));
                                }
                                ConnectRow::Provider {
                                    kind,
                                    title,
                                    subtitle,
                                } => {
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
                                    let status = if is_logged(*kind) {
                                        " · logged in"
                                    } else {
                                        ""
                                    };
                                    lines.push(Line::from(vec![
                                        Span::styled(format!(" {title}"), main_st),
                                        Span::styled(format!(" — {subtitle}{status}"), sub_st),
                                    ]));
                                }
                            }
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
                            " Connect a provider ",
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
            })?;
        }

        if poll(Duration::from_millis(40))? {
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
                Event::Mouse(_) if g.connect_modal_open => continue,
                Event::Mouse(_) if g.api_key_modal_open => continue,
                Event::Mouse(_) if g.provider_picker_open => continue,
                Event::Mouse(_) if g.permission_picker_open => continue,
                Event::Mouse(_) if g.agent_picker_open => continue,
                Event::Mouse(_) if g.theme_picker_open => continue,
                Event::Mouse(_) if g.session_picker_open => continue,
                Event::Mouse(_) if g.question_modal_open => continue,
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
                    let input_h = composer_input_height(&g, main_area.width);
                    let (tr, _, slash_r, _) = layout_chunks(main_area, sh, input_h);

                    // Mouse scroll works anywhere on the screen, not just inside the transcript.
                    if matches!(
                        m.kind,
                        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                    ) {
                        let inner_w = tr.width.saturating_sub(2);
                        let total = transcript_lines(&g, inner_w).len();
                        let th = tr.height.saturating_sub(2) as usize;
                        let max_scroll = total.saturating_sub(th);
                        match m.kind {
                            MouseEventKind::ScrollUp => {
                                g.transcript_follow_tail = false;
                                g.scroll_lines =
                                    g.scroll_lines.saturating_sub(scroll_speed as usize);
                            }
                            MouseEventKind::ScrollDown => {
                                g.scroll_lines =
                                    (g.scroll_lines + scroll_speed as usize).min(max_scroll);
                                if g.scroll_lines >= max_scroll {
                                    g.transcript_follow_tail = true;
                                }
                            }
                            _ => {}
                        }
                        continue;
                    }

                    // Transcript click targets (left-click on question options / code blocks).
                    if rect_contains(tr, m.column, m.row) {
                        let inner_w = tr.width.saturating_sub(2);
                        let (_lines, hits) = transcript_lines_and_hits(&g, inner_w);
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
                                                    let feedback = match Clipboard::new()
                                                        .and_then(|mut cb| cb.set_text(text))
                                                    {
                                                        Ok(_) => " Copied code block to clipboard"
                                                            .to_string(),
                                                        Err(e) => {
                                                            format!(" Clipboard copy failed: {e}")
                                                        }
                                                    };
                                                    g.blocks.push(DisplayBlock::System(feedback));
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
                                    g.input_buffer = slash_filtered[idx].command_str();
                                    g.cursor_char_idx = g.input_buffer.chars().count();
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
                                    g.input_buffer = buf;
                                    g.cursor_char_idx = cidx;
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

                    // Check click on sidebar toggle chip in status bar.
                    if let Some(bounds) = g.sidebar_toggle_bounds
                        && rect_contains(bounds, m.column, m.row)
                        && mouse_left_activated(m.kind)
                    {
                        g.toggle_sidebar();
                    }
                }
                Event::Paste(pasted) => match stage_pasted_image_paths(&mut g, &pasted) {
                    Ok(0) => insert_pasted_text(&mut g, &slash_entries, &pasted),
                    Ok(_) => {}
                    Err(e) => g.push_error(format!("[image] {e}")),
                },
                Event::FocusGained | Event::FocusLost => {}
                Event::Key(key) => {
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
                                    g.input_buffer = cmd.to_string();
                                    g.cursor_char_idx = g.input_buffer.chars().count();
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
                                let max_vis = 16usize;
                                let max_scroll = g.info_modal_lines.len().saturating_sub(max_vis);
                                g.info_modal_scroll = (g.info_modal_scroll + 1).min(max_scroll);
                            }
                            (KeyCode::Home, _) => {
                                g.info_modal_scroll = 0;
                            }
                            (KeyCode::End, _) => {
                                let max_vis = 16usize;
                                g.info_modal_scroll =
                                    g.info_modal_lines.len().saturating_sub(max_vis);
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
                            (KeyCode::Up, _) => {
                                if n_sel > 0 {
                                    g.connect_menu_index =
                                        g.connect_menu_index.saturating_sub(1).min(n_sel - 1);
                                }
                            }
                            (KeyCode::Down, _) => {
                                if n_sel > 0 {
                                    g.connect_menu_index =
                                        (g.connect_menu_index + 1).min(n_sel - 1);
                                }
                            }
                            (KeyCode::Enter, _) => {
                                if let Some(p) = provider_at_selection(&rows, g.connect_menu_index)
                                {
                                    g.close_connect_modal();
                                    drop(g);
                                    let _ = cmd_tx.send(TuiCmd::PromptApiKey(p, true));
                                }
                            }
                            (KeyCode::Backspace, _) => {
                                g.connect_search.pop();
                                g.connect_menu_index = 0;
                                g.connect_modal_scroll = 0;
                                let rows2 = build_connect_rows(&g.connect_search);
                                g.connect_menu_index =
                                    clamp_selection(g.connect_menu_index, &rows2);
                            }
                            (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
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
                                    let _ = cmd_tx.send(TuiCmd::PromptApiKey(p, false));
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
                                let name = g
                                    .theme_picker_entries
                                    .get(g.theme_picker_index)
                                    .cloned();
                                g.close_theme_picker();
                                drop(g);
                                // Ask repl to reapply persisted theme (no-op if same).
                                let _ = name; // index was not persisted — noop is fine
                            }
                            (KeyCode::Up, _) => {
                                if count > 0 {
                                    g.theme_picker_index =
                                        g.theme_picker_index.saturating_sub(1);
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
                                let name = g
                                    .theme_picker_entries
                                    .get(g.theme_picker_index)
                                    .cloned();
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
                            .filter(|s| {
                                filter.is_empty() || s.to_ascii_lowercase().contains(&filter)
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
                                let filtered: Vec<&String> = g
                                    .session_picker_entries
                                    .iter()
                                    .filter(|s| {
                                        filter.is_empty()
                                            || s.to_ascii_lowercase().contains(&filter)
                                    })
                                    .collect();
                                let pick =
                                    g.session_picker_index.min(filtered.len().saturating_sub(1));
                                if let Some(id) = filtered.get(pick) {
                                    let id = (*id).clone();
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
                                g.toggle_sidebar();
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
                        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                            if escape_cancels_active_turn(&g) {
                                request_turn_cancel(&mut g, cancel_flag.as_ref(), &cmd_tx);
                            } else {
                                g.should_exit = true;
                                let _ = cmd_tx.send(TuiCmd::Exit);
                                break;
                            }
                        }
                        (KeyCode::Char('l'), KeyModifiers::CONTROL) => {
                            g.blocks.clear();
                            g.streaming_assistant = None;
                            g.streaming_thinking = None;
                            g.scroll_lines = 0;
                            g.transcript_follow_tail = true;
                        }
                        (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                            g.command_palette_open = true;
                            g.command_palette_query.clear();
                            g.palette_index = 0;
                        }
                        (KeyCode::Char('b'), KeyModifiers::CONTROL) => {
                            g.toggle_sidebar();
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
                        (KeyCode::Tab, _) => {
                            if let Some((buf, cidx)) = apply_selected_at_completion(
                                &workspace_files,
                                &g.input_buffer,
                                g.cursor_char_idx,
                                g.at_menu_index,
                                false,
                            ) {
                                g.input_buffer = buf;
                                g.cursor_char_idx = cidx;
                            } else {
                                let slash_filtered =
                                    filter_slash_entries(&slash_entries, &g.input_buffer);
                                if !slash_filtered.is_empty()
                                    && slash_panel_visible(&g.input_buffer)
                                {
                                    let pick = g.slash_menu_index % slash_filtered.len();
                                    g.input_buffer = slash_filtered[pick].command_str();
                                    g.cursor_char_idx = g.input_buffer.chars().count();
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
                        (KeyCode::F(12), _) => {
                            g.mouse_capture_on = !g.mouse_capture_on;
                            let now_on = g.mouse_capture_on;
                            set_mouse_capture(now_on);
                            let msg = if now_on {
                                "mouse capture ON — wheel scrolls transcript (F12 to toggle)"
                            } else {
                                "mouse capture OFF — click-drag to select text, Ctrl+C to copy (F12 to toggle)"
                            };
                            g.blocks.push(DisplayBlock::System(msg.to_string()));
                        }
                        (KeyCode::Char('y'), KeyModifiers::CONTROL) => {
                            if let Some(req) = g.active_approval.clone() {
                                let call_id = req.call_id.clone();
                                g.input_buffer.clear();
                                g.cursor_char_idx = 0;
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
                                g.input_buffer.clear();
                                g.cursor_char_idx = 0;
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
                                g.input_buffer.clear();
                                g.cursor_char_idx = 0;
                                g.blocks.push(DisplayBlock::System(format!(
                                    "Always allowing: {pattern}"
                                )));
                                drop(g);
                                if let Some(ref tx) = approval_answer_tx {
                                    let _ =
                                        tx.send(ApprovalAnswer::AllowPattern { call_id, pattern });
                                }
                                continue;
                            }
                        }
                        (KeyCode::Enter, _) => {
                            if let Some((buf, cidx)) = apply_selected_at_completion(
                                &workspace_files,
                                &g.input_buffer,
                                g.cursor_char_idx,
                                g.at_menu_index,
                                true,
                            ) {
                                g.input_buffer = buf;
                                g.cursor_char_idx = cidx;
                                continue;
                            }
                            let line = std::mem::take(&mut g.input_buffer);
                            g.cursor_char_idx = 0;
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
                                continue;
                            }
                            let expanded = expand_paste_tokens(&line, &g.paste_store);
                            g.paste_store.clear();
                            g.paste_counter = 0;
                            drop(g);
                            let _ = cmd_tx.send(TuiCmd::Submit(expanded));
                        }
                        (KeyCode::Char('a'), KeyModifiers::CONTROL) | (KeyCode::Home, _) => {
                            g.cursor_char_idx = 0;
                        }
                        (KeyCode::Char('e'), KeyModifiers::CONTROL) => {
                            g.cursor_char_idx = g.input_buffer.chars().count();
                        }
                        (KeyCode::End, _) => {
                            if !g.input_buffer.is_empty() {
                                g.cursor_char_idx = g.input_buffer.chars().count();
                            } else {
                                let sz = terminal.size().ok();
                                if let Some(sz) = sz {
                                    let area = Rect::new(0, 0, sz.width, sz.height);
                                    let (main_area, _) = layout_with_sidebar(area, g.sidebar_open);
                                    let sh = composer_chrome_height(
                                        &slash_entries,
                                        &workspace_files,
                                        &g.input_buffer,
                                        g.cursor_char_idx,
                                    );
                                    let input_h = composer_input_height(&g, main_area.width);
                                    let (tr, _, _, _) = layout_chunks(main_area, sh, input_h);
                                    let total =
                                        transcript_lines(&g, tr.width.saturating_sub(2)).len();
                                    let th = tr.height.saturating_sub(2) as usize;
                                    let max_scroll = total.saturating_sub(th);
                                    g.transcript_follow_tail = true;
                                    g.scroll_lines = max_scroll;
                                }
                            }
                        }
                        (KeyCode::Left, _) => {
                            g.cursor_char_idx = g.cursor_char_idx.saturating_sub(1);
                        }
                        (KeyCode::Right, _) => {
                            let max = g.input_buffer.chars().count();
                            g.cursor_char_idx = (g.cursor_char_idx + 1).min(max);
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
                                    g.transcript_follow_tail = false;
                                    g.scroll_lines =
                                        g.scroll_lines.saturating_sub(scroll_speed as usize);
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
                                        let input_h = composer_input_height(&g, main_area.width);
                                        let (tr, _, _, _) = layout_chunks(main_area, sh, input_h);
                                        let lines =
                                            transcript_lines(&g, tr.width.saturating_sub(2));
                                        let total = lines.len();
                                        let th = tr.height.saturating_sub(2) as usize;
                                        let max_scroll = total.saturating_sub(th);
                                        g.scroll_lines = (g.scroll_lines + scroll_speed as usize)
                                            .min(max_scroll);
                                        if g.scroll_lines >= max_scroll {
                                            g.transcript_follow_tail = true;
                                        }
                                    }
                                }
                            }
                        }
                        (KeyCode::Backspace, _) => {
                            if g.cursor_char_idx > 0 {
                                if let Some((buf, cidx)) =
                                    delete_completed_at_mention(&g.input_buffer, g.cursor_char_idx)
                                {
                                    g.input_buffer = buf;
                                    g.cursor_char_idx = cidx;
                                } else {
                                    let idx = g.cursor_char_idx;
                                    let mut cs: Vec<char> = g.input_buffer.chars().collect();
                                    cs.remove(idx - 1);
                                    g.input_buffer = cs.into_iter().collect();
                                    g.cursor_char_idx -= 1;
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
                        (KeyCode::Char(c), KeyModifiers::NONE | KeyModifiers::SHIFT) => {
                            let idx = g.cursor_char_idx;
                            let mut cs: Vec<char> = g.input_buffer.chars().collect();
                            cs.insert(idx, c);
                            g.input_buffer = cs.into_iter().collect();
                            g.cursor_char_idx += 1;
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
        escape_cancels_active_turn, filtered_branch_indices, parse_approval_verdict, parse_md_line,
        pasted_lines_token, render_markdown_lines, render_markdown_lines_with_hits,
        request_turn_cancel,
    };
    use crate::tui::state::TuiSessionState;
    use dcode_ai_common::event::BusyState;
    use std::path::PathBuf;
    use std::sync::{Arc, atomic::AtomicBool};

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
    fn escape_only_cancels_active_turn_states() {
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
        assert!(!escape_cancels_active_turn(&state));
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
        assert_eq!(heading.spans[0].content, "## ");

        let list = parse_md_line("- item");
        assert_eq!(list.spans[0].content, "• ");
    }

    #[test]
    fn markdown_event_renderer_renders_heading_and_link() {
        let lines = render_markdown_lines("## Title\nSee [docs](https://example.com)");
        assert_eq!(lines[0].spans[0].content, "## ");
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
        assert!(flat.contains("┌"));
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
    fn pasted_lines_token_counts_multiline_input() {
        assert_eq!(
            pasted_lines_token("a\nb\nc", 1),
            Some("[pasted 3 lines #1]".into())
        );
        assert_eq!(pasted_lines_token("single line", 1), None);
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
