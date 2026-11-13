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

    /// One-line description shown beside the command in the popup (Codex-style).
    pub(crate) fn description_text(&self) -> String {
        match self {
            SlashEntry::Command(s) => {
                crate::slash_commands::slash_command_description(s).to_string()
            }
            SlashEntry::Skill {
                description,
                source,
                ..
            } => {
                let tag = match source {
                    SkillSource::AgentsMd => "AGENTS.md skill",
                    SkillSource::FileSystem => "skill",
                    SkillSource::CommandFile => "custom command",
                };
                match description {
                    Some(desc) => desc.clone(),
                    None => tag.to_string(),
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
            // Hide the auto-generated AGENTS.md "learned-*" memory skills —
            // they clutter the command list and aren't user-invokable commands.
            .filter(|s| !s.command.starts_with("learned-"))
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
    let recent = recent_commands();
    // Recently used first (when they match), then commands that start
    // with what's typed, then ones that mention it anywhere (name or
    // description) — each by group, then name.
    let mut ranked: Vec<(u8, usize, u8, String, &SlashEntry)> = entries
        .iter()
        .filter_map(|e| {
            let cmd = e.command_str();
            let name = cmd.trim_start_matches('/').to_lowercase();
            let tier = if name.starts_with(&needle) {
                1
            } else if needle.len() >= 3
                && (name.contains(&needle) || e.description_text().to_lowercase().contains(&needle))
            {
                2
            } else {
                return None;
            };
            let recency = recent.iter().position(|r| *r == cmd);
            let (tier, rank) = match recency {
                Some(i) if tier == 1 => (0, i),
                _ => (tier, 0),
            };
            Some((tier, rank, command_group(&cmd).0, name, e))
        })
        .collect();
    ranked.sort_by(|a, b| (a.0, a.1, a.2, &a.3).cmp(&(b.0, b.1, b.2, &b.3)));
    ranked.into_iter().map(|r| r.4).collect()
}

/// The group a command belongs to in the `/` menu: its order and name.
pub(crate) fn command_group(cmd: &str) -> (u8, &'static str) {
    const GROUPS: &[(&str, &[&str])] = &[
        (
            "Session",
            &[
                "/resume",
                "/detach",
                "/new",
                "/clear",
                "/fork",
                "/rewind",
                "/undo",
                "/redo",
                "/rename",
                "/session-name",
                "/sessions",
                "/sessions-clean",
                "/delete",
                "/export",
                "/replay",
                "/share",
                "/handoff",
                "/side",
                "/btw",
                "/retry",
                "/stop",
                "/quit",
                "/exit",
                "/q",
                "/logs",
                "/attach",
                "/input",
            ],
        ),
        (
            "Context & cost",
            &[
                "/context", "/compact", "/cost", "/usage", "/stats", "/memory", "/effort",
                "/model", "/models", "/think", "/add-dir",
            ],
        ),
        (
            "Board & Jira",
            &["/board", "/plan", "/goal", "/goals", "/todo"],
        ),
        (
            "Team",
            &[
                "/invite", "/join", "/chat", "/people", "/kick", "/call", "/mute", "/watch",
                "/office",
            ],
        ),
        (
            "Agents & tools",
            &[
                "/agents",
                "/browser",
                "/app",
                "/mcp",
                "/plugins",
                "/plugin",
                "/skills",
                "/hooks",
                "/run-bg",
                "/schedule",
                "/fleet",
                "/tour",
                "/bashes",
                "/ps",
                "/diff",
                "/diagram",
                "/talk",
                "/standup",
                "/slack",
                "/teams",
                "/review",
                "/verify",
                "/second-opinion",
                "/security-review",
                "/pr-comments",
                "/init",
                "/ide",
            ],
        ),
        (
            "Settings",
            &[
                "/config",
                "/theme",
                "/permissions",
                "/mode",
                "/vim",
                "/sounds",
                "/speak",
                "/terminal-setup",
                "/statusline",
                "/keymaps",
                "/login",
                "/logout",
                "/connect",
                "/setup",
                "/reload",
            ],
        ),
        (
            "Help",
            &[
                "/help",
                "/status",
                "/release-notes",
                "/doctor",
                "/dashboard",
                "/dash",
                "/transcript",
            ],
        ),
    ];
    for (i, (name, cmds)) in GROUPS.iter().enumerate() {
        if cmds.contains(&cmd) {
            return (i as u8, name);
        }
    }
    (GROUPS.len() as u8, "")
}

/// Recently used commands, newest first (`~/.dcode-ai/recent-commands`),
/// read once and kept in memory.
static RECENT: std::sync::Mutex<Option<Vec<String>>> = std::sync::Mutex::new(None);
const RECENT_KEEP: usize = 12;

fn recent_path() -> Option<PathBuf> {
    dcode_ai_common::config::dcode_ai_home_dir().map(|h| h.join("recent-commands"))
}

pub(crate) fn recent_commands() -> Vec<String> {
    let Ok(mut r) = RECENT.lock() else {
        return Vec::new();
    };
    r.get_or_insert_with(|| {
        if cfg!(test) {
            return Vec::new();
        }
        recent_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .map(|t| {
                t.lines()
                    .map(str::to_string)
                    .filter(|l| l.starts_with('/'))
                    .collect()
            })
            .unwrap_or_default()
    })
    .clone()
}

/// Remember `cmd` (e.g. `/board`) as just used.
pub fn record_recent_command(cmd: &str) {
    if !cmd.starts_with('/') || cmd.len() < 2 || cmd.contains(char::is_whitespace) {
        return;
    }
    let mut list = recent_commands();
    list.retain(|c| c != cmd);
    list.insert(0, cmd.to_string());
    list.truncate(RECENT_KEEP);
    if let Ok(mut r) = RECENT.lock() {
        *r = Some(list.clone());
    }
    if !cfg!(test)
        && let Some(p) = recent_path()
    {
        let _ = std::fs::write(p, list.join("\n") + "\n");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmds(list: &[&'static str]) -> Vec<SlashEntry> {
        list.iter().map(|c| SlashEntry::Command(c)).collect()
    }

    fn names(v: Vec<&SlashEntry>) -> Vec<String> {
        v.into_iter().map(|e| e.command_str()).collect()
    }

    #[test]
    fn grouped_recent_first_and_found_by_description() {
        let all = cmds(&[
            "/theme", "/compact", "/board", "/context", "/resume", "/cost",
        ]);
        // Grouped: Session, then Context & cost, then Board, then Settings.
        let by_group = names(filter_slash_entries(&all, "/"));
        assert_eq!(by_group[0], "/resume");
        assert_eq!(by_group.last().unwrap(), "/theme");
        // Prefix first.
        assert_eq!(
            names(filter_slash_entries(&all, "/co")),
            ["/compact", "/context", "/cost"]
        );
        // A word from a description finds the command (`/board` mentions Jira).
        assert!(names(filter_slash_entries(&all, "/jira")).contains(&"/board".to_string()));
        // Just used: on top.
        record_recent_command("/cost");
        assert_eq!(names(filter_slash_entries(&all, "/co"))[0], "/cost");
        assert_eq!(names(filter_slash_entries(&all, "/"))[0], "/cost");
        // Not a slash panel: nothing.
        assert!(filter_slash_entries(&all, "/co x").is_empty());
    }
}
