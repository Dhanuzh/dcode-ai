use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::env;
use std::path::{Path, PathBuf};

/// Top-level configuration, merged from global, workspace, env, and CLI sources.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DcodeAiConfig {
    pub provider: ProviderConfig,
    pub model: ModelConfig,
    pub permissions: PermissionConfig,
    pub session: SessionConfig,
    pub harness: HarnessConfig,
    pub mcp: McpConfig,
    pub memory: MemoryConfig,
    pub hooks: HookConfig,
    pub web: WebConfig,
    /// CLI/TUI preferences (e.g. external editor).
    #[serde(default)]
    pub ui: UiConfig,
}

impl DcodeAiConfig {
    /// See [`per_token_billing`].
    pub fn bills_per_token(&self) -> bool {
        matches!(self.provider.default, ProviderKind::Anthropic)
            && self.provider.anthropic.backend == AnthropicBackend::Api
    }

    /// Load config from defaults, global file, workspace file, and environment.
    pub fn load() -> Result<Self, ConfigError> {
        let workspace_root = env::current_dir().map_err(|source| ConfigError::Io {
            action: "read current directory",
            path: PathBuf::from("."),
            source,
        })?;
        Self::load_for_workspace(&workspace_root)
    }

    /// Load config for an explicit workspace root.
    ///
    /// Layer order (later layers win):
    ///   1. Built-in defaults
    ///   2. Global user config  (`~/.dcode-ai/config.toml`)
    ///   3. Project config      (`.dcode.toml` in workspace root — commit to git)
    ///   4. Local override      (`.dcode-ai/config.local.toml` — gitignored, personal)
    ///   5. Environment variables
    pub fn load_for_workspace(workspace_root: &Path) -> Result<Self, ConfigError> {
        let mut config = Self::default();

        if let Some(path) = global_config_path()
            && path.exists()
        {
            let partial = load_partial(&path)?;
            config.merge(partial);
        }

        // Project-level config: `.dcode.toml` at workspace root.
        // Intended to be committed to git so teammates share the same defaults.
        let project_path = project_config_path(workspace_root);
        if project_path.exists() {
            let partial = load_partial(&project_path)?;
            config.merge(partial);
        }

        let local_path = workspace_config_path(workspace_root);
        if local_path.exists() {
            let partial = load_partial(&local_path)?;
            config.merge(partial);
        }

        config.add_mcp_json_servers(workspace_root);
        config.add_plugin_contributions(&crate::plugins::enabled_plugins());
        config.apply_disabled_mcp(&crate::folders::disabled_mcp(workspace_root));
        for path in PolicyRules::paths(workspace_root) {
            config.permissions.policy.load_file(&path)?;
        }
        config.apply_env();
        Ok(config)
    }

    /// Append servers from `.mcp.json` files (local > project > user). A
    /// server already declared in TOML keeps its TOML definition.
    pub fn add_mcp_json_servers(&mut self, workspace_root: &Path) {
        for scope in crate::mcp_json::McpScope::ALL {
            for server in crate::mcp_json::load_scope(scope, workspace_root) {
                self.add_mcp_server_if_new(server);
            }
        }
    }

    /// MCP servers and hooks from enabled plugins.
    pub fn add_plugin_contributions(&mut self, plugins: &[crate::plugins::Plugin]) {
        for plugin in plugins {
            for server in plugin.mcp_servers() {
                self.add_mcp_server_if_new(server);
            }
            let hooks = plugin.hooks();
            self.hooks.session_start.extend(hooks.session_start);
            self.hooks.session_end.extend(hooks.session_end);
            self.hooks.pre_tool_use.extend(hooks.pre_tool_use);
            self.hooks.post_tool_use.extend(hooks.post_tool_use);
            self.hooks.post_tool_failure.extend(hooks.post_tool_failure);
            self.hooks
                .approval_requested
                .extend(hooks.approval_requested);
            self.hooks.subagent_start.extend(hooks.subagent_start);
            self.hooks.subagent_stop.extend(hooks.subagent_stop);
            self.hooks
                .user_prompt_submit
                .extend(hooks.user_prompt_submit);
            self.hooks.stop.extend(hooks.stop);
            self.hooks.pre_compact.extend(hooks.pre_compact);
        }
    }

    /// Turn off the servers switched off in `/mcp` for this folder.
    pub fn apply_disabled_mcp(&mut self, names: &[String]) {
        for server in &mut self.mcp.servers {
            if names.contains(&server.name) {
                server.enabled = false;
            }
        }
    }

    /// Add `server` unless one with the same name is already configured.
    pub fn add_mcp_server_if_new(&mut self, server: McpServerConfig) {
        if !self.mcp.servers.iter().any(|s| s.name == server.name) {
            self.mcp.servers.push(server);
        }
    }

    /// Load only the persisted global config file layered over defaults.
    pub fn load_global_file() -> Result<Self, ConfigError> {
        let mut config = Self::default();
        if let Some(path) = global_config_path()
            && path.exists()
        {
            let partial = load_partial(&path)?;
            config.merge(partial);
        }
        Ok(config)
    }

    /// Load only the persisted workspace-local config layered over defaults.
    pub fn load_workspace_file(workspace_root: &Path) -> Result<Self, ConfigError> {
        let mut config = Self::default();
        let local_path = workspace_config_path(workspace_root);
        if local_path.exists() {
            let partial = load_partial(&local_path)?;
            config.merge(partial);
        }
        Ok(config)
    }

    /// Save the full config as the user's global defaults.
    pub fn save_global(&self) -> Result<(), ConfigError> {
        let path = global_config_path().ok_or(ConfigError::NoHomeDir)?;
        save_config_to_path(self, &path)
    }

    /// Save the full config as the workspace-local override file.
    pub fn save_workspace_file(&self, workspace_root: &Path) -> Result<(), ConfigError> {
        let path = workspace_config_path(workspace_root);
        save_config_to_path(self, &path)
    }

    /// Remove the workspace-local config file, if present.
    pub fn clear_workspace_file(workspace_root: &Path) -> Result<(), ConfigError> {
        let path = workspace_config_path(workspace_root);
        if !path.exists() {
            return Ok(());
        }
        std::fs::remove_file(&path).map_err(|source| ConfigError::Io {
            action: "remove config file",
            path,
            source,
        })
    }

    fn merge(&mut self, partial: PartialDcodeAiConfig) {
        let provider_changed = partial.provider.is_some();
        let explicit_model_override = partial
            .model
            .as_ref()
            .and_then(|model| model.default_model.as_ref())
            .is_some();
        if let Some(provider) = partial.provider {
            self.provider.merge(provider);
        }

        if let Some(model) = partial.model {
            self.model.merge(model);
        }

        if let Some(permissions) = partial.permissions {
            self.permissions.merge(permissions);
        }

        if let Some(session) = partial.session {
            self.session.merge(session);
        }
        if let Some(harness) = partial.harness {
            self.harness.merge(harness);
        }
        if let Some(mcp) = partial.mcp {
            self.mcp.merge(mcp);
        }
        if let Some(memory) = partial.memory {
            self.memory.merge(memory);
        }
        if let Some(hooks) = partial.hooks {
            self.hooks.merge(hooks);
        }
        if let Some(web) = partial.web {
            self.web.merge(web);
        }
        if let Some(ui) = partial.ui {
            self.ui.merge(ui);
        }

        if explicit_model_override {
            self.provider
                .set_model_for_default(self.model.default_model.clone());
        }

        if provider_changed || explicit_model_override {
            self.sync_default_model_from_provider();
        }
    }

    fn apply_env(&mut self) {
        if let Ok(provider) = env::var("DCODE_AI_DEFAULT_PROVIDER") {
            match ProviderKind::from_cli_name(&provider) {
                Some(kind) => {
                    self.provider.default = kind;
                    self.sync_default_model_from_provider();
                }
                None => tracing::warn!("DCODE_AI_DEFAULT_PROVIDER=`{provider}` is not supported"),
            }
        }

        if let Ok(model) = env::var("DCODE_AI_MODEL") {
            self.apply_model_override(&model);
        }

        if let Ok(api_key) = env::var("ANTHROPIC_API_KEY") {
            self.provider.anthropic.api_key = Some(api_key);
        }

        if let Ok(base_url) = env::var("ANTHROPIC_BASE_URL") {
            self.provider.anthropic.base_url = base_url;
        }

        if let Ok(model) = env::var("ANTHROPIC_MODEL") {
            self.provider.anthropic.model = migrate_claude_model_id(&model);
        }

        if let Ok(memory_path) = env::var("DCODE_AI_MEMORY_PATH") {
            self.memory.file_path = PathBuf::from(memory_path);
        }

        if let Ok(timeout_secs) = env::var("DCODE_AI_WEB_TIMEOUT_SECS")
            && let Ok(timeout_secs) = timeout_secs.parse()
        {
            self.web.timeout_secs = timeout_secs;
        }

        if let Ok(max_fetch_chars) = env::var("DCODE_AI_WEB_MAX_FETCH_CHARS")
            && let Ok(max_fetch_chars) = max_fetch_chars.parse()
        {
            self.web.max_fetch_chars = max_fetch_chars;
        }

        self.sync_default_model_from_provider();
    }

    pub fn apply_model_override(&mut self, raw_model: &str) {
        let resolved = migrate_claude_model_id(&self.model.resolve_alias(raw_model));
        self.provider.set_model_for_default(resolved);
        self.sync_default_model_from_provider();
    }

    /// Switch the default LLM provider and keep `default_model` aligned with that provider's model field.
    pub fn set_default_provider(&mut self, provider: ProviderKind) {
        self.provider.default = provider;
        self.sync_default_model_from_provider();
    }

    /// Set the API key stored in config for a provider (workspace save may persist it).
    pub fn set_provider_api_key(&mut self, provider: ProviderKind, key: impl Into<String>) {
        match provider {
            ProviderKind::Anthropic => self.provider.anthropic.api_key = Some(key.into()),
            // Copilot authenticates with a GitHub login, not a stored key.
            ProviderKind::Copilot => {}
        }
    }

    /// Editor command: `DCODE_AI_EDITOR`, then `[ui].editor`, then `EDITOR`, then `vim`.
    pub fn effective_editor_command(&self) -> String {
        if let Ok(v) = env::var("DCODE_AI_EDITOR") {
            let t = v.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
        if let Some(ref e) = self.ui.editor {
            let t = e.trim();
            if !t.is_empty() {
                return t.to_string();
            }
        }
        env::var("EDITOR")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "vim".to_string())
    }

    pub fn sync_default_model_from_provider(&mut self) {
        self.model.default_model = self.provider.active_model().to_string();
    }

    /// Returns `true` if the first-run onboarding gate should be shown.
    /// Triggers when: onboarding not completed OR all API keys have been removed.
    pub fn needs_onboarding(&self) -> bool {
        !self.ui.onboarding_completed || !self.provider.any_api_key_present()
    }
}

/// User interface preferences persisted in config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UiConfig {
    /// Shell command to launch the external editor (e.g. `vim` or `code --wait`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor: Option<String>,
    /// Theme name (future: "default", "tokyonight", etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// Hide hint text in the composer area.
    #[serde(default)]
    pub hide_tips: bool,
    /// Vim keys in the prompt (`/vim`): Esc → NORMAL, `i` → INSERT.
    #[serde(default)]
    pub vim_mode: bool,
    /// Suppress transient startup notices (MCP server starting/ready and the
    /// "[permissions] startup approval granted" line). Defaults to hidden; the
    /// `/quiet` command toggles it. Failures are always shown regardless.
    #[serde(default = "default_quiet_startup")]
    pub quiet_startup: bool,
    /// Lines per scroll event (default 3).
    #[serde(default = "default_scroll_speed")]
    pub scroll_speed: u16,
    /// Full mouse capture: enables TUI button clicks but disables native text selection.
    /// Default false: alternate scroll mode is used instead (wheel=arrow keys, selection works).
    #[serde(default = "default_mouse_capture", skip_serializing_if = "is_false")]
    pub mouse_capture: bool,
    /// Show line numbers for fenced code blocks in assistant markdown output.
    #[serde(default)]
    pub code_line_numbers: bool,
    /// Whether the user has completed the first-run onboarding flow.
    #[serde(default)]
    pub onboarding_completed: bool,
    /// Communication style preset injected into the system prompt
    /// ("concise", "friendly", "technical", "default").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub personality: Option<String>,
    /// Output style (`/output-style`): `default`, `explanatory`, `learning`
    /// or a custom style from an `output-styles/*.md` file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_style: Option<String>,
    /// Status-bar items to hide. Valid keys: "agent", "effort", "time",
    /// "context", "model". Empty = show everything (default).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub statusline_hidden: Vec<String>,
    /// Shell command whose first output line is shown at the right of the
    /// footer (Claude Code `statusLine`). It receives session JSON on stdin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statusline_command: Option<String>,
    /// Custom key bindings for global actions (`action = "ctrl+key"`). Rebinds
    /// the action to the given key; its built-in default key then no longer
    /// triggers it. Actions: palette, search, history, pin, expand, subagents,
    /// thinking, clear. Empty = defaults only.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub keymap: BTreeMap<String, String>,
    /// Desktop notifications (OSC 9 + terminal bell) when a turn finishes or
    /// an approval is requested while the terminal is unfocused.
    #[serde(default = "default_notifications")]
    pub notifications: bool,
    /// The "working…" line: `bar` (default), `dots` or `plain` (no animation).
    #[serde(default = "default_working_style")]
    pub working_style: String,
    /// Sweep a highlight across the working label.
    #[serde(default = "default_true")]
    pub shimmer: bool,
    /// Ring the terminal bell when a long turn (10s+) finishes and when
    /// dcode-ai needs an approval or an answer (`/sounds on|off`).
    #[serde(default)]
    pub sounds: bool,
}

/// Look of the "working…" line: `bar` (sweeping bar), `dots`, or `plain`.
fn default_working_style() -> String {
    "bar".into()
}

fn default_notifications() -> bool {
    true
}

fn default_scroll_speed() -> u16 {
    3
}

fn default_mouse_capture() -> bool {
    false
}

fn default_quiet_startup() -> bool {
    true
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            editor: None,
            theme: None,
            hide_tips: false,
            vim_mode: false,
            quiet_startup: default_quiet_startup(),
            scroll_speed: default_scroll_speed(),
            mouse_capture: default_mouse_capture(),
            code_line_numbers: false,
            onboarding_completed: false,
            personality: None,
            output_style: None,
            statusline_hidden: Vec::new(),
            statusline_command: None,
            keymap: BTreeMap::new(),
            notifications: default_notifications(),
            working_style: default_working_style(),
            shimmer: true,
            sounds: false,
        }
    }
}

impl UiConfig {
    fn merge(&mut self, partial: PartialUiConfig) {
        if let Some(editor) = partial.editor {
            self.editor = Some(editor);
        }
        if let Some(theme) = partial.theme {
            self.theme = Some(theme);
        }
        if let Some(v) = partial.vim_mode {
            self.vim_mode = v;
        }
        if let Some(hide_tips) = partial.hide_tips {
            self.hide_tips = hide_tips;
        }
        if let Some(quiet_startup) = partial.quiet_startup {
            self.quiet_startup = quiet_startup;
        }
        if let Some(scroll_speed) = partial.scroll_speed {
            self.scroll_speed = scroll_speed;
        }
        if let Some(mouse_capture) = partial.mouse_capture {
            self.mouse_capture = mouse_capture;
        }
        if let Some(code_line_numbers) = partial.code_line_numbers {
            self.code_line_numbers = code_line_numbers;
        }
        if let Some(onboarding_completed) = partial.onboarding_completed {
            self.onboarding_completed = onboarding_completed;
        }
        if let Some(style) = partial.output_style {
            self.output_style = Some(style).filter(|s| !s.trim().is_empty());
        }
        if let Some(personality) = partial.personality {
            self.personality = Some(personality);
        }
        if let Some(cmd) = partial.statusline_command {
            self.statusline_command = Some(cmd).filter(|c| !c.trim().is_empty());
        }
        if let Some(statusline_hidden) = partial.statusline_hidden {
            self.statusline_hidden = statusline_hidden;
        }
        if let Some(keymap) = partial.keymap {
            self.keymap = keymap;
        }
        if let Some(style) = partial.working_style {
            self.working_style = style;
        }
        if let Some(sounds) = partial.sounds {
            self.sounds = sounds;
        }
        if let Some(shimmer) = partial.shimmer {
            self.shimmer = shimmer;
        }
        if let Some(notifications) = partial.notifications {
            self.notifications = notifications;
        }
    }
}

/// Strip Windows' verbatim prefix (`\\?\C:\…` → `C:\…`) that
/// `Path::canonicalize` adds there. Verbatim paths confuse users in the UI
/// and some child processes; plain drive paths are equivalent for our use.
/// Whether the active provider bills per token (the Anthropic API). Claude
/// through the user's own `claude` binary and GitHub Copilot are plans: a
/// dollar estimate there would mislead. Set by the session runtime.
static PER_TOKEN_BILLING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_per_token_billing(on: bool) {
    PER_TOKEN_BILLING.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn per_token_billing() -> bool {
    PER_TOKEN_BILLING.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn simplify_path(path: PathBuf) -> PathBuf {
    let s = path.as_os_str().to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\") {
        if let Some(unc) = rest.strip_prefix(r"UNC\") {
            return PathBuf::from(format!(r"\\{unc}"));
        }
        return PathBuf::from(rest.to_string());
    }
    path
}

/// `Path::canonicalize` that strips Windows' verbatim prefix from the
/// result, so canonical paths compare cleanly against non-verbatim ones
/// (workspace guards) and remain readable in the UI.
pub fn canonicalize_simplified(path: &Path) -> std::io::Result<PathBuf> {
    Ok(simplify_path(path.canonicalize()?))
}

/// The user's home directory: `$HOME`, falling back to `%USERPROFILE%` —
/// Windows shells don't set `HOME`, which used to break every `~/.dcode-ai`
/// path there.
pub fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .or_else(|| env::var_os("USERPROFILE").filter(|v| !v.is_empty()))
        .map(PathBuf::from)
}

pub fn global_config_path() -> Option<PathBuf> {
    home_dir().map(|home| home.join(".dcode-ai/config.toml"))
}

/// `~/.dcode-ai` when the home directory is known.
pub fn dcode_ai_home_dir() -> Option<PathBuf> {
    home_dir().map(|home| home.join(".dcode-ai"))
}

/// Stable per-workspace id: `{slug}-{hex}` derived from the canonical workspace path.
pub fn workspace_cache_id(workspace_root: &Path) -> Result<(String, PathBuf), WorkspaceCacheError> {
    let canonical = canonicalize_simplified(workspace_root).map_err(|source| {
        WorkspaceCacheError::Canonicalize {
            path: workspace_root.to_path_buf(),
            source,
        }
    })?;
    let path_str = canonical.to_string_lossy();
    let suffix = workspace_path_hash_suffix(path_str.as_ref());
    let slug = workspace_dir_slug(&canonical);
    Ok((format!("{slug}-{suffix}"), canonical))
}

/// `~/.dcode-ai/workspaces/<workspace-id>/`
pub fn workspace_cache_dir(workspace_root: &Path) -> Result<PathBuf, WorkspaceCacheError> {
    let (id, _) = workspace_cache_id(workspace_root)?;
    let home = dcode_ai_home_dir().ok_or(WorkspaceCacheError::NoHomeDir)?;
    Ok(home.join("workspaces").join(id))
}

/// Cached CLI index JSON for this workspace.
pub fn workspace_cli_index_path(workspace_root: &Path) -> Result<PathBuf, WorkspaceCacheError> {
    Ok(workspace_cache_dir(workspace_root)?.join("cli-index.json"))
}

fn workspace_dir_slug(path: &Path) -> String {
    let raw = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("workspace")
        .to_ascii_lowercase();
    let mut out = String::new();
    let mut prev_sep = false;
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_sep = false;
        } else if !out.is_empty() && !prev_sep {
            out.push('-');
            prev_sep = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "workspace".to_string()
    } else {
        trimmed
    }
}

fn workspace_path_hash_suffix(canonical_path: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(canonical_path.as_bytes());
    let digest = hasher.finalize();
    // 16 hex chars — stable across Rust versions (unlike std::collections::hash_map::DefaultHasher).
    format!("{digest:x}")[..16].to_string()
}

#[derive(Debug, thiserror::Error)]
pub enum WorkspaceCacheError {
    #[error("HOME is not set")]
    NoHomeDir,
    #[error("failed to canonicalize workspace path {path}: {source}")]
    Canonicalize {
        path: PathBuf,
        source: std::io::Error,
    },
}

fn default_cleanup_period_days() -> u32 {
    30
}

/// Add (`add`) or remove a `[permissions]` rule (`kind` is `allow`, `ask` or
/// `deny`) in one config file, keeping everything else in it. Returns
/// whether the file changed.
/// The project config teammates share (commit it): `.dcode.toml`.
pub fn project_config_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".dcode.toml")
}

/// Set `[section] key = value` in a TOML file, creating it if needed.
pub fn set_toml_value(
    path: &Path,
    section: &str,
    key: &str,
    value: toml::Value,
) -> Result<(), String> {
    let mut doc: toml::Table = match std::fs::read_to_string(path) {
        Ok(raw) => toml::from_str(&raw).map_err(|e| format!("{}: {e}", path.display()))?,
        Err(_) => toml::Table::new(),
    };
    doc.entry(section)
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or_else(|| format!("`{section}` is not a table"))?
        .insert(key.to_string(), value);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = toml::to_string_pretty(&doc).map_err(|e| e.to_string())?;
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

pub fn edit_permission_rule(
    path: &Path,
    kind: &str,
    rule: &str,
    add: bool,
) -> Result<bool, String> {
    if !matches!(kind, "allow" | "ask" | "deny") {
        return Err(format!("unknown rule kind `{kind}`"));
    }
    let mut doc: toml::Table = match std::fs::read_to_string(path) {
        Ok(raw) => toml::from_str(&raw).map_err(|e| format!("{}: {e}", path.display()))?,
        Err(_) => toml::Table::new(),
    };
    let perms = doc
        .entry("permissions")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or("`permissions` is not a table")?;
    let list = perms
        .entry(kind)
        .or_insert_with(|| toml::Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| format!("`permissions.{kind}` is not a list"))?;
    let present = list.iter().any(|v| v.as_str() == Some(rule));
    let changed = if add && !present {
        list.push(toml::Value::String(rule.to_string()));
        true
    } else if !add && present {
        list.retain(|v| v.as_str() != Some(rule));
        true
    } else {
        false
    };
    if changed {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = toml::to_string_pretty(&doc).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(changed)
}

/// Hook events `[hooks]` accepts, in lifecycle order.
pub const HOOK_EVENTS: [&str; 11] = [
    "session_start",
    "user_prompt_submit",
    "pre_tool_use",
    "post_tool_use",
    "post_tool_failure",
    "approval_requested",
    "subagent_start",
    "subagent_stop",
    "pre_compact",
    "stop",
    "session_end",
];

impl HookConfig {
    /// The hooks registered for `event` (one of [`HOOK_EVENTS`]).
    pub fn for_event(&self, event: &str) -> &[HookCommand] {
        match event {
            "session_start" => &self.session_start,
            "session_end" => &self.session_end,
            "pre_tool_use" => &self.pre_tool_use,
            "post_tool_use" => &self.post_tool_use,
            "post_tool_failure" => &self.post_tool_failure,
            "approval_requested" => &self.approval_requested,
            "subagent_start" => &self.subagent_start,
            "subagent_stop" => &self.subagent_stop,
            "user_prompt_submit" => &self.user_prompt_submit,
            "stop" => &self.stop,
            "pre_compact" => &self.pre_compact,
            _ => &[],
        }
    }
}

/// Add a hook to, or remove hooks running `command` from, one config file's
/// `[[hooks.<event>]]` list. Returns whether the file changed.
pub fn edit_hook(path: &Path, event: &str, hook: &HookCommand, add: bool) -> Result<bool, String> {
    if !HOOK_EVENTS.contains(&event) {
        return Err(format!(
            "unknown hook event `{event}`; one of: {}",
            HOOK_EVENTS.join(", ")
        ));
    }
    let mut doc: toml::Table = match std::fs::read_to_string(path) {
        Ok(raw) => toml::from_str(&raw).map_err(|e| format!("{}: {e}", path.display()))?,
        Err(_) => toml::Table::new(),
    };
    let hooks = doc
        .entry("hooks")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or("`hooks` is not a table")?;
    let list = hooks
        .entry(event)
        .or_insert_with(|| toml::Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| format!("`hooks.{event}` is not a list"))?;
    let same =
        |v: &toml::Value| v.get("command").and_then(|c| c.as_str()) == Some(hook.command.as_str());
    let changed = if add {
        let mut t = toml::Table::new();
        t.insert("command".into(), toml::Value::String(hook.command.clone()));
        if let Some(m) = &hook.matcher {
            t.insert("matcher".into(), toml::Value::String(m.clone()));
        }
        if hook.blocking {
            t.insert("blocking".into(), toml::Value::Boolean(true));
        }
        list.push(toml::Value::Table(t));
        true
    } else {
        let before = list.len();
        list.retain(|v| !same(v));
        list.len() != before
    };
    if changed {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = toml::to_string_pretty(&doc).map_err(|e| e.to_string())?;
        std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    Ok(changed)
}

/// `~/.dcode-ai/config.toml`.
pub fn global_config_file() -> Option<PathBuf> {
    global_config_path()
}

pub fn workspace_config_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(".dcode-ai").join("config.local.toml")
}

fn load_partial(path: &Path) -> Result<PartialDcodeAiConfig, ConfigError> {
    let raw = std::fs::read_to_string(path).map_err(|source| ConfigError::ReadFile {
        path: path.to_path_buf(),
        source,
    })?;

    toml::from_str(&raw).map_err(|source| ConfigError::ParseToml {
        path: path.to_path_buf(),
        source,
    })
}

fn save_config_to_path(config: &DcodeAiConfig, path: &Path) -> Result<(), ConfigError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ConfigError::Io {
            action: "create config directory",
            path: parent.to_path_buf(),
            source,
        })?;
    }

    // Servers from `.mcp.json` files and plugin servers/hooks live in their
    // own files; only TOML-declared entries are written back.
    let mut owned = config.clone();
    owned.mcp.servers.retain(|s| s.origin.is_empty());
    for list in [
        &mut owned.hooks.session_start,
        &mut owned.hooks.session_end,
        &mut owned.hooks.pre_tool_use,
        &mut owned.hooks.post_tool_use,
        &mut owned.hooks.post_tool_failure,
        &mut owned.hooks.approval_requested,
        &mut owned.hooks.subagent_start,
        &mut owned.hooks.subagent_stop,
        &mut owned.hooks.user_prompt_submit,
        &mut owned.hooks.stop,
        &mut owned.hooks.pre_compact,
    ] {
        list.retain(|h| h.origin.is_empty());
    }
    let config = &owned;
    let raw = toml::to_string_pretty(config).map_err(|source| ConfigError::SerializeToml {
        path: path.to_path_buf(),
        source,
    })?;

    std::fs::write(path, raw).map_err(|source| ConfigError::Io {
        action: "write config file",
        path: path.to_path_buf(),
        source,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("unable to determine the home directory for global config")]
    NoHomeDir,
    #[error("failed to read config file {path}: {source}")]
    ReadFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to parse config file {path}: {source}")]
    ParseToml {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("failed to serialize config file {path}: {source}")]
    SerializeToml {
        path: PathBuf,
        source: toml::ser::Error,
    },
    #[error("failed to {action} at {path}: {source}")]
    Io {
        action: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub default: ProviderKind,
    pub anthropic: AnthropicConfig,
    #[serde(default)]
    pub copilot: CopilotConfig,
}

/// GitHub Copilot (signed in with GitHub device login).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopilotConfig {
    /// Env var holding a GitHub token with Copilot access (CI); the device
    /// login stored by `dcode-ai login copilot` is used otherwise.
    pub api_key_env: String,
    /// Copilot API base; the token exchange may return a plan-specific one.
    pub base_url: String,
    pub model: String,
    pub temperature: f32,
    /// Client identity headers the Copilot API expects.
    pub editor_version: String,
    pub editor_plugin_version: String,
    pub integration_id: String,
    pub user_agent: String,
    /// Reasoning effort for models that support it (`low` … `max`); unset
    /// uses the model's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Client id of the GitHub OAuth app used for `dcode-ai login copilot`
    /// (your own app; unset uses dcode-ai's default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_client_id: Option<String>,
}

impl Default for CopilotConfig {
    fn default() -> Self {
        Self {
            api_key_env: "COPILOT_GITHUB_TOKEN".into(),
            base_url: "https://api.githubcopilot.com".into(),
            model: "gpt-4.1".into(),
            temperature: 0.7,
            editor_version: "vscode/1.90.0".into(),
            editor_plugin_version: "copilot-chat/0.22.4".into(),
            integration_id: "vscode-chat".into(),
            user_agent: "GitHubCopilotChat/0.22.4".into(),
            reasoning_effort: None,
            oauth_client_id: None,
        }
    }
}

impl CopilotConfig {
    pub fn resolve_github_token(&self) -> Option<String> {
        env::var(&self.api_key_env)
            .ok()
            .filter(|v| !v.trim().is_empty())
    }

    fn merge(&mut self, partial: PartialCopilotConfig) {
        if let Some(v) = partial.oauth_client_id {
            self.oauth_client_id = Some(v).filter(|v| !v.trim().is_empty());
        }
        if let Some(v) = partial.reasoning_effort {
            self.reasoning_effort = Some(v).filter(|v| !v.trim().is_empty());
        }
        if let Some(v) = partial.api_key_env {
            self.api_key_env = v;
        }
        if let Some(v) = partial.base_url {
            self.base_url = v;
        }
        if let Some(v) = partial.model {
            self.model = v;
        }
        if let Some(v) = partial.temperature {
            self.temperature = v;
        }
        if let Some(v) = partial.editor_version {
            self.editor_version = v;
        }
        if let Some(v) = partial.editor_plugin_version {
            self.editor_plugin_version = v;
        }
        if let Some(v) = partial.integration_id {
            self.integration_id = v;
        }
        if let Some(v) = partial.user_agent {
            self.user_agent = v;
        }
    }
}

/// Older dcode-ai versions shipped Claude model ids with invented date
/// suffixes that the API rejects; map them to the real ids.
pub fn migrate_claude_model_id(model: &str) -> String {
    match model.trim() {
        "claude-sonnet-4-6-20250514" => "claude-sonnet-4-6".into(),
        "claude-opus-4-8-20250520" => "claude-opus-4-8".into(),
        other => other.to_string(),
    }
}

/// Older configs reached Copilot by pointing the OpenAI provider at
/// `api.githubcopilot.com`.
/// Older configs reached Copilot by pointing the OpenAI provider at
/// `api.githubcopilot.com`.
pub fn is_copilot_base_url(base_url: &str) -> bool {
    base_url.to_ascii_lowercase().contains("githubcopilot.com")
}

impl Default for ProviderConfig {
    fn default() -> Self {
        Self {
            default: ProviderKind::Anthropic,
            anthropic: AnthropicConfig::default(),
            copilot: CopilotConfig::default(),
        }
    }
}

impl ProviderConfig {
    fn merge(&mut self, partial: PartialProviderConfig) {
        if let Some(anthropic) = partial.anthropic {
            self.anthropic.merge(anthropic);
            self.anthropic.model = migrate_claude_model_id(&self.anthropic.model);
        }
        if let Some(copilot) = partial.copilot {
            self.copilot.merge(copilot);
        }
        if let Some(default) = partial.default.as_deref() {
            match ProviderKind::from_cli_name(default) {
                Some(kind) => self.default = kind,
                // `default = "openai"` + `openai.base_url = …githubcopilot.com`
                // was how Copilot used to be selected.
                None if partial
                    .openai
                    .as_ref()
                    .and_then(|o| o.base_url.as_deref())
                    .is_some_and(is_copilot_base_url) =>
                {
                    self.default = ProviderKind::Copilot;
                    if let Some(model) = partial
                        .openai
                        .as_ref()
                        .and_then(|o| o.model.clone())
                        .filter(|m| !m.trim().is_empty())
                    {
                        self.copilot.model = model;
                    }
                }
                // Providers that were removed (OpenAI, OpenRouter, MiniMax,
                // Antigravity …): keep the current default instead of failing.
                None => tracing::warn!(
                    "provider `{default}` is no longer supported; using {}",
                    self.default.display_name()
                ),
            }
        }
    }

    pub fn active_model(&self) -> &str {
        self.model_for(self.default)
    }

    pub fn set_model_for_default(&mut self, model: impl Into<String>) {
        self.set_model_for(self.default, model);
    }

    pub fn set_model_for(&mut self, provider: ProviderKind, model: impl Into<String>) {
        let model = model.into();
        match provider {
            ProviderKind::Anthropic => self.anthropic.model = model,
            ProviderKind::Copilot => self.copilot.model = model,
        }
    }

    pub fn model_for(&self, provider: ProviderKind) -> &str {
        match provider {
            ProviderKind::Anthropic => &self.anthropic.model,
            ProviderKind::Copilot => &self.copilot.model,
        }
    }

    pub fn base_url_for(&self, provider: ProviderKind) -> &str {
        match provider {
            ProviderKind::Anthropic => &self.anthropic.base_url,
            ProviderKind::Copilot => &self.copilot.base_url,
        }
    }

    pub fn api_key_env_for(&self, provider: ProviderKind) -> &str {
        match provider {
            ProviderKind::Anthropic => &self.anthropic.api_key_env,
            ProviderKind::Copilot => &self.copilot.api_key_env,
        }
    }

    /// Env/config credential present (CI). Interactive sign-ins (Claude Code,
    /// `ant`, GitHub device login) are checked by the CLI, not here.
    pub fn api_key_present_for(&self, provider: ProviderKind) -> bool {
        match provider {
            ProviderKind::Anthropic => self.anthropic.resolve_api_key().is_some(),
            ProviderKind::Copilot => self.copilot.resolve_github_token().is_some(),
        }
    }

    pub fn any_api_key_present(&self) -> bool {
        ProviderKind::ALL
            .iter()
            .any(|p| self.api_key_present_for(*p))
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Anthropic,
    Copilot,
}

impl ProviderKind {
    pub const ALL: [ProviderKind; 2] = [ProviderKind::Anthropic, ProviderKind::Copilot];

    /// Parse user/CLI input (slash commands, TUI pickers, env, config).
    pub fn from_cli_name(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "anthropic" | "claude" => Some(Self::Anthropic),
            "copilot" | "github-copilot" | "github" | "gh" => Some(Self::Copilot),
            _ => None,
        }
    }

    pub fn display_name(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "Claude",
            ProviderKind::Copilot => "GitHub Copilot",
        }
    }

    pub fn to_config_key(self) -> &'static str {
        match self {
            ProviderKind::Anthropic => "anthropic",
            ProviderKind::Copilot => "copilot",
        }
    }

    /// Match [`display_name`](Self::display_name) output (case-insensitive).
    pub fn parse_display_name(s: &str) -> Option<Self> {
        let t = s.trim();
        Self::ALL
            .into_iter()
            .find(|k| k.display_name().eq_ignore_ascii_case(t))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnthropicConfig {
    pub api_key_env: String,
    pub api_key: Option<String>,
    pub base_url: String,
    pub model: String,
    pub temperature: f32,
    /// `output_config.effort` for models that support it
    /// (low | medium | high | xhigh | max). `None` uses the model default.
    #[serde(
        default = "default_anthropic_effort",
        skip_serializing_if = "Option::is_none"
    )]
    pub effort: Option<String>,
    /// Anthropic OAuth profile (from `ant auth login --profile <name>`).
    /// `None` uses the active profile. Only used by the `api` backend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth_profile: Option<String>,
    /// Which Claude backend to use. Default: the user's own signed-in
    /// Claude Code (`claude`) account.
    #[serde(default)]
    pub backend: AnthropicBackend,
}

/// How dcode-ai reaches Claude.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AnthropicBackend {
    /// The user's own `claude` binary (Claude Code), signed in with their
    /// Claude account; used as a model only — dcode-ai runs every tool.
    #[default]
    ClaudeCode,
    /// The Anthropic Messages API with Console OAuth via the `ant` CLI
    /// (billed to API credits).
    Api,
}

impl AnthropicBackend {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "claude-code" | "claude_code" | "claude" | "account" | "subscription" => {
                Some(Self::ClaudeCode)
            }
            "api" | "console" => Some(Self::Api),
            _ => None,
        }
    }
}

fn default_anthropic_effort() -> Option<String> {
    Some("high".into())
}

impl Default for AnthropicConfig {
    fn default() -> Self {
        Self {
            api_key_env: "ANTHROPIC_API_KEY".into(),
            api_key: None,
            base_url: "https://api.anthropic.com".into(),
            model: "claude-opus-5-5".into(),
            temperature: 1.0,
            effort: default_anthropic_effort(),
            oauth_profile: None,
            backend: AnthropicBackend::default(),
        }
    }
}

impl AnthropicConfig {
    pub fn resolve_api_key(&self) -> Option<String> {
        resolve_api_key_value(&self.api_key, &self.api_key_env)
    }

    fn merge(&mut self, partial: PartialAnthropicConfig) {
        if let Some(api_key_env) = partial.api_key_env {
            self.api_key_env = api_key_env;
        }
        if let Some(api_key) = partial.api_key {
            self.api_key = Some(api_key);
        }
        if let Some(base_url) = partial.base_url {
            self.base_url = base_url;
        }
        if let Some(model) = partial.model {
            self.model = model;
        }
        if let Some(temperature) = partial.temperature {
            self.temperature = temperature;
        }
        if let Some(effort) = partial.effort {
            self.effort = (!effort.trim().is_empty()).then_some(effort);
        }
        if let Some(profile) = partial.oauth_profile {
            self.oauth_profile = (!profile.trim().is_empty()).then_some(profile);
        }
        if let Some(backend) = partial.backend {
            self.backend = backend;
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelConfig {
    pub default_model: String,
    pub max_tokens: u32,
    pub enable_thinking: bool,
    pub thinking_budget: u32,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub aliases: BTreeMap<String, String>,
    /// Last N used model names for F2 cycling.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_models: Vec<String>,
    /// Model for context-compaction summaries (alias or id); unset = the
    /// session's model. A small fast model saves cost on long sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_model: Option<String>,
    /// Default model for sub-agents without their own `model:`; unset =
    /// the parent's model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_model: Option<String>,
    /// Models tried in order (up to three) when the session's model is
    /// overloaded or unavailable (Claude Code's `fallbackModel`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_models: Vec<String>,
}

/// Convenient shorthands that map to full model IDs.
/// User-defined aliases in config override these.
pub const BUILTIN_MODEL_ALIASES: &[(&str, &str)] = &[
    // Anthropic
    ("opus", "claude-opus-5-5"),
    ("sonnet", "claude-sonnet-5-5"),
    ("haiku", "claude-haiku-5-5"),
    ("fable", "claude-fable-5-1"),
];

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            default_model: "claude-opus-5-5".into(),
            max_tokens: 8192,
            enable_thinking: false,
            thinking_budget: 5120,
            aliases: BTreeMap::new(),
            recent_models: Vec::new(),
            compact_model: None,
            subagent_model: None,
            fallback_models: Vec::new(),
        }
    }
}

impl ModelConfig {
    fn merge(&mut self, partial: PartialModelConfig) {
        if let Some(m) = partial.compact_model {
            self.compact_model = Some(m).filter(|m| !m.trim().is_empty());
        }
        if let Some(f) = partial.fallback_models {
            self.fallback_models = f
                .into_iter()
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty())
                .take(3)
                .collect();
        }
        if let Some(m) = partial.subagent_model {
            self.subagent_model = Some(m).filter(|m| !m.trim().is_empty());
        }
        if let Some(default_model) = partial.default_model {
            self.default_model = migrate_claude_model_id(&default_model);
        }
        if let Some(max_tokens) = partial.max_tokens {
            self.max_tokens = max_tokens;
        }
        if let Some(enable_thinking) = partial.enable_thinking {
            self.enable_thinking = enable_thinking;
        }
        if let Some(thinking_budget) = partial.thinking_budget {
            self.thinking_budget = thinking_budget;
        }
        if let Some(aliases) = partial.aliases {
            self.aliases = aliases;
        }
        if let Some(recent_models) = partial.recent_models {
            self.recent_models = recent_models
                .iter()
                .map(|m| migrate_claude_model_id(m))
                .collect();
        }
    }

    pub fn resolve_alias(&self, raw: &str) -> String {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return self.default_model.clone();
        }

        let lowered = trimmed.to_ascii_lowercase();
        // User aliases take priority, then built-in shortcuts.
        self.aliases
            .get(&lowered)
            .cloned()
            .or_else(|| {
                BUILTIN_MODEL_ALIASES
                    .iter()
                    .find(|(k, _)| *k == lowered)
                    .map(|(_, v)| v.to_string())
            })
            .unwrap_or_else(|| trimmed.to_string())
    }

    /// Return all aliases (built-in + user overrides) for display in `/models`.
    pub fn all_aliases(&self) -> Vec<(&str, &str)> {
        let mut out: Vec<(&str, &str)> = BUILTIN_MODEL_ALIASES.to_vec();
        for (k, v) in &self.aliases {
            if let Some(entry) = out.iter_mut().find(|(alias, _)| *alias == k.as_str()) {
                entry.1 = v.as_str();
            } else {
                // Can't push user aliases as &str easily; skip — user aliases
                // are already displayed separately.
            }
        }
        out
    }

    /// Push a model name to the front of the recent list, deduplicating and capping at 8.
    pub fn track_recent_model(&mut self, model: &str) {
        self.recent_models.retain(|m| m != model);
        self.recent_models.insert(0, model.to_string());
        self.recent_models.truncate(8);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionConfig {
    pub mode: PermissionMode,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub ask: Vec<String>,
    #[serde(default)]
    pub startup_approve_all: bool,
    /// Landlock-confine shell commands (Linux): write access only beneath the
    /// workspace and scratch dirs; read/execute everywhere. Best-effort on
    /// kernels without Landlock. Off by default.
    #[serde(default)]
    pub sandbox_bash: bool,
    /// Extra directories writable inside the bash sandbox (e.g. "~/.cargo"
    /// so `cargo build` can use its registry cache). `~` expands to $HOME.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sandbox_writable_roots: Vec<String>,
    /// Extra folders the agent may read and edit besides the workspace
    /// (Claude Code's `additionalDirectories`; also `/add-dir`, `--add-dir`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_directories: Vec<String>,
    /// Rules from `policy.toml` files (never written back to config.toml).
    #[serde(skip)]
    pub policy: PolicyRules,
}

/// `allow` / `ask` / `deny` rules from a `policy.toml`, tagged with the file
/// they came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PolicyRules {
    pub allow: Vec<(String, String)>,
    pub ask: Vec<(String, String)>,
    pub deny: Vec<(String, String)>,
}

#[derive(Debug, Default, Deserialize)]
struct PolicyFile {
    #[serde(default)]
    allow: Vec<String>,
    #[serde(default)]
    ask: Vec<String>,
    #[serde(default)]
    deny: Vec<String>,
}

impl PolicyRules {
    /// Append the rules of one `policy.toml` (missing file = no rules).
    pub fn load_file(&mut self, path: &Path) -> Result<(), ConfigError> {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Ok(());
        };
        let file: PolicyFile = toml::from_str(&text).map_err(|source| ConfigError::ParseToml {
            path: path.to_path_buf(),
            source,
        })?;
        let origin = path.display().to_string();
        let tag = |v: Vec<String>| {
            v.into_iter()
                .map(|r| (r, origin.clone()))
                .collect::<Vec<_>>()
        };
        self.allow.extend(tag(file.allow));
        self.ask.extend(tag(file.ask));
        self.deny.extend(tag(file.deny));
        Ok(())
    }

    /// Policy files for a workspace: user (`~/.dcode-ai/policy.toml`) then
    /// project (`.dcode-ai/policy.toml`).
    pub fn paths(workspace_root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Some(home) = dcode_ai_home_dir() {
            out.push(home.join("policy.toml"));
        }
        out.push(workspace_root.join(".dcode-ai/policy.toml"));
        out
    }
}

impl Default for PermissionConfig {
    fn default() -> Self {
        Self {
            mode: PermissionMode::Default,
            allow: Vec::new(),
            deny: Vec::new(),
            ask: Vec::new(),
            startup_approve_all: false,
            sandbox_bash: false,
            sandbox_writable_roots: Vec::new(),
            additional_directories: Vec::new(),
            policy: PolicyRules::default(),
        }
    }
}

impl PermissionConfig {
    fn merge(&mut self, partial: PartialPermissionConfig) {
        if let Some(mode) = partial.mode {
            self.mode = mode;
        }
        if let Some(allow) = partial.allow {
            self.allow = allow;
        }
        if let Some(deny) = partial.deny {
            self.deny = deny;
        }
        if let Some(ask) = partial.ask {
            self.ask = ask;
        }
        if let Some(startup_approve_all) = partial.startup_approve_all {
            self.startup_approve_all = startup_approve_all;
        }
        if let Some(sandbox_bash) = partial.sandbox_bash {
            self.sandbox_bash = sandbox_bash;
        }
        if let Some(dirs) = partial.additional_directories {
            self.additional_directories = dirs;
        }
        if let Some(sandbox_writable_roots) = partial.sandbox_writable_roots {
            self.sandbox_writable_roots = sandbox_writable_roots;
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum PermissionMode {
    #[default]
    Default,
    Plan,
    AcceptEdits,
    DontAsk,
    BypassPermissions,
    /// Claude Code's auto mode: reads, edits and ordinary commands run
    /// without asking; risky actions (deleting, pushing, publishing, `sudo`,
    /// …) still ask and known-destructive commands are refused.
    Auto,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionConfig {
    pub history_dir: PathBuf,
    #[serde(alias = "max_turn_per_run")]
    pub max_turns_per_run: u32,
    pub max_tool_calls_per_turn: u32,
    pub checkpoint_interval: u32,
    /// File that stores the last active session ID for auto-resume.
    pub last_session_file: PathBuf,
    /// Auto-compact when switching away from a session.
    #[serde(default)]
    pub auto_compact_on_finish: bool,
    /// Stop the agent once a session's estimated cost reaches this many
    /// USD (API list prices; 0 = no limit).
    #[serde(default)]
    pub max_cost_usd: f64,
    /// Pause a turn once it has processed this many tokens (input, cache
    /// and output across its requests), so a runaway turn can't drain a
    /// plan. Send "continue" to go on. 0 = no limit.
    #[serde(default = "default_turn_token_alarm")]
    pub turn_token_alarm: u64,
    /// After a turn that changed files, run the read-only verifier (`/verify`).
    #[serde(default)]
    pub verify_after_edits: bool,
    /// After a turn that changed files, get a second opinion from a model of
    /// a different family (`/second-opinion`).
    #[serde(default)]
    pub cross_review: bool,
    /// Reviewer for `/second-opinion`: a model id, optionally prefixed with
    /// the provider (`copilot:gpt-5`, `anthropic:opus`). Empty = pick a model
    /// from a different family automatically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub second_opinion_model: Option<String>,
    /// Delete saved sessions untouched for this many days, at startup
    /// (Claude Code's `cleanupPeriodDays`). 0 keeps them forever.
    #[serde(default = "default_cleanup_period_days")]
    pub cleanup_period_days: u32,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            history_dir: PathBuf::from(".dcode-ai/sessions"),
            max_turns_per_run: 128,
            max_tool_calls_per_turn: 200,
            checkpoint_interval: 5,
            last_session_file: PathBuf::from(".dcode-ai/.last_session"),
            auto_compact_on_finish: false,
            max_cost_usd: 0.0,
            turn_token_alarm: default_turn_token_alarm(),
            verify_after_edits: false,
            cross_review: false,
            second_opinion_model: None,
            cleanup_period_days: default_cleanup_period_days(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessConfig {
    pub built_in_enabled: bool,
    pub project_instructions_path: PathBuf,
    pub local_instructions_path: PathBuf,
    pub skill_directories: Vec<PathBuf>,
    /// Prepend a ranked repo map (top referenced files) to the system prompt so
    /// the agent starts oriented. Off by default — it adds tokens every session.
    #[serde(default)]
    pub include_repo_map: bool,
    /// Extra system-prompt text for this run (`--append-system-prompt`).
    /// Never saved.
    #[serde(skip)]
    pub append_system_prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpConfig {
    #[serde(default)]
    pub expose_in_safe_mode: bool,
    #[serde(default)]
    pub servers: Vec<McpServerConfig>,
    /// With more MCP tools than this, their schemas are left out of requests
    /// and the model loads them with `tool_search` (Claude Code's deferred
    /// tools). 0 = always send every tool.
    #[serde(default = "default_tool_search_threshold")]
    pub tool_search_threshold: usize,
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            expose_in_safe_mode: false,
            servers: Vec::new(),
            tool_search_threshold: default_tool_search_threshold(),
        }
    }
}

fn default_tool_search_threshold() -> usize {
    6
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    pub name: String,
    /// Command for a stdio transport. Empty/omitted when `url` is set.
    #[serde(default)]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<PathBuf>,
    /// Endpoint for a Streamable-HTTP transport. When set, the server is reached
    /// over HTTP (JSON-RPC POST) instead of spawning `command`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Extra HTTP headers for the Streamable-HTTP transport (e.g. an
    /// `Authorization` token). Values support `${ENV_VAR}` expansion so secrets
    /// stay out of the config file.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Where the server was declared (`config`, `local`, `project`, `user`,
    /// `plugin:<name>`). Not persisted.
    #[serde(skip)]
    pub origin: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryConfig {
    pub file_path: PathBuf,
    /// Auto memory: the agent saves corrections, preferences and
    /// non-obvious facts without being asked (Claude Code's auto memory).
    #[serde(default = "default_true")]
    pub auto: bool,
    #[serde(default = "default_max_memory_notes")]
    pub max_notes: usize,
    #[serde(default)]
    pub auto_compact_on_finish: bool,
    /// Context management configuration.
    #[serde(default)]
    pub context: ContextConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextConfig {
    /// Target context window size (approximate tokens).
    /// Set to 0 for auto-detection based on model, or specify a custom value.
    /// Auto-detection uses known model context windows.
    #[serde(default)]
    pub context_window_target: usize,
    /// Use model-specific context window detection.
    /// When true, ignores context_window_target and auto-detects from model name.
    #[serde(default = "default_true")]
    pub auto_detect_context_window: bool,
    /// When true with `auto_detect_context_window`, query the active provider's models API
    /// before falling back to built-in tables. OpenRouter's catalog is public; OpenAI and
    /// Anthropic require configured API keys. Set `DCODE_AI_SKIP_CONTEXT_API=1` to disable at runtime.
    /// Catalog responses are cached in-process; override TTL with `DCODE_AI_CONTEXT_API_CACHE_TTL_SECS`.
    #[serde(default = "default_true")]
    pub query_provider_models_api: bool,
    /// Maximum messages to retain after compaction.
    #[serde(default = "default_max_retained_messages")]
    pub max_retained_messages: usize,
    /// Percentage of context window that triggers auto-summarize (0-100).
    #[serde(default = "default_summarize_threshold")]
    pub auto_summarize_threshold: u8,
    /// Compact automatically when the context fills up.
    #[serde(default = "default_true", alias = "auto_compact")]
    pub enable_auto_summarize: bool,
    /// Where automatic compaction happens, as % of the window. 0 = automatic:
    /// the window minus room for the reply and the summary (~75–85%).
    #[serde(default)]
    pub auto_compact_at: u8,
    /// Before summarizing, clear old tool outputs (no model call).
    #[serde(default = "default_true")]
    pub prune_tool_outputs: bool,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            context_window_target: 0, // 0 means auto-detect
            auto_detect_context_window: true,
            query_provider_models_api: true,
            max_retained_messages: default_max_retained_messages(),
            auto_summarize_threshold: default_summarize_threshold(),
            enable_auto_summarize: default_true(),
            auto_compact_at: 0,
            prune_tool_outputs: true,
        }
    }
}

fn default_summarize_threshold() -> u8 {
    // Compacting a little earlier keeps every later step cheaper.
    70
}

fn default_max_retained_messages() -> usize {
    50
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HookConfig {
    #[serde(default)]
    pub session_start: Vec<HookCommand>,
    #[serde(default)]
    pub session_end: Vec<HookCommand>,
    #[serde(default)]
    pub pre_tool_use: Vec<HookCommand>,
    #[serde(default)]
    pub post_tool_use: Vec<HookCommand>,
    #[serde(default)]
    pub post_tool_failure: Vec<HookCommand>,
    #[serde(default)]
    pub approval_requested: Vec<HookCommand>,
    #[serde(default)]
    pub subagent_start: Vec<HookCommand>,
    #[serde(default)]
    pub subagent_stop: Vec<HookCommand>,
    /// Before a prompt reaches the model; a failing blocking hook stops it.
    #[serde(default)]
    pub user_prompt_submit: Vec<HookCommand>,
    /// After every turn.
    #[serde(default)]
    pub stop: Vec<HookCommand>,
    /// Before the conversation is compacted.
    #[serde(default)]
    pub pre_compact: Vec<HookCommand>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookCommand {
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    #[serde(default)]
    pub blocking: bool,
    /// `plugin:<name>` for hooks contributed by a plugin. Not persisted.
    #[serde(skip)]
    pub origin: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebConfig {
    pub timeout_secs: u64,
    pub max_fetch_chars: usize,
    pub default_search_limit: usize,
    pub user_agent: String,
    #[serde(default)]
    pub webhooks: Vec<WebhookConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct WebhookConfig {
    pub url: String,
    pub enabled_events: Vec<String>,
    pub secret_header: Option<String>,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self {
            timeout_secs: 15,
            max_fetch_chars: 25_000,
            default_search_limit: 5,
            user_agent: "dcode-ai/0.5 (+https://github.com/user/dcode-ai)".into(),
            webhooks: Vec::new(),
        }
    }
}

impl WebConfig {
    fn merge(&mut self, partial: PartialWebConfig) {
        if let Some(timeout_secs) = partial.timeout_secs {
            self.timeout_secs = timeout_secs;
        }
        if let Some(max_fetch_chars) = partial.max_fetch_chars {
            self.max_fetch_chars = max_fetch_chars;
        }
        if let Some(default_search_limit) = partial.default_search_limit {
            self.default_search_limit = default_search_limit;
        }
        if let Some(user_agent) = partial.user_agent {
            self.user_agent = user_agent;
        }
        if let Some(webhooks) = partial.webhooks {
            self.webhooks = webhooks;
        }
    }
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            built_in_enabled: true,
            project_instructions_path: PathBuf::from(".dcode-airc"),
            local_instructions_path: PathBuf::from(".dcode-ai/instructions.md"),
            skill_directories: default_skill_directories(),
            include_repo_map: false,
            append_system_prompt: None,
        }
    }
}

impl HarnessConfig {
    fn merge(&mut self, partial: PartialHarnessConfig) {
        if let Some(enabled) = partial.built_in_enabled {
            self.built_in_enabled = enabled;
        }
        if let Some(path) = partial.project_instructions_path {
            self.project_instructions_path = path;
        }
        if let Some(path) = partial.local_instructions_path {
            self.local_instructions_path = path;
        }
        if let Some(skill_directories) = partial.skill_directories {
            self.skill_directories = skill_directories;
        }
        if let Some(include_repo_map) = partial.include_repo_map {
            self.include_repo_map = include_repo_map;
        }
    }
}

impl McpConfig {
    fn merge(&mut self, partial: PartialMcpConfig) {
        if let Some(expose_in_safe_mode) = partial.expose_in_safe_mode {
            self.expose_in_safe_mode = expose_in_safe_mode;
        }
        if let Some(servers) = partial.servers {
            self.servers = servers;
        }
        if let Some(t) = partial.tool_search_threshold {
            self.tool_search_threshold = t;
        }
    }
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            file_path: PathBuf::from(".dcode-ai/memory.json"),
            auto: true,
            max_notes: default_max_memory_notes(),
            auto_compact_on_finish: false,
            context: ContextConfig::default(),
        }
    }
}

impl MemoryConfig {
    fn merge(&mut self, partial: PartialMemoryConfig) {
        if let Some(file_path) = partial.file_path {
            self.file_path = file_path;
        }
        if let Some(max_notes) = partial.max_notes {
            self.max_notes = max_notes;
        }
        if let Some(auto) = partial.auto {
            self.auto = auto;
        }
        if let Some(auto_compact_on_finish) = partial.auto_compact_on_finish {
            self.auto_compact_on_finish = auto_compact_on_finish;
        }
        if let Some(context) = partial.context {
            self.context.merge(context);
        }
    }
}

impl ContextConfig {
    fn merge(&mut self, partial: PartialContextConfig) {
        if let Some(auto_detect) = partial.auto_detect_context_window {
            self.auto_detect_context_window = auto_detect;
        }
        if let Some(context_window_target) = partial.context_window_target {
            self.context_window_target = context_window_target;
        }
        if let Some(max_retained_messages) = partial.max_retained_messages {
            self.max_retained_messages = max_retained_messages;
        }
        if let Some(auto_summarize_threshold) = partial.auto_summarize_threshold {
            self.auto_summarize_threshold = auto_summarize_threshold;
        }
        if let Some(enable_auto_summarize) = partial.enable_auto_summarize {
            self.enable_auto_summarize = enable_auto_summarize;
        }
        if let Some(v) = partial.auto_compact_at {
            self.auto_compact_at = v.min(95);
        }
        if let Some(v) = partial.prune_tool_outputs {
            self.prune_tool_outputs = v;
        }
        if let Some(query_provider_models_api) = partial.query_provider_models_api {
            self.query_provider_models_api = query_provider_models_api;
        }
    }
}

impl HookConfig {
    fn merge(&mut self, partial: PartialHookConfig) {
        if let Some(session_start) = partial.session_start {
            self.session_start = session_start;
        }
        if let Some(session_end) = partial.session_end {
            self.session_end = session_end;
        }
        if let Some(pre_tool_use) = partial.pre_tool_use {
            self.pre_tool_use = pre_tool_use;
        }
        if let Some(post_tool_use) = partial.post_tool_use {
            self.post_tool_use = post_tool_use;
        }
        if let Some(post_tool_failure) = partial.post_tool_failure {
            self.post_tool_failure = post_tool_failure;
        }
        if let Some(approval_requested) = partial.approval_requested {
            self.approval_requested = approval_requested;
        }
        if let Some(subagent_start) = partial.subagent_start {
            self.subagent_start = subagent_start;
        }
        if let Some(v) = partial.user_prompt_submit {
            self.user_prompt_submit = v;
        }
        if let Some(v) = partial.stop {
            self.stop = v;
        }
        if let Some(v) = partial.pre_compact {
            self.pre_compact = v;
        }
        if let Some(subagent_stop) = partial.subagent_stop {
            self.subagent_stop = subagent_stop;
        }
    }
}

fn default_turn_token_alarm() -> u64 {
    5_000_000
}

impl SessionConfig {
    fn merge(&mut self, partial: PartialSessionConfig) {
        if let Some(v) = partial.max_cost_usd {
            self.max_cost_usd = v.max(0.0);
        }
        if let Some(v) = partial.turn_token_alarm {
            self.turn_token_alarm = v;
        }
        if let Some(v) = partial.cleanup_period_days {
            self.cleanup_period_days = v;
        }
        if let Some(v) = partial.cross_review {
            self.cross_review = v;
        }
        if let Some(v) = partial.second_opinion_model {
            self.second_opinion_model = Some(v).filter(|m| !m.trim().is_empty());
        }
        if let Some(v) = partial.verify_after_edits {
            self.verify_after_edits = v;
        }
        if let Some(history_dir) = partial.history_dir {
            self.history_dir = history_dir;
        }
        if let Some(max_turns_per_run) = partial.max_turns_per_run {
            self.max_turns_per_run = max_turns_per_run;
        }
        if let Some(max_tool_calls_per_turn) = partial.max_tool_calls_per_turn {
            self.max_tool_calls_per_turn = max_tool_calls_per_turn;
        }
        if let Some(checkpoint_interval) = partial.checkpoint_interval {
            self.checkpoint_interval = checkpoint_interval;
        }
        if let Some(last_session_file) = partial.last_session_file {
            self.last_session_file = last_session_file;
        }
        if let Some(auto_compact_on_finish) = partial.auto_compact_on_finish {
            self.auto_compact_on_finish = auto_compact_on_finish;
        }
    }
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialDcodeAiConfig {
    provider: Option<PartialProviderConfig>,
    model: Option<PartialModelConfig>,
    permissions: Option<PartialPermissionConfig>,
    session: Option<PartialSessionConfig>,
    harness: Option<PartialHarnessConfig>,
    mcp: Option<PartialMcpConfig>,
    memory: Option<PartialMemoryConfig>,
    hooks: Option<PartialHookConfig>,
    web: Option<PartialWebConfig>,
    ui: Option<PartialUiConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialUiConfig {
    editor: Option<String>,
    theme: Option<String>,
    hide_tips: Option<bool>,
    vim_mode: Option<bool>,
    quiet_startup: Option<bool>,
    scroll_speed: Option<u16>,
    mouse_capture: Option<bool>,
    code_line_numbers: Option<bool>,
    onboarding_completed: Option<bool>,
    personality: Option<String>,
    output_style: Option<String>,
    statusline_hidden: Option<Vec<String>>,
    statusline_command: Option<String>,
    keymap: Option<BTreeMap<String, String>>,
    notifications: Option<bool>,
    working_style: Option<String>,
    shimmer: Option<bool>,
    sounds: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialProviderConfig {
    /// Parsed leniently: names of removed providers must not break loading.
    default: Option<String>,
    /// Only read to migrate the legacy Copilot-via-OpenAI setup.
    openai: Option<PartialLegacyOpenAiConfig>,
    anthropic: Option<PartialAnthropicConfig>,
    copilot: Option<PartialCopilotConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialLegacyOpenAiConfig {
    base_url: Option<String>,
    model: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialCopilotConfig {
    api_key_env: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    temperature: Option<f32>,
    editor_version: Option<String>,
    editor_plugin_version: Option<String>,
    integration_id: Option<String>,
    user_agent: Option<String>,
    reasoning_effort: Option<String>,
    oauth_client_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialAnthropicConfig {
    api_key_env: Option<String>,
    api_key: Option<String>,
    base_url: Option<String>,
    model: Option<String>,
    temperature: Option<f32>,
    effort: Option<String>,
    oauth_profile: Option<String>,
    backend: Option<AnthropicBackend>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialModelConfig {
    default_model: Option<String>,
    max_tokens: Option<u32>,
    enable_thinking: Option<bool>,
    thinking_budget: Option<u32>,
    aliases: Option<BTreeMap<String, String>>,
    recent_models: Option<Vec<String>>,
    compact_model: Option<String>,
    subagent_model: Option<String>,
    fallback_models: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialPermissionConfig {
    mode: Option<PermissionMode>,
    allow: Option<Vec<String>>,
    deny: Option<Vec<String>>,
    ask: Option<Vec<String>>,
    startup_approve_all: Option<bool>,
    sandbox_bash: Option<bool>,
    sandbox_writable_roots: Option<Vec<String>>,
    additional_directories: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialSessionConfig {
    history_dir: Option<PathBuf>,
    #[serde(alias = "max_turn_per_run")]
    max_turns_per_run: Option<u32>,
    max_tool_calls_per_turn: Option<u32>,
    checkpoint_interval: Option<u32>,
    last_session_file: Option<PathBuf>,
    auto_compact_on_finish: Option<bool>,
    max_cost_usd: Option<f64>,
    turn_token_alarm: Option<u64>,
    verify_after_edits: Option<bool>,
    cross_review: Option<bool>,
    second_opinion_model: Option<String>,
    cleanup_period_days: Option<u32>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialHarnessConfig {
    built_in_enabled: Option<bool>,
    project_instructions_path: Option<PathBuf>,
    local_instructions_path: Option<PathBuf>,
    skill_directories: Option<Vec<PathBuf>>,
    include_repo_map: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialMcpConfig {
    expose_in_safe_mode: Option<bool>,
    servers: Option<Vec<McpServerConfig>>,
    tool_search_threshold: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialMemoryConfig {
    file_path: Option<PathBuf>,
    auto: Option<bool>,
    max_notes: Option<usize>,
    auto_compact_on_finish: Option<bool>,
    context: Option<PartialContextConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialContextConfig {
    context_window_target: Option<usize>,
    auto_detect_context_window: Option<bool>,
    query_provider_models_api: Option<bool>,
    max_retained_messages: Option<usize>,
    auto_summarize_threshold: Option<u8>,
    #[serde(alias = "auto_compact")]
    enable_auto_summarize: Option<bool>,
    auto_compact_at: Option<u8>,
    prune_tool_outputs: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialHookConfig {
    session_start: Option<Vec<HookCommand>>,
    session_end: Option<Vec<HookCommand>>,
    pre_tool_use: Option<Vec<HookCommand>>,
    post_tool_use: Option<Vec<HookCommand>>,
    post_tool_failure: Option<Vec<HookCommand>>,
    approval_requested: Option<Vec<HookCommand>>,
    subagent_start: Option<Vec<HookCommand>>,
    subagent_stop: Option<Vec<HookCommand>>,
    user_prompt_submit: Option<Vec<HookCommand>>,
    stop: Option<Vec<HookCommand>>,
    pre_compact: Option<Vec<HookCommand>>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialWebConfig {
    timeout_secs: Option<u64>,
    max_fetch_chars: Option<usize>,
    default_search_limit: Option<usize>,
    user_agent: Option<String>,
    webhooks: Option<Vec<WebhookConfig>>,
}

fn default_true() -> bool {
    true
}

fn is_false(v: &bool) -> bool {
    !v
}

fn default_max_memory_notes() -> usize {
    128
}

fn resolve_api_key_value(inline: &Option<String>, env_name: &str) -> Option<String> {
    inline
        .as_deref()
        .filter(|v| !v.trim().is_empty())
        .map(String::from)
        .or_else(|| env::var(env_name).ok())
        .filter(|v| !v.trim().is_empty())
        // Last: the 0600 credentials store (~/.dcode-ai/credentials.toml),
        // keyed by the same env name.
        .or_else(|| crate::credentials::get(env_name))
}

fn default_skill_directories() -> Vec<PathBuf> {
    vec![
        PathBuf::from(".dcode-ai/skills"),
        PathBuf::from(".claude/skills"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permission_rules_edit_keeps_the_rest_of_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.local.toml");
        std::fs::write(&path, "[model]\ndefault_model = \"x\"\n").unwrap();
        assert!(edit_permission_rule(&path, "allow", "execute_bash:cargo *", true).unwrap());
        assert!(!edit_permission_rule(&path, "allow", "execute_bash:cargo *", true).unwrap());
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("default_model"));
        assert!(text.contains("execute_bash:cargo *"));
        assert!(edit_permission_rule(&path, "allow", "execute_bash:cargo *", false).unwrap());
        assert!(!std::fs::read_to_string(&path).unwrap().contains("cargo"));
        assert!(edit_permission_rule(&path, "maybe", "x", true).is_err());
    }

    #[test]
    fn session_accepts_max_turn_per_run_typo_alias() {
        let raw = r#"
            [session]
            max_turn_per_run = 99
        "#;
        let partial: PartialDcodeAiConfig = toml::from_str(raw).expect("parse");
        let session = partial.session.expect("session table");
        assert_eq!(session.max_turns_per_run, Some(99));
    }

    #[test]
    fn apply_model_override_updates_selected_provider_model() {
        let mut config = DcodeAiConfig::default();
        config.provider.default = ProviderKind::Copilot;
        config.sync_default_model_from_provider();
        config
            .model
            .aliases
            .insert("fast".into(), "provider-model".into());

        config.apply_model_override("fast");

        assert_eq!(config.provider.copilot.model, "provider-model");
        assert_eq!(config.model.default_model, "provider-model");
    }

    #[test]
    fn apply_env_selects_provider_and_anthropic_settings() {
        let _guard = EnvGuard::set(&[
            ("DCODE_AI_DEFAULT_PROVIDER", Some("anthropic")),
            ("ANTHROPIC_API_KEY", Some("anthropic-key")),
            ("ANTHROPIC_MODEL", Some("claude-sonnet-5-5")),
        ]);

        let mut config = DcodeAiConfig::default();
        config.provider.default = ProviderKind::Copilot;
        config.apply_env();

        assert_eq!(config.provider.default, ProviderKind::Anthropic);
        assert_eq!(
            config.provider.anthropic.resolve_api_key().as_deref(),
            Some("anthropic-key")
        );
        assert_eq!(config.model.default_model, "claude-sonnet-5-5");
    }

    struct EnvGuard {
        previous: Vec<(String, Option<String>)>,
    }

    impl EnvGuard {
        fn set(vars: &[(&str, Option<&str>)]) -> Self {
            let mut previous = Vec::new();
            for (key, value) in vars {
                previous.push((key.to_string(), env::var(key).ok()));
                match value {
                    Some(value) => unsafe { env::set_var(key, value) },
                    None => unsafe { env::remove_var(key) },
                }
            }
            Self { previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.previous.drain(..) {
                match value {
                    Some(value) => unsafe { env::set_var(&key, value) },
                    None => unsafe { env::remove_var(&key) },
                }
            }
        }
    }

    #[test]
    fn workspace_cache_id_stable_for_same_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        let (id1, p1) = workspace_cache_id(dir.path()).expect("id");
        let (id2, p2) = workspace_cache_id(dir.path()).expect("id");
        assert_eq!(id1, id2);
        assert_eq!(p1, p2);
        assert!(id1.contains('-'));
        assert!(id1.len() > 16);
    }

    #[test]
    fn ui_editor_roundtrips_through_workspace_file() {
        let _guard = EnvGuard::set(&[("DCODE_AI_EDITOR", None), ("EDITOR", None)]);
        let dir = tempfile::tempdir().expect("tempdir");
        let mut config = DcodeAiConfig::default();
        config.ui.editor = Some("vim".into());
        config.set_default_provider(ProviderKind::Anthropic);
        config.save_workspace_file(dir.path()).expect("save");

        let loaded = DcodeAiConfig::load_for_workspace(dir.path()).expect("load");
        assert_eq!(loaded.ui.editor.as_deref(), Some("vim"));
        assert_eq!(loaded.effective_editor_command(), "vim");
    }

    #[test]
    fn provider_kind_from_cli_name() {
        assert_eq!(
            ProviderKind::from_cli_name("Anthropic"),
            Some(ProviderKind::Anthropic)
        );
        assert_eq!(
            ProviderKind::from_cli_name("copilot"),
            Some(ProviderKind::Copilot)
        );
        assert_eq!(ProviderKind::from_cli_name("openai"), None);
    }

    #[test]
    fn onboarding_completed_defaults_to_false() {
        let config = DcodeAiConfig::default();
        assert!(!config.ui.onboarding_completed);
    }

    #[test]
    fn mouse_capture_defaults_to_false() {
        let config = DcodeAiConfig::default();
        assert!(!config.ui.mouse_capture);
    }

    #[test]
    fn mouse_capture_merges_from_partial() {
        let mut config = DcodeAiConfig::default();
        let toml_str = r#"
[ui]
mouse_capture = true
"#;
        let partial: PartialDcodeAiConfig = toml::from_str(toml_str).unwrap();
        config.merge(partial);
        assert!(config.ui.mouse_capture);
    }

    #[test]
    fn code_line_numbers_defaults_to_false() {
        let config = DcodeAiConfig::default();
        assert!(!config.ui.code_line_numbers);
    }

    #[test]
    fn code_line_numbers_merges_from_partial() {
        let mut config = DcodeAiConfig::default();
        let toml_str = r#"
[ui]
code_line_numbers = true
"#;
        let partial: PartialDcodeAiConfig = toml::from_str(toml_str).unwrap();
        config.merge(partial);
        assert!(config.ui.code_line_numbers);
    }

    #[test]
    fn onboarding_completed_merges_from_partial() {
        let mut config = DcodeAiConfig::default();
        let toml_str = r#"
[ui]
onboarding_completed = true
"#;
        let partial: PartialDcodeAiConfig = toml::from_str(toml_str).unwrap();
        config.merge(partial);
        assert!(config.ui.onboarding_completed);
    }

    #[test]
    fn any_api_key_present_returns_false_when_no_keys() {
        let config = config_without_env_keys();
        assert!(!config.provider.any_api_key_present());
    }

    #[test]
    fn any_api_key_present_returns_true_when_one_key_set() {
        let mut config = DcodeAiConfig::default();
        config.provider.anthropic.api_key = Some("sk-ant-test".into());
        assert!(config.provider.any_api_key_present());
    }

    /// Returns an DcodeAiConfig with env var fallbacks disabled so tests don't
    /// pick up real API keys from the shell environment.
    fn config_without_env_keys() -> DcodeAiConfig {
        let mut config = DcodeAiConfig::default();
        config.provider.anthropic.api_key_env = "__DCODE_AI_TEST_NONE__".into();
        config.provider.copilot.api_key_env = "__DCODE_AI_TEST_NONE__".into();
        config
    }

    #[test]
    fn needs_onboarding_true_when_no_flag_and_no_keys() {
        let config = config_without_env_keys();
        assert!(config.needs_onboarding());
    }

    #[test]
    fn needs_onboarding_false_when_flag_set_and_key_present() {
        let mut config = DcodeAiConfig::default();
        config.ui.onboarding_completed = true;
        config.provider.anthropic.api_key = Some("test-key".into());
        assert!(!config.needs_onboarding());
    }

    #[test]
    fn needs_onboarding_true_when_flag_set_but_all_keys_removed() {
        let mut config = config_without_env_keys();
        config.ui.onboarding_completed = true;
        // no keys set — safety net triggers
        assert!(config.needs_onboarding());
    }

    #[test]
    fn needs_onboarding_true_when_key_present_but_flag_not_set() {
        let mut config = DcodeAiConfig::default();
        config.provider.anthropic.api_key = Some("sk-ant-test".into());
        // onboarding_completed is false
        assert!(config.needs_onboarding());
    }

    #[test]
    fn onboarding_roundtrip_through_toml() {
        let toml_str = r#"
[ui]
onboarding_completed = true

[provider.anthropic]
api_key = "test-key"
"#;
        let partial: PartialDcodeAiConfig = toml::from_str(toml_str).unwrap();
        let mut config = DcodeAiConfig::default();
        config.merge(partial);
        assert!(!config.needs_onboarding());
    }

    #[test]
    fn onboarding_triggers_when_key_removed_after_completion() {
        let toml_str = r#"
[ui]
onboarding_completed = true
"#;
        let partial: PartialDcodeAiConfig = toml::from_str(toml_str).unwrap();
        let mut config = config_without_env_keys();
        config.merge(partial);
        assert!(config.needs_onboarding());
    }

    #[test]
    fn legacy_and_removed_provider_configs_still_load() {
        // Copilot used to be `default = "openai"` + a githubcopilot base_url.
        let partial: PartialProviderConfig = toml::from_str(
            r#"
default = "openai"
[openai]
base_url = "https://api.githubcopilot.com"
model = "claude-sonnet-4.5"
"#,
        )
        .expect("parse");
        let mut provider = ProviderConfig::default();
        provider.merge(partial);
        assert_eq!(provider.default, ProviderKind::Copilot);
        assert_eq!(provider.copilot.model, "claude-sonnet-4.5");

        // A removed provider (and its sections) must not break loading.
        let partial: PartialDcodeAiConfig = toml::from_str(
            r#"
[provider]
default = "antigravityoauth"
[provider.openrouter]
api_key = "x"
[provider.opencodezen]
model = "MiniMax-M2.5"
"#,
        )
        .expect("parse");
        let mut config = DcodeAiConfig::default();
        config.merge(partial);
        assert_eq!(config.provider.default, ProviderKind::Anthropic);
    }

    #[test]
    fn invalid_dated_claude_ids_from_old_defaults_are_migrated() {
        let partial: PartialProviderConfig =
            toml::from_str("[anthropic]\nmodel = \"claude-sonnet-4-6-20250514\"\n").expect("parse");
        let mut provider = ProviderConfig::default();
        provider.merge(partial);
        assert_eq!(provider.anthropic.model, "claude-sonnet-4-6");
        assert_eq!(
            migrate_claude_model_id("claude-opus-5-5"),
            "claude-opus-5-5"
        );
    }
}

#[cfg(test)]
mod routing_tests {
    use super::*;

    #[test]
    fn model_routing_settings_load() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        std::fs::write(
            &path,
            "[model]\ncompact_model = \"haiku\"\nsubagent_model = \"sonnet\"\n[session]\nmax_cost_usd = 2.5\n",
        )
        .unwrap();
        let mut config = DcodeAiConfig::default();
        config.merge(load_partial(&path).unwrap());
        assert_eq!(config.model.compact_model.as_deref(), Some("haiku"));
        assert_eq!(config.model.subagent_model.as_deref(), Some("sonnet"));
        assert_eq!(config.session.max_cost_usd, 2.5);
        assert_eq!(config.model.resolve_alias("haiku"), "claude-haiku-5-5");
    }
}
