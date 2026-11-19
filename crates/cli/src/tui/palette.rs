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
    /// A search hit: a slash command, a recent prompt, a file, a session, a
    /// checkpoint, a video, a setting or a line of text. Picking it runs
    /// `insert` when `run`, else puts it in the prompt.
    Found {
        label: String,
        detail: String,
        insert: String,
        run: bool,
    },
}

impl PaletteRow {
    fn found(
        label: impl Into<String>,
        detail: impl Into<String>,
        insert: impl Into<String>,
        run: bool,
    ) -> Self {
        PaletteRow::Found {
            label: label.into(),
            detail: detail.into(),
            insert: insert.into(),
            run,
        }
    }
}

/// What the palette searches besides its own entries (gathered when it
/// opens; text hits as you type `#…`).
#[derive(Default, Clone)]
pub(crate) struct PaletteSources {
    /// Recent prompts, newest last.
    pub prompts: Vec<String>,
    /// Project files (relative paths).
    pub files: Vec<String>,
    /// Recent sessions: (id, what it was about, when).
    pub sessions: Vec<(String, String, String)>,
    /// Checkpoints: (number, name, when).
    pub checkpoints: Vec<(usize, String, String)>,
    /// Recordings: (id, title).
    pub videos: Vec<(String, String)>,
    /// Settings: (name, current value, command, run it).
    pub settings: Vec<(String, String, String, bool)>,
    /// Lines found by `#text`: (file, line number, the line).
    pub text_hits: Vec<(String, usize, String)>,
    /// How often and how lately each pick was used: insert → (count, unix secs).
    pub frecency: std::collections::HashMap<String, (u32, i64)>,
    /// This conversation's messages: (block index, yours, text).
    pub convo: Vec<(usize, bool, String)>,
}

/// A pick that jumps to message `n` of this conversation is `JUMP` + n.
pub(crate) const JUMP: &str = "\u{1}jump:";

/// The message block a pick jumps to, if it is one.
pub(crate) fn jump_target(insert: &str) -> Option<usize> {
    insert.strip_prefix(JUMP)?.parse().ok()
}

/// `text` around the first match of `needle` (lowercase), on one line.
fn snippet(text: &str, needle: &str, width: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    let lower = flat.to_lowercase();
    let at_byte = lower.find(needle).unwrap_or(0);
    let at = lower[..at_byte].chars().count();
    let start = at.saturating_sub(width / 3);
    let mut s: String = flat.chars().skip(start).take(width).collect();
    if start > 0 {
        s.insert(0, '…');
    }
    if flat.chars().count() > start + width {
        s.push('…');
    }
    s
}

/// A pick's rank from use: often and recent first (a week halves it).
fn frecency_score(src: &PaletteSources, key: &str) -> f64 {
    let Some((n, at)) = src.frecency.get(key) else {
        return 0.0;
    };
    let days = ((chrono::Utc::now().timestamp() - at).max(0) as f64) / 86_400.0;
    *n as f64 / (1.0 + days / 7.0)
}

fn frecency_file() -> Option<std::path::PathBuf> {
    dcode_ai_common::config::dcode_ai_home_dir().map(|d| d.join("palette.json"))
}

pub(crate) fn load_frecency() -> std::collections::HashMap<String, (u32, i64)> {
    frecency_file()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Remembers a pick (for ranking next time).
pub(crate) fn record_pick(key: &str) {
    let mut f = load_frecency();
    let e = f.entry(key.to_string()).or_insert((0, 0));
    e.0 = e.0.saturating_add(1);
    e.1 = chrono::Utc::now().timestamp();
    // Keep it small: the 300 best.
    if f.len() > 300 {
        let mut v: Vec<(String, (u32, i64))> = f.into_iter().collect();
        v.sort_by_key(|(_, (n, at))| std::cmp::Reverse((*n as i64) * 86_400 + at / 7));
        v.truncate(300);
        f = v.into_iter().collect();
    }
    if let Some(file) = frecency_file() {
        let _ = std::fs::write(file, serde_json::to_string(&f).unwrap_or_default());
    }
}

/// Whether a command does nothing useful without more words after it
/// (`/pin <fact>`): it goes to the prompt instead of running.
fn needs_words(cmd: &str) -> bool {
    let d = crate::slash_commands::slash_command_description(cmd);
    d.contains(&format!("{cmd} <")) || d.starts_with('<')
}

/// Recent sessions, checkpoints, recordings and settings for the palette.
/// Quick: file names, the first lines of logs, one git call.
pub(crate) fn gather_sources(
    workspace: &std::path::Path,
    prompts: Vec<String>,
    files: Vec<String>,
    model: &str,
    permission_mode: &str,
) -> PaletteSources {
    use std::io::Read;
    let ago = |t: std::time::SystemTime| {
        let s = t.elapsed().map(|d| d.as_secs()).unwrap_or(0);
        match s {
            s if s < 3600 => format!("{}m ago", s / 60),
            s if s < 86_400 => format!("{}h ago", s / 3600),
            s => format!("{}d ago", s / 86_400),
        }
    };
    // Sessions: newest 30 logs, labelled with their first request.
    let dir = workspace.join(".dcode-ai/sessions");
    let mut logs: Vec<(std::time::SystemTime, std::path::PathBuf)> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            let name = p.file_name()?.to_str()?.to_string();
            name.ends_with(".events.jsonl")
                .then(|| Some((e.metadata().ok()?.modified().ok()?, p)))?
        })
        .collect();
    logs.sort_by_key(|x| std::cmp::Reverse(x.0));
    let sessions = logs
        .into_iter()
        .take(30)
        .filter_map(|(at, p)| {
            let id = p
                .file_name()?
                .to_str()?
                .strip_suffix(".events.jsonl")?
                .to_string();
            let mut head = vec![0u8; 64 * 1024];
            let n = std::fs::File::open(&p).ok()?.read(&mut head).ok()?;
            let text = String::from_utf8_lossy(&head[..n]).into_owned();
            let first = text.lines().find_map(|l| {
                let v: serde_json::Value = serde_json::from_str(l).ok()?;
                let e = &v["event"];
                (e["type"] == "MessageReceived" && e["role"] == "user")
                    .then(|| e["content"].as_str().map(str::to_string))?
            })?;
            let label: String = first
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(70)
                .collect();
            Some((id, label, ago(at)))
        })
        .collect();
    // Checkpoints (one git call; none outside git).
    let checkpoints = crate::checkpoints::list(workspace)
        .map(|v| v.into_iter().rev().map(|c| (c.n, c.name, c.when)).collect())
        .unwrap_or_default();
    // Recordings.
    let videos = dcode_ai_core::video::record::list(workspace)
        .into_iter()
        .take(20)
        .map(|(id, dir)| {
            let title = std::fs::read_to_string(dir.join("edl.json"))
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .and_then(|v| v["title"].as_str().map(str::to_string))
                .unwrap_or_default();
            (id, title)
        })
        .collect();
    let persona = dcode_ai_common::config::dcode_ai_home_dir()
        .and_then(|d| std::fs::read_to_string(d.join("voice-persona")).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "none".into());
    let settings = vec![
        ("Model".into(), model.to_string(), "/models".into(), true),
        (
            "Permission mode".into(),
            permission_mode.to_string(),
            "/permissions ".into(),
            false,
        ),
        ("Effort".into(), String::new(), "/effort ".into(), false),
        ("Theme".into(), String::new(), "/theme".into(), true),
        (
            "Talk voice".into(),
            crate::tui::notify::talk_voice(),
            "/talk voice".into(),
            true,
        ),
        (
            "Talk persona".into(),
            persona,
            "/talk persona ".into(),
            false,
        ),
        ("Cost budget".into(), String::new(), "/budget".into(), true),
        ("Pinned facts".into(), String::new(), "/pins".into(), true),
        (
            "Video voice".into(),
            String::new(),
            "/video voices".into(),
            true,
        ),
        ("Sounds".into(), String::new(), "/sounds ".into(), false),
    ];
    PaletteSources {
        prompts,
        files,
        sessions,
        checkpoints,
        videos,
        settings,
        text_hits: Vec::new(),
        frecency: load_frecency(),
        convo: Vec::new(),
    }
}

/// Lines in the project's files containing `needle` (case-insensitive;
/// `.gitignore` respected; at most 12, small text files only).
pub(crate) fn search_text(
    workspace: &std::path::Path,
    needle: &str,
) -> Vec<(String, usize, String)> {
    let needle = needle.trim().to_lowercase();
    if needle.chars().count() < 3 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let started = std::time::Instant::now();
    for e in ignore::WalkBuilder::new(workspace).build().flatten() {
        if out.len() >= 12 || started.elapsed() > std::time::Duration::from_millis(400) {
            break;
        }
        let p = e.path();
        if !p.is_file() || p.metadata().map(|m| m.len() > 512 * 1024).unwrap_or(true) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(p) else {
            continue;
        };
        let rel = p.strip_prefix(workspace).unwrap_or(p).display().to_string();
        for (i, line) in text.lines().enumerate() {
            if line.to_lowercase().contains(&needle) {
                out.push((rel.clone(), i + 1, line.trim().chars().take(90).collect()));
                if out.len() >= 12 {
                    break;
                }
            }
        }
    }
    out
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
    // Prefix modes, as in editors: > commands, @ files, # text in files,
    // : settings, ! history (sessions, checkpoints, videos).
    let q = query.trim_start();
    let (mode, rest) = match q.chars().next() {
        Some(c @ ('>' | '@' | '#' | ':' | '!' | '?')) => (Some(c), &q[c.len_utf8()..]),
        _ => (None, q),
    };
    let needle = rest.trim().to_ascii_lowercase();
    let want = |m: char| mode.is_none() || mode == Some(m);
    let mut result: Vec<PaletteRow> = Vec::new();
    let section = |name: &'static str, mut rows: Vec<PaletteRow>, result: &mut Vec<PaletteRow>| {
        if rows.is_empty() {
            return;
        }
        // Often and lately used first (a stable sort keeps match order).
        rows.sort_by(|a, b| {
            let k = |r: &PaletteRow| match r {
                PaletteRow::Found { insert, .. } => frecency_score(src, insert),
                _ => 0.0,
            };
            k(b).total_cmp(&k(a))
        });
        result.push(PaletteRow::Section(name));
        result.extend(rows);
    };
    if needle.is_empty() && mode.is_none() {
        // Nothing typed: what you picked lately, then the palette's own.
        let mut recent: Vec<(&String, f64)> = src
            .frecency
            .keys()
            .map(|k| (k, frecency_score(src, k)))
            .collect();
        recent.sort_by(|a, b| b.1.total_cmp(&a.1));
        let rows: Vec<PaletteRow> = recent
            .into_iter()
            .take(5)
            .map(|(k, _)| {
                let cmd = k.split_whitespace().next().unwrap_or("");
                let run = k.starts_with('/') && !k.ends_with(' ') && !needs_words(cmd);
                PaletteRow::found(k.trim().to_string(), "recent", k.clone(), run)
            })
            .collect();
        result.push(PaletteRow::Section(
            "type to search · > commands  @ files  # text  : settings  ! sessions, checkpoints, videos  ? this conversation",
        ));
        section("Recent", rows, &mut result);
        result.extend(PALETTE_CATALOG.iter().cloned());
        return result;
    }
    if mode.is_none() {
        let mut pending_section: Option<&'static PaletteRow> = None;
        for row in PALETTE_CATALOG {
            match row {
                PaletteRow::Section(_) => pending_section = Some(row),
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
    }
    if want('>') {
        let mut matched: Vec<&&str> = crate::slash_commands::SLASH_COMMANDS
            .iter()
            .filter(|c| {
                needle.is_empty()
                    || c.contains(&needle)
                    || crate::slash_commands::slash_command_description(c)
                        .to_ascii_lowercase()
                        .contains(&needle)
            })
            .collect();
        matched.sort_by_key(|c| (!c.contains(&needle), !c.starts_with(&format!("/{needle}"))));
        let rows = matched
            .into_iter()
            .take(if mode == Some('>') { 30 } else { 8 })
            .map(|c| {
                let run = !needs_words(c);
                PaletteRow::found(
                    c.to_string(),
                    crate::slash_commands::slash_command_description(c),
                    if run { c.to_string() } else { format!("{c} ") },
                    run,
                )
            })
            .collect();
        section("Commands", rows, &mut result);
    }
    if want(':') {
        let rows = src
            .settings
            .iter()
            .filter(|(n, v, c, _)| {
                needle.is_empty()
                    || n.to_ascii_lowercase().contains(&needle)
                    || v.to_ascii_lowercase().contains(&needle)
                    || c.contains(&needle)
            })
            .map(|(n, v, c, run)| PaletteRow::found(n.clone(), v.clone(), c.clone(), *run))
            .collect();
        section("Settings", rows, &mut result);
    }
    if want('!') {
        let hit = |s: &str| needle.is_empty() || s.to_ascii_lowercase().contains(&needle);
        let rows = src
            .sessions
            .iter()
            .filter(|(id, l, _)| hit(l) || hit(id))
            .take(if mode == Some('!') { 12 } else { 4 })
            .map(|(id, l, when)| {
                PaletteRow::found(
                    l.clone(),
                    format!("session · {when}"),
                    format!("/resume {id}"),
                    true,
                )
            })
            .collect();
        section("Sessions", rows, &mut result);
        let rows = src
            .checkpoints
            .iter()
            .filter(|(_, n, _)| hit(n))
            .take(6)
            .map(|(n, name, when)| {
                // A restore goes to the prompt first: it changes your files.
                PaletteRow::found(
                    format!("↺ {name}"),
                    format!("checkpoint {n} · {when}"),
                    format!("/checkpoint restore {n}"),
                    false,
                )
            })
            .collect();
        section("Checkpoints", rows, &mut result);
        let rows = src
            .videos
            .iter()
            .filter(|(id, t)| hit(id) || hit(t))
            .take(6)
            .map(|(id, t)| {
                PaletteRow::found(
                    if t.is_empty() { id.clone() } else { t.clone() },
                    format!("video {id} · Enter opens it"),
                    format!("/video open {id}"),
                    true,
                )
            })
            .collect();
        section("Videos", rows, &mut result);
    }
    // This conversation: messages that mention it, newest first; Enter
    // jumps there in the transcript.
    if (mode == Some('?') && !needle.is_empty()) || (mode.is_none() && needle.len() >= 3) {
        let rows: Vec<PaletteRow> = src
            .convo
            .iter()
            .rev()
            .filter(|(_, _, t)| t.to_lowercase().contains(&needle))
            .take(if mode == Some('?') { 20 } else { 3 })
            .map(|(i, yours, t)| {
                PaletteRow::found(
                    snippet(t, &needle, 70),
                    if *yours { "you said" } else { "reply" },
                    format!("{JUMP}{i}"),
                    true,
                )
            })
            .collect();
        section("In this conversation", rows, &mut result);
    }
    if mode.is_none() && !needle.is_empty() {
        // Recent prompts, newest first, each once.
        let mut seen = std::collections::HashSet::new();
        let rows = src
            .prompts
            .iter()
            .rev()
            .filter(|p| !p.trim().is_empty() && p.to_ascii_lowercase().contains(&needle))
            .filter(|p| seen.insert(p.trim().to_string()))
            .take(5)
            .map(|p| {
                PaletteRow::found(
                    p.lines()
                        .next()
                        .unwrap_or("")
                        .chars()
                        .take(70)
                        .collect::<String>(),
                    "",
                    p.clone(),
                    false,
                )
            })
            .collect();
        section("Recent prompts", rows, &mut result);
    }
    if want('@') && !(mode.is_none() && needle.is_empty()) {
        // Files, by their path's letters in order (shortest paths first).
        let mut files: Vec<&String> = src
            .files
            .iter()
            .filter(|f| subsequence(&needle.replace(' ', ""), &f.to_ascii_lowercase()))
            .collect();
        files.sort_by_key(|f| (!f.to_ascii_lowercase().contains(&needle), f.len()));
        let rows = files
            .into_iter()
            .take(if mode == Some('@') { 20 } else { 8 })
            .map(|f| PaletteRow::found(f.clone(), "", format!("@{f} "), false))
            .collect();
        section("Files", rows, &mut result);
    }
    if mode == Some('#') {
        let rows = src
            .text_hits
            .iter()
            .map(|(f, n, line)| {
                PaletteRow::found(format!("{f}:{n}"), line.clone(), format!("@{f} "), false)
            })
            .collect();
        section("Text in files", rows, &mut result);
        if needle.chars().count() < 3 && result.is_empty() {
            result.push(PaletteRow::Section("type 3+ letters to search the files"));
        }
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
    fn question_mark_searches_this_conversation_and_jumps() {
        let src = PaletteSources {
            convo: vec![
                (0, true, "why does the login fail?".into()),
                (
                    3,
                    false,
                    "The token expiry check used < instead of <=.".into(),
                ),
                (7, true, "thanks".into()),
            ],
            ..Default::default()
        };
        let rows = filter_palette_rows("?expiry", &src);
        let hits: Vec<(&String, &String)> = rows
            .iter()
            .filter_map(|r| match r {
                PaletteRow::Found { label, insert, .. } => Some((label, insert)),
                _ => None,
            })
            .collect();
        assert_eq!(hits.len(), 1);
        assert!(hits[0].0.contains("expiry check"));
        assert_eq!(jump_target(hits[0].1), Some(3));
        // Without the prefix a longer query also finds messages.
        let rows = filter_palette_rows("login", &src);
        assert!(
            rows.iter()
                .any(|r| matches!(r, PaletteRow::Section("In this conversation")))
        );
        assert_eq!(jump_target("/resume x"), None);
        assert_eq!(snippet("aaaa needle bbbb", "needle", 8), "…a needle…");
    }

    #[test]
    fn the_palette_finds_commands_prompts_and_files() {
        let src = PaletteSources {
            prompts: vec!["fix the login bug".into(), "add tests".into()],
            files: vec!["crates/cli/src/tui/palette.rs".into(), "README.md".into()],
            sessions: vec![(
                "session-1".into(),
                "fix the payment webhook".into(),
                "2h ago".into(),
            )],
            checkpoints: vec![(3, "before payments".into(), "1h ago".into())],
            videos: vec![("rec-1".into(), "Tour of the shop".into())],
            settings: vec![(
                "Model".into(),
                "claude-opus-5-5".into(),
                "/models".into(),
                true,
            )],
            ..Default::default()
        };
        let rows = filter_palette_rows("login", &src);
        assert!(rows.iter().any(
            |r| matches!(r, PaletteRow::Found { insert, .. } if insert == "fix the login bug")
        ));
        let rows = filter_palette_rows("pal", &src);
        assert!(rows.iter().any(|r| matches!(r, PaletteRow::Found { insert, .. } if insert == "@crates/cli/src/tui/palette.rs ")));
        // A name match comes before a description match.
        let rows = filter_palette_rows("check", &src);
        let first = rows.iter().find_map(|r| match r {
            PaletteRow::Found { label, .. } if label.starts_with('/') => Some(label.clone()),
            _ => None,
        });
        assert_eq!(first.as_deref(), Some("/checkpoint"));
        let rows = filter_palette_rows("checkpoint", &src);
        assert!(
            rows.iter()
                .any(|r| matches!(r, PaletteRow::Found { label, .. } if label == "/checkpoint"))
        );
        // Nothing typed (nothing picked yet): the hint, then the palette's own
        // entries.
        assert_eq!(
            filter_palette_rows("", &src).len(),
            PALETTE_CATALOG.len() + 1
        );
        let found = |q: &str| -> Vec<(String, String, bool)> {
            filter_palette_rows(q, &src)
                .into_iter()
                .filter_map(|r| match r {
                    PaletteRow::Found {
                        label, insert, run, ..
                    } => Some((label, insert, run)),
                    _ => None,
                })
                .collect()
        };
        // Commands that need nothing more run when picked; ones that do go
        // to the prompt.
        assert!(
            found(">recap")
                .iter()
                .any(|(l, i, run)| l == "/recap" && i == "/recap" && *run)
        );
        assert!(
            found(">pin")
                .iter()
                .any(|(l, i, run)| l == "/pin" && i == "/pin " && !*run)
        );
        // Prefix modes keep to their kind.
        assert!(found("@pal").iter().all(|(_, i, _)| i.starts_with('@')));
        assert!(
            found(":model")
                .iter()
                .any(|(l, i, run)| l == "Model" && i == "/models" && *run)
        );
        let hist = found("!pay");
        assert!(
            hist.iter()
                .any(|(_, i, run)| i == "/resume session-1" && *run)
        );
        assert!(
            hist.iter()
                .any(|(_, i, run)| i == "/checkpoint restore 3" && !*run)
        );
        assert!(
            found("!tour")
                .iter()
                .any(|(_, i, _)| i == "/video open rec-1")
        );
    }
}
