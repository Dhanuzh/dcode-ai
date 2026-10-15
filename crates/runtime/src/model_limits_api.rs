//! Live model catalogs and context-window lookups for the active provider.
//!
//! - Claude: `ant models list` (Anthropic's CLI, signed in with OAuth) gives
//!   ids plus `max_input_tokens` / `max_tokens`. Works without API credits.
//!   Without `ant`, the Claude Code backend falls back to the aliases Claude
//!   Code itself resolves (`opus`, `sonnet`, `haiku`).
//! - GitHub Copilot: `GET {api}/models` with the GitHub login.
//!
//! Context windows found this way are persisted to disk so a cold start never
//! waits on the network. Model-id catalogs are cached in the temp dir for
//! `DCODE_AI_CONTEXT_API_CACHE_TTL_SECS` (default `3600`). Set
//! `DCODE_AI_SKIP_CONTEXT_API=1` to disable lookups entirely.

use crate::model_limits::ModelLimits;
use dcode_ai_common::config::{AnthropicBackend, DcodeAiConfig, ProviderKind};
use serde::Deserialize;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::time::Duration;

fn catalog_cache_ttl() -> Duration {
    std::env::var("DCODE_AI_CONTEXT_API_CACHE_TTL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(3600))
}

fn secret_tag(secret: &str) -> u64 {
    let mut h = DefaultHasher::new();
    secret.hash(&mut h);
    h.finish()
}

// --- On-disk context-window cache -------------------------------------------
//
// Persists the last-known context window per (provider, model) so startup can
// read it instantly (no network); a background refresh keeps it current.

fn disk_cache_path() -> Option<PathBuf> {
    dcode_ai_common::config::dcode_ai_home_dir().map(|d| d.join("model_limits-v2.json"))
}

fn disk_cache_key(config: &DcodeAiConfig, model: &str) -> String {
    format!("{:?}/{model}", config.provider.default)
}

/// Last-known context window for `(provider, model)` from the on-disk cache, or
/// `None`. Pure disk read — safe on the startup path, never touches the network.
pub fn cached_context_window(config: &DcodeAiConfig, model: &str) -> Option<usize> {
    let path = disk_cache_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    let map: HashMap<String, usize> = serde_json::from_str(&text).ok()?;
    map.get(&disk_cache_key(config, model)).copied()
}

fn persist_context_window(config: &DcodeAiConfig, model: &str, window: usize) {
    let Some(path) = disk_cache_path() else {
        return;
    };
    let mut map: HashMap<String, usize> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    if map.get(&disk_cache_key(config, model)) == Some(&window) {
        return;
    }
    map.insert(disk_cache_key(config, model), window);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(&map) {
        let _ = std::fs::write(path, text);
    }
}

/// Look up the model's limits and persist the context window for the next
/// launch. Spawned in the background so it never blocks startup.
pub async fn refresh_and_persist(config: DcodeAiConfig, model: String) {
    let limits = resolve_model_limits(&config, &model).await;
    persist_context_window(&config, &model, limits.context_window);
}

/// Model limits from the provider when it publishes them, else the static table.
pub async fn resolve_model_limits(config: &DcodeAiConfig, model: &str) -> ModelLimits {
    let static_limits = ModelLimits::for_model(model);
    if std::env::var("DCODE_AI_SKIP_CONTEXT_API").ok().as_deref() == Some("1")
        || !config.memory.context.auto_detect_context_window
        || !config.memory.context.query_provider_models_api
    {
        return static_limits;
    }
    match config.provider.default {
        ProviderKind::Anthropic if dcode_ai_common::provider_runtime::has_ant_cli() => {
            let wanted = normalize_claude_id(model);
            match list_ant_models(config.provider.anthropic.oauth_profile.as_deref()).await {
                Ok(models) => models
                    .into_iter()
                    .find(|m| m.id == wanted)
                    .and_then(|m| {
                        Some(ModelLimits {
                            context_window: m.max_input_tokens? as usize,
                            max_output_tokens: m
                                .max_tokens
                                .map(|t| t as usize)
                                .unwrap_or(static_limits.max_output_tokens),
                        })
                    })
                    .unwrap_or(static_limits),
                Err(err) => {
                    tracing::debug!("context API: {err}");
                    static_limits
                }
            }
        }
        // Copilot's catalog has no reliable per-plan limits; use the table.
        _ => static_limits,
    }
}

fn normalize_claude_id(model: &str) -> String {
    let m = model.trim();
    if m.to_ascii_lowercase().starts_with("claude-") {
        m.replace('.', "-")
    } else {
        m.to_string()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ModelCatalogError {
    #[error("{provider} model discovery requires authentication: {message}")]
    Authentication {
        provider: &'static str,
        message: String,
    },
    #[error("{provider} model discovery request failed: {message}")]
    Request {
        provider: &'static str,
        message: String,
    },
    #[error("{provider} returned an invalid model catalog: {message}")]
    InvalidResponse {
        provider: &'static str,
        message: String,
    },
    #[error("{provider} returned an empty model catalog")]
    Empty { provider: &'static str },
}

/// Fetch available model ids from the active provider.
///
/// Failures are reported, never papered over with a stale list. Successful
/// results are cached on disk so a cold start within the TTL skips the network.
pub async fn fetch_provider_model_ids(
    config: &DcodeAiConfig,
) -> Result<Vec<String>, ModelCatalogError> {
    let provider = config.provider.default;
    let cache_tag = disk_cache_tag(config);
    if let Some(ids) = disk_cache_read(&cache_tag) {
        tracing::debug!(provider = %provider.display_name(), "model catalog loaded from disk cache");
        return Ok(ids);
    }
    let ids = match provider {
        ProviderKind::Anthropic => fetch_claude_model_ids(config).await?,
        ProviderKind::Copilot => fetch_copilot_model_ids(config).await?,
    };
    let ids = finish_catalog(provider.display_name(), ids)?;
    disk_cache_write(&cache_tag, &ids);
    Ok(ids)
}

async fn fetch_claude_model_ids(config: &DcodeAiConfig) -> Result<Vec<String>, ModelCatalogError> {
    let anthropic = &config.provider.anthropic;
    let via_ant = if dcode_ai_common::provider_runtime::has_ant_cli() {
        list_ant_models(anthropic.oauth_profile.as_deref())
            .await
            .map(|models| models.into_iter().map(|m| m.id).collect::<Vec<_>>())
    } else {
        Err("`ant` is not installed".to_string())
    };
    match via_ant {
        Ok(ids) if !ids.is_empty() => Ok(ids),
        // Claude Code resolves these aliases to its current models itself.
        _ if anthropic.backend == AnthropicBackend::ClaudeCode => {
            Ok(vec!["opus".into(), "sonnet".into(), "haiku".into()])
        }
        Ok(_) => Err(ModelCatalogError::InvalidResponse {
            provider: "Claude",
            message: "`ant models list` returned no models".into(),
        }),
        Err(message) => Err(ModelCatalogError::Authentication {
            provider: "Claude",
            message: format!("{message}; run `dcode-ai login anthropic`"),
        }),
    }
}

async fn fetch_copilot_model_ids(config: &DcodeAiConfig) -> Result<Vec<String>, ModelCatalogError> {
    use dcode_ai_core::provider::ProviderError;
    dcode_ai_core::provider::copilot::list_copilot_models(&config.provider.copilot)
        .await
        .map_err(|error| match error {
            ProviderError::AuthError(message) | ProviderError::Configuration(message) => {
                ModelCatalogError::Authentication {
                    provider: "GitHub Copilot",
                    message,
                }
            }
            other => ModelCatalogError::Request {
                provider: "GitHub Copilot",
                message: other.to_string(),
            },
        })
}

#[derive(Debug, Clone, Deserialize)]
struct AntModel {
    id: String,
    #[serde(default)]
    max_input_tokens: Option<u64>,
    #[serde(default)]
    max_tokens: Option<u64>,
}

async fn list_ant_models(profile: Option<&str>) -> Result<Vec<AntModel>, String> {
    let mut cmd = dcode_ai_common::provider_runtime::windows_compat_command_tokio("ant");
    cmd.args(["models", "list", "--format", "jsonl"])
        .stdin(std::process::Stdio::null());
    if let Some(profile) = profile {
        cmd.env("ANTHROPIC_PROFILE", profile);
    }
    let output = tokio::time::timeout(Duration::from_secs(30), cmd.output())
        .await
        .map_err(|_| "`ant models list` timed out".to_string())?
        .map_err(|e| format!("failed to run `ant`: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "`ant models list` failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(parse_ant_models(&String::from_utf8_lossy(&output.stdout)))
}

fn parse_ant_models(jsonl: &str) -> Vec<AntModel> {
    jsonl
        .lines()
        .filter_map(|line| serde_json::from_str::<AntModel>(line).ok())
        .collect()
}

fn finish_catalog(
    provider: &'static str,
    mut ids: Vec<String>,
) -> Result<Vec<String>, ModelCatalogError> {
    ids.retain(|id| !id.trim().is_empty());
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        Err(ModelCatalogError::Empty { provider })
    } else {
        Ok(ids)
    }
}

// ── Disk cache for model id catalogs ──────────────────────────────────────────

fn disk_cache_dir() -> PathBuf {
    std::env::temp_dir().join("dcode-ai-model-cache-v3")
}

fn disk_cache_tag(config: &DcodeAiConfig) -> String {
    let provider = config.provider.default;
    let account_hint = match provider {
        ProviderKind::Copilot => config
            .provider
            .copilot
            .resolve_github_token()
            .or_else(|| {
                dcode_ai_common::auth::AuthStore::load()
                    .ok()
                    .and_then(|s| s.copilot)
                    .map(|c| c.github_token)
            })
            .map(|k| secret_tag(&k))
            .unwrap_or(0),
        ProviderKind::Anthropic => secret_tag(&format!(
            "{:?}/{}",
            config.provider.anthropic.backend,
            config
                .provider
                .anthropic
                .oauth_profile
                .as_deref()
                .unwrap_or("")
        )),
    };
    format!("{}-{account_hint:x}", provider.to_config_key())
}

#[derive(Deserialize)]
struct DiskCacheEntry {
    ts: u64,
    ids: Vec<String>,
}

fn disk_cache_read(tag: &str) -> Option<Vec<String>> {
    let path = disk_cache_dir().join(format!("{tag}.json"));
    let raw = std::fs::read_to_string(&path).ok()?;
    let entry: DiskCacheEntry = serde_json::from_str(&raw).ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    if now.saturating_sub(entry.ts) > catalog_cache_ttl().as_secs() || entry.ids.is_empty() {
        return None;
    }
    Some(entry.ids)
}

fn disk_cache_write(tag: &str, ids: &[String]) {
    let dir = disk_cache_dir();
    let _ = std::fs::create_dir_all(&dir);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let json = serde_json::json!({ "ts": ts, "ids": ids });
    let _ = std::fs::write(dir.join(format!("{tag}.json")), json.to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_live_catalog_fails_loudly() {
        assert!(matches!(
            finish_catalog("test", vec![" ".into()]),
            Err(ModelCatalogError::Empty { provider: "test" })
        ));
    }

    #[test]
    fn live_catalog_is_sorted_and_deduplicated() {
        let ids = finish_catalog(
            "test",
            vec!["second".into(), "first".into(), "second".into()],
        )
        .expect("catalog");
        assert_eq!(ids, ["first", "second"]);
    }

    #[test]
    fn ant_jsonl_models_are_parsed_with_limits() {
        let models = parse_ant_models(
            "{\"type\":\"model\",\"id\":\"claude-opus-5-5\",\"max_input_tokens\":1000000,\"max_tokens\":128000}\nnot json\n{\"id\":\"claude-haiku-5-5\"}\n",
        );
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "claude-opus-5-5");
        assert_eq!(models[0].max_input_tokens, Some(1_000_000));
        assert_eq!(models[1].max_tokens, None);
    }

    #[test]
    fn dotted_claude_ids_match_catalog_ids() {
        assert_eq!(
            normalize_claude_id("claude-sonnet-4.6"),
            "claude-sonnet-4-6"
        );
        assert_eq!(normalize_claude_id("gpt-4.1"), "gpt-4.1");
    }
}
