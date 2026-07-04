use crate::tui::state::{ModelPickerAction, ModelPickerEntry};
use dcode_ai_common::auth::AuthStore;
use dcode_ai_common::config::{DcodeAiConfig, PermissionMode, ProviderKind};
use dcode_ai_common::provider_runtime::has_claude_cli;

pub(crate) fn provider_label(
    config: &dcode_ai_common::config::DcodeAiConfig,
    p: ProviderKind,
) -> String {
    if p == ProviderKind::OpenAi
        && config
            .provider
            .openai
            .base_url
            .to_ascii_lowercase()
            .contains("githubcopilot.com")
    {
        "OpenAI (Copilot endpoint)".to_string()
    } else {
        p.display_name().to_string()
    }
}

pub(crate) fn is_copilot_surface(config: &dcode_ai_common::config::DcodeAiConfig) -> bool {
    config
        .provider
        .openai
        .base_url
        .to_ascii_lowercase()
        .contains("githubcopilot.com")
}

pub(crate) fn active_provider_connected(
    config: &dcode_ai_common::config::DcodeAiConfig,
    auth: &dcode_ai_common::auth::AuthStore,
) -> bool {
    match config.provider.default {
        ProviderKind::OpenAi => {
            if is_copilot_surface(config) {
                auth.copilot.is_some()
            } else {
                config.provider.api_key_present_for(ProviderKind::OpenAi)
                    || auth.openai_oauth.is_some()
            }
        }
        ProviderKind::Anthropic => {
            config.provider.api_key_present_for(ProviderKind::Anthropic)
                || auth.anthropic.is_some()
                || has_claude_cli()
        }
        ProviderKind::OpenRouter => config
            .provider
            .api_key_present_for(ProviderKind::OpenRouter),
        ProviderKind::Antigravity => {
            config
                .provider
                .api_key_present_for(ProviderKind::Antigravity)
                || auth.antigravity.is_some()
        }
        ProviderKind::OpenCodeZen => {
            config
                .provider
                .api_key_present_for(ProviderKind::OpenCodeZen)
                || auth.opencodezen_oauth.is_some()
        }
    }
}

pub(crate) fn active_surface_label(config: &dcode_ai_common::config::DcodeAiConfig) -> String {
    if config.provider.default == ProviderKind::OpenAi && is_copilot_surface(config) {
        "Copilot".to_string()
    } else {
        provider_label(config, config.provider.default)
    }
}

pub(crate) fn build_model_picker_entries(
    config: &dcode_ai_common::config::DcodeAiConfig,
    provider_models: &[String],
) -> Vec<ModelPickerEntry> {
    let auth = dcode_ai_common::auth::AuthStore::load().unwrap_or_default();
    let mut entries = Vec::new();
    entries.push(ModelPickerEntry {
        label: "Providers".into(),
        detail: String::new(),
        action: ModelPickerAction::ApplyModel(String::new()),
        is_header: true,
    });
    for p in ProviderKind::ALL {
        let model = config.provider.model_for(p);
        let key_status = if config.provider.api_key_present_for(p) {
            "key ✓"
        } else if p == ProviderKind::Anthropic && has_claude_cli() {
            "cli ✓"
        } else if (p == ProviderKind::OpenAi && auth.openai_oauth.is_some())
            || (p == ProviderKind::Anthropic && auth.anthropic.is_some())
            || (p == ProviderKind::Antigravity && auth.antigravity.is_some())
            || (p == ProviderKind::OpenCodeZen && auth.opencodezen_oauth.is_some())
        {
            "oauth ✓"
        } else {
            "not connected"
        };
        let selected = if p == config.provider.default && !is_copilot_surface(config) {
            " [active]"
        } else {
            ""
        };
        entries.push(ModelPickerEntry {
            label: format!("{}{}", provider_label(config, p), selected),
            detail: format!("{model} ({key_status})"),
            action: ModelPickerAction::SwitchProvider(p),
            is_header: false,
        });
    }
    let copilot_active = config.provider.default == ProviderKind::OpenAi
        && config
            .provider
            .openai
            .base_url
            .to_ascii_lowercase()
            .contains("githubcopilot.com");
    let copilot_selected = if copilot_active { " [active]" } else { "" };
    let copilot_status = if auth.copilot.is_some() {
        "oauth ✓"
    } else {
        "not logged in"
    };
    entries.push(ModelPickerEntry {
        label: format!("Copilot{}", copilot_selected),
        detail: if copilot_active {
            format!("{} ({copilot_status})", config.provider.openai.model)
        } else {
            format!("separate model list ({copilot_status})")
        },
        action: ModelPickerAction::SwitchCopilot,
        is_header: false,
    });

    let active_connected = active_provider_connected(config, &auth);
    entries.push(ModelPickerEntry {
        label: format!("{} models", active_surface_label(config)),
        detail: String::new(),
        action: ModelPickerAction::ApplyModel(String::new()),
        is_header: true,
    });
    if provider_models.is_empty() {
        let fallback = if active_connected {
            "No models available for active provider."
        } else {
            "No provider connected. Run /connect or /login."
        };
        entries.push(ModelPickerEntry {
            label: fallback.to_string(),
            detail: String::new(),
            action: ModelPickerAction::ApplyModel(String::new()),
            is_header: true,
        });
    } else {
        for model_id in provider_models {
            entries.push(ModelPickerEntry {
                label: model_id.clone(),
                detail: String::new(),
                action: ModelPickerAction::ApplyModel(model_id.clone()),
                is_header: false,
            });
        }
    }

    entries
}

pub(crate) fn permission_mode_index(mode: PermissionMode) -> usize {
    match mode {
        PermissionMode::Default => 0,
        PermissionMode::Plan => 1,
        PermissionMode::AcceptEdits => 2,
        PermissionMode::DontAsk => 3,
        PermissionMode::BypassPermissions => 4,
    }
}

pub(crate) fn permission_mode_from_index(idx: usize) -> PermissionMode {
    match idx {
        0 => PermissionMode::Default,
        1 => PermissionMode::Plan,
        2 => PermissionMode::AcceptEdits,
        3 => PermissionMode::DontAsk,
        4 => PermissionMode::BypassPermissions,
        _ => PermissionMode::Default,
    }
}

pub(crate) fn parse_permission_mode(raw: &str) -> Option<PermissionMode> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "default" => Some(PermissionMode::Default),
        "plan" => Some(PermissionMode::Plan),
        "accept-edits" | "accept_edits" | "acceptedits" => Some(PermissionMode::AcceptEdits),
        "dont-ask" | "dont_ask" | "dontask" => Some(PermissionMode::DontAsk),
        "bypass-permissions" | "bypass_permissions" | "bypasspermissions" => {
            Some(PermissionMode::BypassPermissions)
        }
        _ => None,
    }
}

// ── /import helper: import a Claude Code chat transcript ──────────────────────

/// Locate `~/.claude` (Claude Code's home), honoring HOME then USERPROFILE.
pub(crate) fn claude_home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|h| std::path::PathBuf::from(h).join(".claude"))
}

/// Encode a workspace path the way Claude Code names its project dirs: path
/// separators and the drive colon become `-` (e.g. `D:\a\b` → `D--a-b`).
pub(crate) fn encode_claude_project_dir(path: &std::path::Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' => '-',
            other => other,
        })
        .collect()
}

/// Pull the readable text out of a Claude Code `message` object (content may be
/// a plain string or an array of parts; only `text` parts are kept).
pub(crate) fn claude_message_text(msg: Option<&serde_json::Value>) -> String {
    let Some(msg) = msg else { return String::new() };
    match msg.get("content") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| {
                if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                    p.get("text").and_then(|t| t.as_str()).map(str::to_string)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// Import the most recent Claude Code chat for `workspace_root`, returning
/// `(title, transcript_text)`. Skips images/thinking/tool calls; keeps the
/// user/assistant text exchange, capped to keep context manageable.
pub(crate) fn import_latest_claude_chat(
    workspace_root: &std::path::Path,
) -> Result<(String, String), String> {
    let home = claude_home_dir().ok_or("could not locate ~/.claude")?;
    let dir = home
        .join("projects")
        .join(encode_claude_project_dir(workspace_root));
    if !dir.is_dir() {
        return Err(format!(
            "no Claude Code chats found for this workspace ({})",
            dir.display()
        ));
    }
    // Newest .jsonl by modified time.
    let mut sessions: Vec<(std::time::SystemTime, std::path::PathBuf)> = std::fs::read_dir(&dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("jsonl"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    sessions.sort_by_key(|(t, _)| *t);
    let (_, path) = sessions.pop().ok_or("no chat sessions in this workspace")?;

    let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    const MAX_CHARS: usize = 16_384;
    let mut title = String::new();
    let mut out = String::new();
    for line in content.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        match v.get("type").and_then(|t| t.as_str()) {
            Some("ai-title") => {
                if let Some(t) = v.get("aiTitle").and_then(|t| t.as_str()) {
                    title = t.to_string();
                }
            }
            Some(role @ ("user" | "assistant")) => {
                let text = claude_message_text(v.get("message"));
                if text.trim().is_empty() {
                    continue;
                }
                let who = if role == "user" { "User" } else { "Assistant" };
                out.push_str(&format!("{who}: {}\n\n", text.trim()));
                if out.len() > MAX_CHARS {
                    out.push_str("[… transcript truncated]");
                    break;
                }
            }
            _ => {}
        }
    }
    if out.trim().is_empty() {
        return Err("the latest chat had no importable text messages".into());
    }
    if title.is_empty() {
        title = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("chat")
            .to_string();
    }
    Ok((title, out))
}

// ── /ide helpers: read editor context bridge + git working set ────────────────

/// Format IDE context JSON (written by an editor bridge to
/// `.dcode/ide-context.json`) into a readable context block. Recognized fields:
/// `active_file`, `cursor.line`, `open_files` (array), `selection`. Returns
/// `None` when the JSON is unusable or has nothing useful.
pub(crate) fn format_ide_context(json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let mut out = String::from("Current IDE context:\n");
    let mut has_content = false;
    if let Some(f) = v.get("active_file").and_then(|x| x.as_str()) {
        out.push_str(&format!("Active file: {f}"));
        if let Some(line) = v
            .get("cursor")
            .and_then(|c| c.get("line"))
            .and_then(|l| l.as_u64())
        {
            out.push_str(&format!(" (line {line})"));
        }
        out.push('\n');
        has_content = true;
    }
    if let Some(files) = v.get("open_files").and_then(|x| x.as_array()) {
        let names: Vec<String> = files
            .iter()
            .filter_map(|f| f.as_str().map(String::from))
            .collect();
        if !names.is_empty() {
            out.push_str(&format!("Open files: {}\n", names.join(", ")));
            has_content = true;
        }
    }
    if let Some(sel) = v
        .get("selection")
        .and_then(|x| x.as_str())
        .filter(|s| !s.trim().is_empty())
    {
        out.push_str(&format!("\nSelected code:\n```\n{sel}\n```\n"));
        has_content = true;
    }
    has_content.then_some(out)
}

/// The workspace's git working set (paths from `git status --porcelain`),
/// used as a fallback "what you're working on" when no editor bridge exists.
pub(crate) fn git_changed_files(ws: &std::path::Path) -> Vec<String> {
    match std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(ws)
        .output()
    {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .lines()
            .filter_map(|l| l.get(3..).map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect(),
        Err(_) => Vec::new(),
    }
}

// ── /web helper: fetch URL and return plain text ──────────────────────────────

/// Fetch a URL and return its content as plain text (HTML tags stripped).
/// Truncates to ~16 KiB to keep context sizes manageable.
pub(crate) async fn fetch_url_as_text(url: &str) -> anyhow::Result<String> {
    use reqwest::header;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .user_agent("dcode-ai/1.0 (URL context fetcher)")
        .build()?;
    let resp = client.get(url).send().await?;
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();
    let body = resp.text().await?;
    const MAX_BYTES: usize = 16_384;
    let text = if content_type.contains("html") {
        strip_html_tags(&body)
    } else {
        body
    };
    if text.len() > MAX_BYTES {
        let mut end = MAX_BYTES;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        Ok(format!("{}\n[…content truncated at 16 KiB]", &text[..end]))
    } else {
        Ok(text)
    }
}

/// Very light HTML→text: remove tags, decode common entities, collapse whitespace.
pub(crate) fn strip_html_tags(html: &str) -> String {
    // Remove <script> and <style> blocks first.
    let mut s = html.to_string();
    for tag in &["script", "style", "head"] {
        loop {
            let open = format!("<{tag}");
            let close = format!("</{tag}>");
            if let Some(start) = s.to_ascii_lowercase().find(&open) {
                if let Some(end) = s[start..].to_ascii_lowercase().find(&close) {
                    s.drain(start..start + end + close.len());
                } else {
                    break;
                }
            } else {
                break;
            }
        }
    }
    // Strip remaining tags.
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    // Decode common HTML entities.
    let out = out
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&nbsp;", " ");
    // Collapse runs of blank lines and trim.
    let lines: Vec<&str> = out.lines().collect();
    let mut result = String::new();
    let mut prev_blank = false;
    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !prev_blank {
                result.push('\n');
            }
            prev_blank = true;
        } else {
            result.push_str(trimmed);
            result.push('\n');
            prev_blank = false;
        }
    }
    result.trim().to_string()
}

/// Truncate to at most `max_bytes` UTF-8 bytes (on a char boundary).
pub(crate) fn truncate_str_bytes(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

// ── /map helper: build an indented file tree ──────────────────────────────────

/// Build a compact indented tree from a flat list of `/`-separated relative paths.
/// Limits output to 200 entries to avoid flooding the transcript.
pub(crate) fn build_file_tree(paths: &[String]) -> String {
    use std::collections::BTreeMap;

    // Represent the tree as nested BTreeMaps for sorted output.
    #[derive(Default)]
    struct Dir {
        children: BTreeMap<String, Dir>,
        files: Vec<String>,
    }

    let mut root = Dir::default();

    for path in paths.iter().take(2000) {
        let parts: Vec<&str> = path.split('/').collect();
        let mut cur = &mut root;
        if let Some((file, dirs)) = parts.split_last() {
            for dir in dirs {
                cur = cur.children.entry(dir.to_string()).or_default();
            }
            cur.files.push(file.to_string());
        }
    }

    fn render(dir: &Dir, prefix: &str, out: &mut Vec<String>, count: &mut usize) {
        for file in &dir.files {
            if *count >= 200 {
                return;
            }
            out.push(format!("{prefix}{file}"));
            *count += 1;
        }
        let mut children: Vec<(&String, &Dir)> = dir.children.iter().collect();
        children.sort_by_key(|(k, _)| k.as_str());
        for (name, child) in children {
            if *count >= 200 {
                out.push(format!("{prefix}… (truncated)"));
                return;
            }
            out.push(format!("{prefix}{name}/"));
            render(child, &format!("{prefix}  "), out, count);
        }
    }

    let mut lines = Vec::new();
    let mut count = 0usize;
    render(&root, "", &mut lines, &mut count);
    lines.join("\n")
}

// ── Session event preview ─────────────────────────────────────────────────────

/// Extract the last `n` user/assistant messages from a session's event log
/// for preview display.
pub(crate) fn session_event_preview(
    sessions_dir: &std::path::Path,
    session_id: &str,
    max_lines: usize,
) -> Option<String> {
    let log_path = sessions_dir.join(format!("{session_id}.events.jsonl"));
    let raw = std::fs::read_to_string(&log_path).ok()?;
    let mut previews: Vec<String> = Vec::new();
    for line in raw.lines().rev() {
        if previews.len() >= max_lines {
            break;
        }
        let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let kind = val.get("kind").and_then(|v| v.as_str()).unwrap_or("");
        if kind != "MessageReceived" {
            continue;
        }
        let role = val.get("role").and_then(|v| v.as_str()).unwrap_or("?");
        let content = val
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .chars()
            .take(80)
            .collect::<String>();
        if content.trim().is_empty() {
            continue;
        }
        previews.push(format!("{role}: {content}"));
    }
    previews.reverse();
    if previews.is_empty() {
        None
    } else {
        Some(previews.join("\n"))
    }
}

/// Search across all session event logs for a keyword.
pub(crate) fn search_session_history(
    sessions_dir: &std::path::Path,
    query: &str,
    max_results: usize,
) -> Vec<String> {
    let query_lower = query.to_ascii_lowercase();
    let mut results = Vec::new();
    let Ok(entries) = std::fs::read_dir(sessions_dir) else {
        return results;
    };
    let mut paths: Vec<_> = entries
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".events.jsonl"))
        .collect();
    paths.sort_by_key(|e| std::cmp::Reverse(e.metadata().ok().and_then(|m| m.modified().ok())));
    for entry in paths {
        if results.len() >= max_results {
            break;
        }
        let session_id = entry
            .file_name()
            .to_string_lossy()
            .trim_end_matches(".events.jsonl")
            .to_string();
        let Ok(raw) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        for line in raw.lines() {
            if results.len() >= max_results {
                break;
            }
            let Ok(val) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            let kind = val.get("kind").and_then(|v| v.as_str()).unwrap_or("");
            if kind != "MessageReceived" {
                continue;
            }
            let content = val.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if content.to_ascii_lowercase().contains(&query_lower) {
                let role = val.get("role").and_then(|v| v.as_str()).unwrap_or("?");
                let preview: String = content.chars().take(100).collect();
                results.push(format!("[{session_id}] {role}: {preview}"));
            }
        }
    }
    results
}
