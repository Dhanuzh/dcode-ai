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

pub(crate) fn filter_palette_rows(query: &str) -> Vec<&'static PaletteRow> {
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

pub(crate) fn palette_selectable_indices(rows: &[&PaletteRow]) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter_map(|(i, r)| matches!(r, PaletteRow::Entry { .. }).then_some(i))
        .collect()
}
