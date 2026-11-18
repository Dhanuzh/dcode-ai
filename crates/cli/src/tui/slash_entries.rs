//! The slash command panel: its entries (hardcoded commands + discovered
//! skills), discovery/loading/filtering, and the small panel-visibility and
//! height helpers. Extracted from `tui::app`.

use std::path::{Path, PathBuf};

use dcode_ai_core::skills::{SkillCatalog, SkillSource};

use crate::slash_commands::SLASH_COMMANDS;

/// Max command rows shown in the slash/at panels before scrolling.
pub(crate) const SLASH_PANEL_MAX_ROWS: usize = 8;

/// A command's options: (option, needs more text after it, what it does).
type Options = &'static [(&'static str, bool, &'static str)];

/// The options of commands that take them, for the dropdown after
/// `/command `: (command, [(option, needs more text after it, what it does)]).
pub(crate) const SUBCOMMANDS: &[(&str, Options)] = &[
    (
        "/video",
        &[
            ("list", false, "your recordings"),
            (
                "open",
                false,
                "play the newest (or `open <id> contact|preview|short`)",
            ),
            (
                "preview",
                false,
                "a quick low-resolution draft to check timing",
            ),
            ("render", false, "render again from the edit list"),
            (
                "render all",
                false,
                "re-render every recording (after a style change)",
            ),
            (
                "edit",
                true,
                "change it in plain words — \"under 30 s, no music\"",
            ),
            ("short", false, "add a 9:16 version (Shorts, Reels, Status)"),
            (
                "script",
                false,
                "the narration lines and which have your voice",
            ),
            ("voice", false, "record your own narration, line by line"),
            (
                "hear",
                false,
                "check your takes against the script (Whisper)",
            ),
            ("note", true, "a review note: <line> <what to change>"),
            ("notes", false, "the open review notes"),
            ("apply", false, "the agent applies the review notes"),
            ("translate", true, "another language: <language>"),
            ("status", false, "takes, renders, approval"),
            ("approve", false, "sign off on what's rendered"),
            ("publish", false, "upload folders per platform"),
            ("setup", false, "download ffmpeg (MP4 and music)"),
            (
                "setup voice",
                false,
                "download Piper, a natural draft voice (or `setup voice <name>`)",
            ),
            (
                "voices",
                false,
                "Piper voices: installed, to download, in use",
            ),
            ("voices try", true, "hear a voice: <name> [text]"),
            ("voices use", true, "draft voice from now on: <name> [id]"),
            ("steps", false, "the recording's steps, numbered"),
            (
                "check",
                false,
                "edit report: length, long narration, cut-away lines, merged zooms",
            ),
            (
                "timeline",
                false,
                "the edit as a time bar: kept parts, zooms, lines",
            ),
            ("nudge", true, "<line|zoom-N> <±seconds>: move one"),
            (
                "autoedit",
                false,
                "cut and zoom by where the screen changes (screen recordings)",
            ),
            (
                "doc",
                false,
                "a step-by-step how-to guide (Markdown + HTML)",
            ),
            (
                "brand",
                false,
                "the brand kit: logo, colours, font, intro/outro (`brand init`)",
            ),
            ("redo", true, "re-record one step: <step>"),
            ("frames", false, "the stills, drawn in the terminal"),
            (
                "show",
                false,
                "the contact sheet (or `show <id> <frame>`) in the terminal",
            ),
        ],
    ),
    (
        "/record",
        &[
            (
                "--stills",
                true,
                "one sharp still per step (dashboards, reports)",
            ),
            (
                "--screen",
                true,
                "the whole screen: desktop apps, the terminal",
            ),
            ("--webcam", true, "your camera in a corner bubble too"),
        ],
    ),
    (
        "/talk",
        &[
            ("send", false, "say it and send it"),
            (
                "loop",
                false,
                "hands-free: it answers aloud, then listens again",
            ),
            ("test", false, "check the microphone"),
            (
                "ptt",
                false,
                "push-to-talk: speak, then Enter to send (no pause detection)",
            ),
            (
                "wake",
                false,
                "hands-free: say \"hey dcode\", then what you need",
            ),
            (
                "lang",
                false,
                "any language: answers in the language you speak (`lang ta` for Tamil)",
            ),
            (
                "voice",
                false,
                "the voice replies are read in (Piper or Windows)",
            ),
            ("persona", false, "how replies sound: calm, brief, cheerful"),
        ],
    ),
    ("/standup", &[("voice", false, "talk through your day")]),
    (
        "/slack",
        &[("on", false, "start answering"), ("off", false, "stop")],
    ),
    (
        "/teams",
        &[("on", false, "start answering"), ("off", false, "stop")],
    ),
    (
        "/sounds",
        &[("on", false, "turn on"), ("off", false, "turn off")],
    ),
    (
        "/watch",
        &[
            ("lan", false, "share on this network"),
            ("stop", false, "stop sharing"),
        ],
    ),
    (
        "/invite",
        &[
            ("web", false, "join from a browser"),
            ("relay", true, "through a relay: <host:port>"),
            ("stop", false, "close the invite"),
        ],
    ),
    ("/todo", &[("done", true, "tick one off: <n>")]),
    (
        "/board",
        &[
            ("add", true, "a card: [jira] <title>"),
            ("work", true, "work on a card: <KEY> [worktree]"),
            ("plan", true, "plan cards for a goal: <goal>"),
            ("run", false, "let agents work the board"),
            ("jira", true, "link a Jira project: <PROJECT> [me] [sprint]"),
        ],
    ),
    (
        "/schedule",
        &[
            ("add", true, "\"weekdays 09:00\" [--auto] <what to do>"),
            ("list", false, "scheduled runs"),
            ("run", true, "run one now: <n>"),
            ("pause", true, "<n>"),
            ("resume", true, "<n>"),
            ("rm", true, "<n>"),
        ],
    ),
    (
        "/app",
        &[
            ("open", true, "<name|path> [--new]"),
            ("connect", true, "<name> <port>"),
            ("list", false, "connected apps"),
            ("close", false, "close it"),
        ],
    ),
    (
        "/browser",
        &[
            ("status", false, "what it's connected to"),
            ("open", false, "open it (optionally a url)"),
            ("connect", true, "<port>"),
            ("save", false, "save the recorded steps"),
            ("remember", false, "keep this site's sign-in for next time"),
            ("forget", true, "<site>: drop a remembered sign-in"),
            (
                "test save",
                true,
                "<name>: the saved steps as a smoke test, with a baseline",
            ),
            (
                "test run",
                false,
                "replay the smoke tests and compare screenshots",
            ),
            ("close", false, "close it"),
        ],
    ),
    (
        "/move",
        &[("import", true, "<file>: a session from another computer")],
    ),
    (
        "/files",
        &[("open", false, "the files panel full screen (Esc returns)")],
    ),
    (
        "/dashboard",
        &[
            (
                "left off",
                false,
                "hide the sessions panel (left on: show it)",
            ),
            (
                "right off",
                false,
                "hide the side panel (right on: show it)",
            ),
            ("reset", false, "panel widths and panels back to default"),
        ],
    ),
    (
        "/copy",
        &[
            (
                "code",
                false,
                "the newest code block (`code 2`: the one before)",
            ),
            ("link", false, "the newest link"),
        ],
    ),
    ("/budget", &[("off", false, "no budget (or /budget <usd>)")]),
    (
        "/checkpoint",
        &[
            ("list", false, "the saved checkpoints"),
            ("restore", true, "<n>: put the working tree back"),
            ("drop", true, "<n>: delete one"),
        ],
    ),
    (
        "/plugins",
        &[("enable", true, "<name>"), ("disable", true, "<name>")],
    ),
    (
        "/goal",
        &[
            ("pause", false, "pause the objective"),
            ("resume", false, "resume it"),
            ("clear", false, "drop it"),
        ],
    ),
    (
        "/cost",
        &[
            ("today", false, "today's spend"),
            ("week", false, "this week"),
            ("month", false, "this month"),
        ],
    ),
    ("/replay", &[("--gif", false, "as a GIF too")]),
    (
        "/effort",
        &[
            ("low", false, "quick answers"),
            ("medium", false, "balanced"),
            ("high", false, "think harder"),
            ("xhigh", false, "think much harder"),
            ("max", false, "the most thinking"),
        ],
    ),
    (
        "/permission-bypass",
        &[
            ("on", false, "skip approval prompts"),
            ("off", false, "ask again"),
            ("toggle", false, "switch"),
        ],
    ),
    (
        "/transcript",
        &[
            ("today", false, "today's conversations"),
            ("week", false, "this week"),
            ("month", false, "this month"),
        ],
    ),
    ("/bashes", &[("kill", true, "stop one: <id>")]),
    (
        "/memory",
        &[
            ("forget", true, "remove a note: <id>"),
            ("tidy", false, "show duplicates to drop"),
            ("tidy apply", false, "drop the duplicates"),
            ("auto on", false, "the agent saves notes by itself"),
            ("auto off", false, "only when you ask"),
        ],
    ),
    (
        "/permissions",
        &[
            ("mode", false, "pick the permission mode"),
            ("allow", true, "always allow: <rule> [--user|--project]"),
            ("ask", true, "always ask: <rule> [--user|--project]"),
            ("deny", true, "never allow: <rule> [--user|--project]"),
            ("remove", true, "drop a rule: <rule>"),
            ("default", false, "ask before edits and commands"),
            ("plan", false, "read-only planning"),
            ("accept-edits", false, "edits without asking"),
            ("auto", false, "a model checks each action"),
            ("dont-ask", false, "deny what isn't allowed, never prompt"),
            ("bypass-permissions", false, "no checks at all"),
        ],
    ),
    (
        "/hooks",
        &[
            (
                "add",
                true,
                "<event> [--matcher <tool>] [--blocking] [--user] <command>",
            ),
            ("remove", true, "<event> <n> [--user]"),
        ],
    ),
    (
        "/mcp",
        &[("test", false, "check the servers answer (or `test <name>`)")],
    ),
    (
        "/project-config",
        &[
            ("model", false, "save the current model for the team"),
            ("mode", false, "save the current mode for the team"),
        ],
    ),
    ("/export", &[("copy", false, "to the clipboard")]),
    ("/share", &[("copy", false, "to the clipboard")]),
    ("/side", &[("end", false, "back to the main conversation")]),
    ("/stop", &[("all", false, "every background job")]),
    (
        "/provider",
        &[
            ("anthropic", false, "Claude"),
            ("copilot", false, "GitHub Copilot"),
        ],
    ),
    (
        "/update",
        &[
            ("--check", false, "only check for a new version"),
            ("--force", false, "reinstall even if up to date"),
        ],
    ),
    (
        "/context",
        &[("config", false, "back to the configured context size")],
    ),
];

fn options_of(cmd: &str) -> Option<Options> {
    SUBCOMMANDS
        .iter()
        .find(|(c, _)| c.eq_ignore_ascii_case(cmd))
        .map(|(_, o)| *o)
}

/// Commands listed with their option (`/compact --preview`,
/// `/project add`): (command, option).
fn listed_options() -> impl Iterator<Item = (&'static str, &'static str)> {
    SLASH_COMMANDS.iter().filter_map(|c| c.split_once(' '))
}

/// Whether `cmd` has options to drop down.
fn has_options(cmd: &str) -> bool {
    options_of(cmd).is_some() || listed_options().any(|(c, _)| c.eq_ignore_ascii_case(cmd))
}

/// `/command partial` with a command that has options: (command, partial).
fn option_query(buffer: &str) -> Option<(&str, &str)> {
    let (cmd, rest) = buffer.split_once(' ')?;
    if !cmd.starts_with('/') || !has_options(cmd) {
        return None;
    }
    // One level: "setup voice" is a single option, so allow its space.
    let opts = options_of(cmd).unwrap_or(&[]);
    if rest.contains(' ')
        && !opts
            .iter()
            .any(|o| o.0.starts_with(rest) && o.0.contains(' '))
    {
        return None;
    }
    Some((cmd, rest))
}

/// The slash panel shows while the buffer is a bare `/command` token, or a
/// command with options followed by a space (its options drop down).
pub(crate) fn slash_panel_visible(buffer: &str) -> bool {
    buffer.starts_with('/') && (!buffer.contains(' ') || option_query(buffer).is_some())
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
    /// One option of a command (`/video render`).
    Option {
        command: &'static str,
        option: &'static str,
        /// Needs more text after it (the prompt waits for it).
        more: bool,
        what: &'static str,
    },
    Skill {
        command: String,
        description: Option<String>,
        source: SkillSource,
    },
}

impl SlashEntry {
    /// What picking it puts in the prompt: a command with options ends in a
    /// space (its options drop down), an option needing more text too.
    pub(crate) fn completion(&self) -> String {
        match self {
            SlashEntry::Command(c) if has_options(c) => format!("{c} "),
            SlashEntry::Option { more: true, .. } => format!("{} ", self.command_str()),
            _ => self.command_str(),
        }
    }

    /// Whether picking it should wait for more text instead of running.
    pub(crate) fn needs_more(&self) -> bool {
        matches!(self, SlashEntry::Option { more: true, .. })
    }

    pub(crate) fn command_str(&self) -> String {
        match self {
            SlashEntry::Command(s) => s.to_string(),
            SlashEntry::Option {
                command,
                option: "",
                ..
            } => command.to_string(),
            SlashEntry::Option {
                command, option, ..
            } => format!("{command} {option}"),
            SlashEntry::Skill { command, .. } => format!("/{command}"),
        }
    }

    /// One-line description shown beside the command in the popup (Codex-style).
    pub(crate) fn description_text(&self) -> String {
        match self {
            SlashEntry::Command(s) => {
                crate::slash_commands::slash_command_description(s).to_string()
            }
            SlashEntry::Option { what, .. } => what.to_string(),
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
    // Each command with options: first, the command on its own.
    let mut bases: Vec<&'static str> = SUBCOMMANDS.iter().map(|(c, _)| *c).collect();
    bases.extend(listed_options().map(|(c, _)| c));
    bases.dedup();
    for command in bases {
        if let Some(c) = SLASH_COMMANDS.iter().find(|c| **c == command) {
            entries.push(SlashEntry::Option {
                command: c,
                option: "",
                more: false,
                what: crate::slash_commands::slash_command_description(c),
            });
        }
    }
    // Commands listed with an option become that command's options too.
    for (command, option) in listed_options() {
        if options_of(command).is_none() {
            entries.push(SlashEntry::Option {
                command,
                option,
                more: false,
                what: crate::slash_commands::slash_command_description(&format!(
                    "{command} {option}"
                )),
            });
        }
    }
    // Commands' options, in their own order (shown after `/command `).
    for (command, opts) in SUBCOMMANDS {
        for (option, more, what) in *opts {
            entries.push(SlashEntry::Option {
                command,
                option,
                more: *more,
                what,
            });
        }
    }
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
    // After `/command `: that command's options, in order.
    if let Some((cmd, partial)) = option_query(buffer) {
        let partial = partial.to_lowercase();
        let starts: Vec<&SlashEntry> = entries
            .iter()
            .filter(|e| {
                matches!(e, SlashEntry::Option { command, option, .. }
                    if command.eq_ignore_ascii_case(cmd) && option.starts_with(&partial))
            })
            .collect();
        if !starts.is_empty() || partial.len() < 3 {
            return starts;
        }
        return entries
            .iter()
            .filter(|e| {
                matches!(e, SlashEntry::Option { command, option, .. }
                    if command.eq_ignore_ascii_case(cmd) && option.contains(&partial))
            })
            .collect();
    }
    let needle = buffer.trim_start_matches('/').to_lowercase();
    let recent = recent_commands();
    // Recently used first (when they match), then commands that start
    // with what's typed, then ones that mention it anywhere (name or
    // description) — each by group, then name.
    let mut ranked: Vec<(u8, usize, u8, String, &SlashEntry)> = entries
        .iter()
        .filter(|e| !matches!(e, SlashEntry::Option { .. }))
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
                "/record",
                "/video",
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
    /// Every option the `/` menu offers is one its command's handler knows:
    /// the option's first word appears as a string the handlers match on.
    /// (A renamed or removed subcommand fails here, not in front of a user.)
    #[test]
    fn every_menu_option_is_handled() {
        let handlers = [
            include_str!("../repl.rs"),
            include_str!("../repl_helpers.rs"),
            include_str!("../repl/cmd_choice.rs"),
            include_str!("../repl/cmd_doctor.rs"),
            include_str!("../repl/cmd_ext.rs"),
            include_str!("../repl/cmd_info.rs"),
            include_str!("../repl/cmd_mcp.rs"),
            include_str!("../repl/cmd_session.rs"),
            include_str!("../repl/cmd_settings.rs"),
            include_str!("../repl/cmd_team.rs"),
            include_str!("../repl/cmd_work.rs"),
            include_str!("app.rs"),
            include_str!("app/keys.rs"),
        ]
        .concat();
        let mut missing = Vec::new();
        for (command, options) in SUBCOMMANDS {
            for (option, _, _) in options.iter() {
                // Placeholders (`<n>`) and free-text examples aren't words
                // a handler matches.
                let word = option.split_whitespace().next().unwrap_or("");
                if word.starts_with('<') || word.is_empty() {
                    continue;
                }
                let quoted = format!("\"{word}\"");
                let in_pattern = format!("\"{word}\" |");
                let in_list = format!("| \"{word}\"");
                if !(handlers.contains(&quoted)
                    || handlers.contains(&in_pattern)
                    || handlers.contains(&in_list))
                {
                    missing.push(format!("{command} {option}"));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "menu options no handler knows: {missing:?}"
        );
    }

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

    #[test]
    fn a_commands_options_drop_down_after_a_space() {
        let all = load_slash_entries(std::path::Path::new("/nonexistent"), &[]);
        let names = |v: Vec<&SlashEntry>| v.iter().map(|e| e.command_str()).collect::<Vec<_>>();
        // Options never crowd the root list.
        assert!(
            !names(filter_slash_entries(&all, "/vid"))
                .iter()
                .any(|n| n.contains(' '))
        );
        // `/video ` lists every option, in order.
        let opts = names(filter_slash_entries(&all, "/video "));
        assert_eq!(
            opts.first().map(String::as_str),
            Some("/video"),
            "on its own first"
        );
        assert_eq!(opts.get(1).map(String::as_str), Some("/video list"));
        // Options listed as commands (`/project add`) drop down too.
        let project = names(filter_slash_entries(&all, "/project "));
        assert!(project.contains(&"/project add".to_string()), "{project:?}");
        assert!(opts.contains(&"/video translate".to_string()));
        assert!(opts.len() >= 15, "{opts:?}");
        // Typing narrows them; a two-word option still matches.
        assert_eq!(
            names(filter_slash_entries(&all, "/video tr")),
            ["/video translate"]
        );
        assert_eq!(
            names(filter_slash_entries(&all, "/video setup v")),
            ["/video setup voice"]
        );
        // Picking: a command with options opens them; an option needing
        // text waits for it.
        let video = all.iter().find(|e| e.command_str() == "/video").unwrap();
        assert_eq!(video.completion(), "/video ");
        let tr = all
            .iter()
            .find(|e| e.command_str() == "/video translate")
            .unwrap();
        assert!(tr.needs_more());
        assert_eq!(tr.completion(), "/video translate ");
        // Free text after an option: the panel closes.
        assert!(!slash_panel_visible("/video translate german"));
        assert!(!slash_panel_visible("/help me"), "no options: no panel");
        // Free text after a command with options isn't turned into one.
        assert!(names(filter_slash_entries(&all, "/memory the")).is_empty());
        assert!(slash_panel_visible("/talk "));
    }
}
