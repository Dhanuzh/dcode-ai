//! The categorized command palette: its catalog of rows, label→slash-command
//! mapping, and query filtering. Pure data + string matching, extracted from
//! `tui::app`.

/// A row in the categorized command palette.
#[derive(Clone)]
pub(crate) enum PaletteRow {
    Section(&'static str),
    Entry {
        label: &'static str,
        shortcut: &'static str,
    },
    /// A search hit: a slash command, a recent prompt or a file. Picking it
    /// puts `insert` in the prompt.
    Found {
        label: String,
        detail: String,
        insert: String,
    },
}

/// What the palette searches besides its own entries.
#[derive(Default, Clone)]
pub(crate) struct PaletteSources {
    /// Recent prompts, newest last.
    pub prompts: Vec<String>,
    /// Project files (relative paths).
    pub files: Vec<String>,
}

/// Every character of `needle` in order in `hay` (both lower-case).
fn subsequence(needle: &str, hay: &str) -> bool {
    let mut it = hay.chars();
    needle.chars().all(|c| it.any(|h| h == c))
}

pub(crate) const PALETTE_CATALOG: &[PaletteRow] = &[
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
    PaletteRow::Entry {
        label: "Rename session",
        shortcut: "",
    },
    PaletteRow::Entry {
        label: "Fork session",
        shortcut: "",
    },
    PaletteRow::Entry {
        label: "Delete session",
        shortcut: "",
    },
    PaletteRow::Entry {
        label: "Usage",
        shortcut: "",
    },
    PaletteRow::Section("Code"),
    PaletteRow::Entry {
        label: "Full transcript",
        shortcut: "ctrl+x v",
    },
    PaletteRow::Entry {
        label: "Show diff",
        shortcut: "",
    },
    PaletteRow::Entry {
        label: "Copy last response",
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
    PaletteRow::Entry {
        label: "Personality",
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
    PaletteRow::Section("Projects"),
    PaletteRow::Entry {
        label: "Switch project",
        shortcut: "ctrl+x p",
    },
    PaletteRow::Entry {
        label: "Add project",
        shortcut: "",
    },
    PaletteRow::Entry {
        label: "List projects",
        shortcut: "",
    },
    PaletteRow::Entry {
        label: "Exit",
        shortcut: "ctrl+x q",
    },
];

pub(crate) fn palette_command_for_label(label: &str) -> &'static str {
    match label {
        "Switch model" => "/models",
        "Connect provider" => "/connect",
        "Open editor" => "/editor",
        "Switch session" => "/sessions",
        "New session" => "/new",
        "Compact" => "/compact",
        "Export session" => "/export",
        "Rename session" => "/rename ",
        "Fork session" => "/fork",
        "Delete session" => "/delete",
        "Usage" => "/usage",
        "Skills" => "/skills",
        "Agent profile" => "/agent",
        "Toggle thinking" => "/thinking",
        "Personality" => "/personality",
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
        "Full transcript" => "/transcript",
        "Show diff" => "/diff",
        "Copy last response" => "/copy",
        "Switch project" => "/project switch",
        "Add project" => "/project add ",
        "List projects" => "/project list",
        "Exit" => "/exit",
        _ => "/help",
    }
}

pub(crate) fn filter_palette_rows(query: &str, src: &PaletteSources) -> Vec<PaletteRow> {
    let needle = query.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return PALETTE_CATALOG.to_vec();
    }
    let mut result: Vec<PaletteRow> = Vec::new();
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
                        result.push(s.clone());
                    }
                    result.push(row.clone());
                }
            }
            PaletteRow::Found { .. } => {}
        }
    }
    // Slash commands by name or what they do.
    let commands: Vec<PaletteRow> = crate::slash_commands::SLASH_COMMANDS
        .iter()
        .filter(|c| {
            c.contains(&needle)
                || crate::slash_commands::slash_command_description(c)
                    .to_ascii_lowercase()
                    .contains(&needle)
        })
        .take(8)
        .map(|c| PaletteRow::Found {
            label: c.to_string(),
            detail: crate::slash_commands::slash_command_description(c).to_string(),
            insert: format!("{c} "),
        })
        .collect();
    if !commands.is_empty() {
        result.push(PaletteRow::Section("Commands"));
        result.extend(commands);
    }
    // Recent prompts, newest first, each once.
    let mut seen = std::collections::HashSet::new();
    let prompts: Vec<PaletteRow> = src
        .prompts
        .iter()
        .rev()
        .filter(|p| !p.trim().is_empty() && p.to_ascii_lowercase().contains(&needle))
        .filter(|p| seen.insert(p.trim().to_string()))
        .take(5)
        .map(|p| PaletteRow::Found {
            label: p.lines().next().unwrap_or("").chars().take(70).collect(),
            detail: String::new(),
            insert: p.clone(),
        })
        .collect();
    if !prompts.is_empty() {
        result.push(PaletteRow::Section("Recent prompts"));
        result.extend(prompts);
    }
    // Files, by their path's letters in order (shortest paths first).
    let mut files: Vec<&String> = src
        .files
        .iter()
        .filter(|f| subsequence(&needle.replace(' ', ""), &f.to_ascii_lowercase()))
        .collect();
    files.sort_by_key(|f| (!f.to_ascii_lowercase().contains(&needle), f.len()));
    let files: Vec<PaletteRow> = files
        .into_iter()
        .take(8)
        .map(|f| PaletteRow::Found {
            label: f.clone(),
            detail: String::new(),
            insert: format!("@{f} "),
        })
        .collect();
    if !files.is_empty() {
        result.push(PaletteRow::Section("Files"));
        result.extend(files);
    }
    result
}

pub(crate) fn palette_selectable_indices(rows: &[PaletteRow]) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter_map(|(i, r)| {
            matches!(r, PaletteRow::Entry { .. } | PaletteRow::Found { .. }).then_some(i)
        })
        .collect()
}

#[cfg(test)]
mod search_tests {
    use super::*;

    #[test]
    fn the_palette_finds_commands_prompts_and_files() {
        let src = PaletteSources {
            prompts: vec!["fix the login bug".into(), "add tests".into()],
            files: vec!["crates/cli/src/tui/palette.rs".into(), "README.md".into()],
        };
        let rows = filter_palette_rows("login", &src);
        assert!(rows.iter().any(
            |r| matches!(r, PaletteRow::Found { insert, .. } if insert == "fix the login bug")
        ));
        let rows = filter_palette_rows("pal", &src);
        assert!(rows.iter().any(|r| matches!(r, PaletteRow::Found { insert, .. } if insert == "@crates/cli/src/tui/palette.rs ")));
        let rows = filter_palette_rows("checkpoint", &src);
        assert!(
            rows.iter()
                .any(|r| matches!(r, PaletteRow::Found { label, .. } if label == "/checkpoint"))
        );
        // Nothing typed: just the palette's own entries.
        assert_eq!(filter_palette_rows("", &src).len(), PALETTE_CATALOG.len());
    }
}
