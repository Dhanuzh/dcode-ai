//! The slash command panel: its entries (hardcoded commands + discovered
//! skills), discovery/loading/filtering, and the small panel-visibility and
//! height helpers. Extracted from `tui::app`.

use std::path::{Path, PathBuf};

use dcode_ai_core::skills::{SkillCatalog, SkillSource};

use crate::slash_commands::SLASH_COMMANDS;

/// Max command rows shown in the slash/at panels before scrolling.
pub(crate) const SLASH_PANEL_MAX_ROWS: usize = 8;

/// The slash panel shows while the buffer is a bare `/command` token.
pub(crate) fn slash_panel_visible(buffer: &str) -> bool {
    buffer.starts_with('/') && !buffer.contains(' ')
}

pub(crate) fn slash_panel_height(filtered_len: usize) -> u16 {
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
    pub(crate) fn command_str(&self) -> String {
        match self {
            SlashEntry::Command(s) => s.to_string(),
            SlashEntry::Skill { command, .. } => format!("/{command}"),
        }
    }

    pub(crate) fn display_text(&self) -> String {
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
pub(crate) fn collect_skill_entries(
    workspace_root: &Path,
    skill_dirs: &[PathBuf],
) -> Vec<SlashEntry> {
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
pub(crate) fn load_slash_entries(workspace_root: &Path, skill_dirs: &[PathBuf]) -> Vec<SlashEntry> {
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
pub(crate) fn filter_slash_entries<'a>(
    entries: &'a [SlashEntry],
    buffer: &str,
) -> Vec<&'a SlashEntry> {
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
