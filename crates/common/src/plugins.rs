//! Plugins: bundles of slash commands, skills, MCP servers and hooks.
//!
//! The on-disk layout is Claude Code's plugin layout, so existing plugins
//! work unchanged:
//!
//! ```text
//! my-plugin/
//!   .claude-plugin/plugin.json   name, version, description (also accepted:
//!                                .dcode-plugin/plugin.json, plugin.json)
//!   commands/**/*.md             → /my-plugin:<command>
//!   skills/<skill>/SKILL.md      → /my-plugin:<skill>
//!   .mcp.json                    MCP servers (or `mcpServers` in plugin.json)
//!   hooks/hooks.json             PreToolUse / PostToolUse / SessionStart / …
//! ```
//!
//! Installed plugins live in `~/.dcode-ai/plugins/<name>/`, recorded in
//! `~/.dcode-ai/plugins/installed.json`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::{HookCommand, HookConfig, McpServerConfig, dcode_ai_home_dir};

/// Manifest locations, first match wins.
const MANIFEST_PATHS: [&str; 3] = [
    ".claude-plugin/plugin.json",
    ".dcode-plugin/plugin.json",
    "plugin.json",
];

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Extra command paths (string or list), relative to the plugin root.
    #[serde(default)]
    pub commands: Option<Value>,
    /// Extra skill paths: a skill directory or a directory of skills.
    #[serde(default)]
    pub skills: Option<Value>,
    /// Extra agent-definition directories.
    #[serde(default)]
    pub agents: Option<Value>,
    /// Inline `mcpServers` map, or a path to a JSON file holding one.
    #[serde(default, rename = "mcpServers")]
    pub mcp_servers: Option<Value>,
    /// Inline hooks object, or a path to a hooks JSON file.
    #[serde(default)]
    pub hooks: Option<Value>,
    /// Inline `lspServers` map, or a path to a JSON file holding one.
    #[serde(default, rename = "lspServers")]
    pub lsp_servers: Option<Value>,
}

/// One entry of `installed.json`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstalledPlugin {
    pub name: String,
    /// What `plugin install` was given (path, `owner/repo`, git URL).
    pub source: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub commit: Option<String>,
    #[serde(default = "enabled_default")]
    pub enabled: bool,
}

fn enabled_default() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Registry {
    #[serde(default)]
    pub plugins: Vec<InstalledPlugin>,
}

/// A plugin on disk with its manifest.
#[derive(Debug, Clone)]
pub struct Plugin {
    pub manifest: PluginManifest,
    pub root: PathBuf,
    pub enabled: bool,
}

pub fn plugins_dir() -> Option<PathBuf> {
    dcode_ai_home_dir().map(|h| h.join("plugins"))
}

fn registry_path(dir: &Path) -> PathBuf {
    dir.join("installed.json")
}

impl Registry {
    pub fn load_from(dir: &Path) -> Self {
        std::fs::read_to_string(registry_path(dir))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save_to(&self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(registry_path(dir), text + "\n").map_err(|e| e.to_string())
    }

    pub fn get(&self, name: &str) -> Option<&InstalledPlugin> {
        self.plugins.iter().find(|p| p.name == name)
    }

    pub fn upsert(&mut self, entry: InstalledPlugin) {
        match self.plugins.iter_mut().find(|p| p.name == entry.name) {
            Some(existing) => *existing = entry,
            None => self.plugins.push(entry),
        }
        self.plugins.sort_by(|a, b| a.name.cmp(&b.name));
    }

    pub fn remove(&mut self, name: &str) -> bool {
        let before = self.plugins.len();
        self.plugins.retain(|p| p.name != name);
        before != self.plugins.len()
    }

    pub fn set_enabled(&mut self, name: &str, enabled: bool) -> bool {
        match self.plugins.iter_mut().find(|p| p.name == name) {
            Some(p) => {
                p.enabled = enabled;
                true
            }
            None => false,
        }
    }
}

/// Read the manifest of the plugin rooted at `root`.
pub fn read_manifest(root: &Path) -> Result<PluginManifest, String> {
    for rel in MANIFEST_PATHS {
        let path = root.join(rel);
        if let Ok(text) = std::fs::read_to_string(&path) {
            let mut manifest: PluginManifest =
                serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            manifest.name = manifest.name.trim().to_string();
            if manifest.name.is_empty() {
                return Err(format!("{}: `name` is required", path.display()));
            }
            validate_name(&manifest.name)?;
            return Ok(manifest);
        }
    }
    Err(format!(
        "no plugin manifest in {} (expected .claude-plugin/plugin.json)",
        root.display()
    ))
}

/// Plugin names become directory names and command prefixes.
pub fn validate_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && !name.starts_with('.');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid plugin name `{name}` (letters, digits, - _ . only)"
        ))
    }
}

/// Every installed plugin (enabled or not) under `dir`.
pub fn installed_in(dir: &Path) -> Vec<Plugin> {
    let registry = Registry::load_from(dir);
    registry
        .plugins
        .iter()
        .filter_map(|entry| {
            let root = dir.join(&entry.name);
            match read_manifest(&root) {
                Ok(manifest) => Some(Plugin {
                    manifest,
                    root,
                    enabled: entry.enabled,
                }),
                Err(e) => {
                    tracing::warn!("plugin `{}` skipped: {e}", entry.name);
                    None
                }
            }
        })
        .collect()
}

/// Enabled plugins from `~/.dcode-ai/plugins`.
pub fn enabled_plugins() -> Vec<Plugin> {
    plugins_dir()
        .map(|dir| {
            installed_in(&dir)
                .into_iter()
                .filter(|p| p.enabled)
                .collect()
        })
        .unwrap_or_default()
}

/// Map a Claude Code tool name used in hook matchers to dcode-ai's tool.
fn tool_name_for_matcher(name: &str) -> &str {
    match name {
        "Bash" => "execute_bash",
        "Edit" | "MultiEdit" => "edit_file",
        "Write" => "write_file",
        "Read" => "read_file",
        "Grep" | "Glob" => "search_code",
        "LS" => "list_directory",
        "WebFetch" => "fetch_url",
        "WebSearch" => "web_search",
        "Task" => "spawn_subagent",
        "TodoWrite" => "update_plan",
        other => other,
    }
}

impl Plugin {
    pub fn name(&self) -> &str {
        &self.manifest.name
    }

    /// `${CLAUDE_PLUGIN_ROOT}` / `${DCODE_PLUGIN_ROOT}` for substitutions.
    fn vars(&self) -> BTreeMap<String, String> {
        let root = self.root.display().to_string();
        BTreeMap::from([
            ("CLAUDE_PLUGIN_ROOT".to_string(), root.clone()),
            ("DCODE_PLUGIN_ROOT".to_string(), root),
        ])
    }

    fn rel_paths(value: &Option<Value>) -> Vec<String> {
        match value {
            Some(Value::String(s)) => vec![s.clone()],
            Some(Value::Array(a)) => a
                .iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Directories holding `*.md` command files.
    pub fn command_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = vec![self.root.join("commands")];
        for rel in Self::rel_paths(&self.manifest.commands) {
            dirs.push(self.root.join(rel.trim_start_matches("./")));
        }
        dirs.retain(|d| d.is_dir());
        dirs.dedup();
        dirs
    }

    pub fn skills_dir(&self) -> PathBuf {
        self.root.join("skills")
    }

    /// `skills/` plus manifest `skills` paths that exist.
    pub fn skill_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = vec![self.skills_dir()];
        for rel in Self::rel_paths(&self.manifest.skills) {
            dirs.push(self.root.join(rel.trim_start_matches("./")));
        }
        dirs.retain(|d| d.is_dir());
        dirs.dedup();
        dirs
    }

    /// `agents/` plus manifest `agents` paths that exist.
    pub fn agent_dirs(&self) -> Vec<PathBuf> {
        let mut dirs = vec![self.root.join("agents")];
        for rel in Self::rel_paths(&self.manifest.agents) {
            dirs.push(self.root.join(rel.trim_start_matches("./")));
        }
        dirs.retain(|d| d.is_dir());
        dirs.dedup();
        dirs
    }

    /// The JSON object found inline or at a manifest-relative path.
    fn json_at(&self, value: &Option<Value>, default_rel: &str) -> Option<Value> {
        match value {
            Some(v @ Value::Object(_)) => Some(v.clone()),
            Some(Value::String(rel)) => {
                let path = self.root.join(rel.trim_start_matches("./"));
                std::fs::read_to_string(path)
                    .ok()
                    .and_then(|t| serde_json::from_str(&t).ok())
            }
            _ => std::fs::read_to_string(self.root.join(default_rel))
                .ok()
                .and_then(|t| serde_json::from_str(&t).ok()),
        }
    }

    /// MCP servers the plugin declares, named `<plugin>_<server>`.
    pub fn mcp_servers(&self) -> Vec<McpServerConfig> {
        let Some(doc) = self.json_at(&self.manifest.mcp_servers, ".mcp.json") else {
            return Vec::new();
        };
        let origin = format!("plugin:{}", self.name());
        match crate::mcp_json::servers_from_value(&doc, &origin, &self.vars()) {
            Ok(servers) => servers
                .into_iter()
                .map(|mut s| {
                    s.name = format!("{}_{}", self.name(), s.name);
                    if s.cwd.is_none() && s.url.is_none() {
                        s.cwd = Some(self.root.clone());
                    }
                    s
                })
                .collect(),
            Err(e) => {
                tracing::warn!("plugin `{}` MCP servers ignored: {e}", self.name());
                Vec::new()
            }
        }
    }

    /// Language servers the plugin declares (`lspServers` / `.lsp.json`).
    pub fn lsp_servers(&self) -> Vec<crate::lsp::LspServerSpec> {
        self.json_at(&self.manifest.lsp_servers, ".lsp.json")
            .map(|doc| crate::lsp::parse_servers(&doc, &self.vars()))
            .unwrap_or_default()
    }

    /// Hooks from `hooks/hooks.json` (Claude Code format), mapped onto
    /// dcode-ai's hook events. Only `command` hooks are supported.
    pub fn hooks(&self) -> HookConfig {
        let mut out = HookConfig::default();
        let Some(doc) = self.json_at(&self.manifest.hooks, "hooks/hooks.json") else {
            return out;
        };
        let events = doc.get("hooks").unwrap_or(&doc);
        let Some(events) = events.as_object() else {
            return out;
        };
        let vars = self.vars();
        let origin = format!("plugin:{}", self.name());
        for (event, groups) in events {
            let target = match event.as_str() {
                "PreToolUse" => &mut out.pre_tool_use,
                "PostToolUse" => &mut out.post_tool_use,
                "SessionStart" => &mut out.session_start,
                "SessionEnd" => &mut out.session_end,
                "Stop" => &mut out.stop,
                "UserPromptSubmit" => &mut out.user_prompt_submit,
                "PreCompact" => &mut out.pre_compact,
                "SubagentStart" => &mut out.subagent_start,
                "SubagentStop" => &mut out.subagent_stop,
                "Notification" => &mut out.approval_requested,
                _ => continue,
            };
            let blocking = matches!(event.as_str(), "PreToolUse" | "UserPromptSubmit");
            for group in groups.as_array().into_iter().flatten() {
                let matcher = group.get("matcher").and_then(Value::as_str).unwrap_or("");
                // `Edit|Write` → one hook per alternative (matchers are plain).
                let matchers: Vec<Option<String>> = if matcher.trim().is_empty() || matcher == "*" {
                    vec![None]
                } else {
                    matcher
                        .split('|')
                        .map(|m| Some(tool_name_for_matcher(m.trim()).to_string()))
                        .collect()
                };
                for hook in group
                    .get("hooks")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if hook
                        .get("type")
                        .and_then(Value::as_str)
                        .unwrap_or("command")
                        != "command"
                    {
                        continue;
                    }
                    let Some(cmd) = hook.get("command").and_then(Value::as_str) else {
                        continue;
                    };
                    let command = crate::mcp_json::expand_vars(cmd, &vars);
                    for m in &matchers {
                        target.push(HookCommand {
                            command: command.clone(),
                            matcher: m.clone(),
                            blocking,
                            origin: origin.clone(),
                        });
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn write_plugin(root: &Path, name: &str) {
        std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        std::fs::write(
            root.join(".claude-plugin/plugin.json"),
            format!(r#"{{"name":"{name}","version":"1.2.0","description":"demo"}}"#),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("commands/git")).unwrap();
        std::fs::write(root.join("commands/hello.md"), "Say hello to $ARGUMENTS").unwrap();
        std::fs::write(root.join("commands/git/log.md"), "Show the log").unwrap();
        std::fs::write(
            root.join(".mcp.json"),
            r#"{"mcpServers":{"srv":{"command":"${CLAUDE_PLUGIN_ROOT}/bin/srv"}}}"#,
        )
        .unwrap();
        std::fs::create_dir_all(root.join("hooks")).unwrap();
        std::fs::write(
            root.join("hooks/hooks.json"),
            r#"{"hooks":{"PreToolUse":[{"matcher":"Edit|Write","hooks":[{"type":"command","command":"${CLAUDE_PLUGIN_ROOT}/check.sh"}]}],
                         "Stop":[{"hooks":[{"type":"command","command":"echo bye"}]}]}}"#,
        )
        .unwrap();
    }

    #[test]
    fn plugin_contributes_commands_mcp_and_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("demo");
        write_plugin(&root, "demo");
        let plugin = Plugin {
            manifest: read_manifest(&root).unwrap(),
            root: root.clone(),
            enabled: true,
        };
        assert_eq!(plugin.manifest.version.as_deref(), Some("1.2.0"));
        assert_eq!(plugin.command_dirs(), vec![root.join("commands")]);

        let servers = plugin.mcp_servers();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "demo_srv");
        assert_eq!(servers[0].command, format!("{}/bin/srv", root.display()));
        assert_eq!(servers[0].origin, "plugin:demo");

        let hooks = plugin.hooks();
        let matchers: Vec<_> = hooks
            .pre_tool_use
            .iter()
            .map(|h| h.matcher.clone().unwrap())
            .collect();
        assert_eq!(matchers, vec!["edit_file", "write_file"]);
        assert!(hooks.pre_tool_use[0].blocking);
        assert_eq!(hooks.stop[0].command, "echo bye");
    }

    #[test]
    fn registry_tracks_enabled_state() {
        let dir = tempfile::tempdir().unwrap();
        write_plugin(&dir.path().join("demo"), "demo");
        let mut reg = Registry::default();
        reg.upsert(InstalledPlugin {
            name: "demo".into(),
            source: "./demo".into(),
            version: None,
            commit: None,
            enabled: true,
        });
        reg.save_to(dir.path()).unwrap();
        assert_eq!(installed_in(dir.path()).len(), 1);
        let mut reg = Registry::load_from(dir.path());
        assert!(reg.set_enabled("demo", false));
        reg.save_to(dir.path()).unwrap();
        assert!(!installed_in(dir.path())[0].enabled);
        assert!(reg.remove("demo"));
    }

    #[test]
    fn names_are_validated() {
        assert!(validate_name("code-review").is_ok());
        assert!(validate_name("../evil").is_err());
        assert!(validate_name(".hidden").is_err());
    }
}
