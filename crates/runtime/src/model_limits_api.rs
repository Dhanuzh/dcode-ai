//! Optional provider HTTP lookups for context window sizes.
//!
//! - [OpenRouter](https://openrouter.ai/docs/api-reference/models/get-models): public `GET .../models`
//!   with `context_length` per model.
//! - Anthropic: `GET /v1/models` (requires API key); entries may include `max_input_tokens`.
//! - OpenAI: `GET /v1/models` (requires key); `context_window` is present on some responses.
//!
//! Successful catalog responses are cached in memory per process. Tune with
//! `DCODE_AI_CONTEXT_API_CACHE_TTL_SECS` (default `3600`). Use `DCODE_AI_SKIP_CONTEXT_API=1` to disable
//! lookups entirely.

use crate::model_limits::ModelLimits;
use dcode_ai_common::config::{DcodeAiConfig, ProviderKind};
use serde::Deserialize;
use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

const HTTP_TIMEOUT_SECS: u64 = 12;

fn catalog_cache_ttl() -> Duration {
    std::env::var("DCODE_AI_CONTEXT_API_CACHE_TTL_SECS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|&n| n > 0)
        .map(Duration::from_secs)
        .unwrap_or_else(|| Duration::from_secs(3600))
}

fn api_key_tag(secret: &str) -> u64 {
    let mut h = DefaultHasher::new();
    secret.hash(&mut h);
    h.finish()
}

fn cache_stale(fetched_at: Instant, ttl: Duration) -> bool {
    fetched_at.elapsed() >= ttl
}

// --- On-disk context-window cache -------------------------------------------
//
// The in-memory catalog caches above are empty on every fresh process, so
// without this a cold start would block on the `/models` API. This persists
// the last-known context window per (provider, model) to disk so startup can
// read it instantly (no network) and a background refresh keeps it current for
// the next launch — the same "static/cached value, refreshed out-of-band"
// model Codex uses.

fn disk_cache_path() -> Option<PathBuf> {
    // `-v2`: drops pre-fix cached context windows (e.g. the stale 128k persisted
    // for Antigravity `gemini-3-*` before they were recognized) so the corrected
    // large window applies on the very next launch, not the one after.
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
        return; // unchanged — avoid a pointless rewrite
    }
    map.insert(disk_cache_key(config, model), window);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(text) = serde_json::to_string_pretty(&map) {
        let _ = std::fs::write(path, text);
    }
}

/// Fetch the model's context window from the provider API and persist it to the
/// on-disk cache for the next launch. Intended to be spawned in the background
/// so it never blocks startup.
pub async fn refresh_and_persist(config: DcodeAiConfig, model: String) {
    let limits = resolve_model_limits(&config, &model).await;
    persist_context_window(&config, &model, limits.context_window);
}

// --- OpenRouter ---

struct OpenRouterCatalogEntry {
    url: String,
    fetched_at: Instant,
    models: Arc<Vec<OpenRouterModel>>,
}

fn openrouter_catalog_cache() -> &'static Mutex<Option<OpenRouterCatalogEntry>> {
    static CELL: OnceLock<Mutex<Option<OpenRouterCatalogEntry>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

// --- Anthropic ---

struct AnthropicCatalogEntry {
    cache_key: String,
    fetched_at: Instant,
    models: Arc<Vec<AnthropicModel>>,
}

fn anthropic_catalog_cache() -> &'static Mutex<Option<AnthropicCatalogEntry>> {
    static CELL: OnceLock<Mutex<Option<AnthropicCatalogEntry>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

// --- OpenAI ---

struct OpenAiCatalogEntry {
    cache_key: String,
    fetched_at: Instant,
    value: Arc<serde_json::Value>,
}

fn openai_catalog_cache() -> &'static Mutex<Option<OpenAiCatalogEntry>> {
    static CELL: OnceLock<Mutex<Option<OpenAiCatalogEntry>>> = OnceLock::new();
    CELL.get_or_init(|| Mutex::new(None))
}

pub async fn resolve_model_limits(config: &DcodeAiConfig, model: &str) -> ModelLimits {
    let static_limits = ModelLimits::for_model(model);

    if std::env::var("DCODE_AI_SKIP_CONTEXT_API").ok().as_deref() == Some("1") {
        return static_limits;
    }

    if !config.memory.context.auto_detect_context_window
        || !config.memory.context.query_provider_models_api
    {
        return static_limits;
    }

    let client = match http_client() {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!(error = %e, "context API: failed to build HTTP client");
            return static_limits;
        }
    };

    let from_api = match config.provider.default {
        ProviderKind::OpenRouter => {
            let base = config.provider.openrouter.base_url.trim_end_matches('/');
            let url = format!("{base}/v1/models");
            let key = config.provider.openrouter.resolve_api_key();
            fetch_openrouter_context(&client, &url, model, key.as_deref()).await
        }
        ProviderKind::Anthropic => {
            let key = match config.provider.anthropic.resolve_api_key() {
                Some(k) => k,
                None => {
                    tracing::debug!("context API: anthropic selected but no API key");
                    return static_limits;
                }
            };
            let base = config.provider.anthropic.base_url.trim_end_matches('/');
            fetch_anthropic_context(&client, base, &key, model).await
        }
        ProviderKind::OpenAi | ProviderKind::Antigravity => {
            let key = match config.provider.openai.resolve_api_key() {
                Some(k) => k,
                None => {
                    tracing::debug!("context API: openai selected but no API key");
                    return static_limits;
                }
            };
            let base = config.provider.openai.base_url.trim_end_matches('/');
            fetch_openai_context(&client, base, &key, model).await
        }
        ProviderKind::OpenCodeZen => {
            let key = match config.provider.opencodezen.resolve_api_key() {
                Some(k) => k,
                None => {
                    tracing::debug!("context API: opencodezen selected but no API key");
                    return static_limits;
                }
            };
            let base = config.provider.opencodezen.base_url.trim_end_matches('/');
            fetch_openai_context(&client, base, &key, model).await
        }
    };

    match from_api {
        Some(cw) if cw > 0 => {
            tracing::info!(
                model = %model,
                context_window = cw,
                "context window from provider models API"
            );
            ModelLimits {
                context_window: cw,
                max_output_tokens: static_limits.max_output_tokens,
            }
        }
        _ => static_limits,
    }
}

fn http_client() -> Result<reqwest::Client, reqwest::Error> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(HTTP_TIMEOUT_SECS))
        .user_agent(concat!(
            "dcode-ai/",
            env!("CARGO_PKG_VERSION"),
            " (context-window lookup)"
        ))
        .build()
}

#[derive(Debug, Deserialize)]
struct OpenRouterModelsResponse {
    data: Vec<OpenRouterModel>,
}

#[derive(Debug, Deserialize)]
struct OpenRouterModel {
    id: String,
    context_length: Option<u64>,
}

async fn fetch_openrouter_context(
    client: &reqwest::Client,
    url: &str,
    model: &str,
    api_key: Option<&str>,
) -> Option<usize> {
    let ttl = catalog_cache_ttl();
    {
        let guard = openrouter_catalog_cache().lock().ok()?;
        if let Some(entry) = guard.as_ref()
            && entry.url == url
            && !cache_stale(entry.fetched_at, ttl)
        {
            tracing::debug!(url = %url, "openrouter models catalog cache hit");
            return pick_openrouter(entry.models.as_ref(), model)
                .and_then(|m| m.context_length)
                .map(|n| n as usize);
        }
    }

    let mut req = client.get(url);
    if let Some(k) = api_key.filter(|s| !s.is_empty()) {
        req = req.bearer_auth(k);
    }
    let resp = req.send().await.ok()?;
    if !resp.status().is_success() {
        tracing::debug!(status = %resp.status(), url = %url, "openrouter models request failed");
        return None;
    }
    let body: OpenRouterModelsResponse = resp.json().await.ok()?;
    let models = Arc::new(body.data);
    {
        if let Ok(mut guard) = openrouter_catalog_cache().lock() {
            *guard = Some(OpenRouterCatalogEntry {
                url: url.to_string(),
                fetched_at: Instant::now(),
                models: Arc::clone(&models),
            });
        }
    }
    pick_openrouter(models.as_ref(), model)
        .and_then(|m| m.context_length)
        .map(|n| n as usize)
}

fn pick_openrouter<'a>(models: &'a [OpenRouterModel], wanted: &str) -> Option<&'a OpenRouterModel> {
    let w = wanted.to_lowercase();
    models
        .iter()
        .find(|m| m.id.to_lowercase() == w)
        .or_else(|| {
            models.iter().find(|m| {
                let id = m.id.to_lowercase();
                id.ends_with(&format!("/{w}"))
            })
        })
}

#[derive(Debug, Deserialize)]
struct AnthropicModelsPage {
    data: Vec<AnthropicModel>,
    #[serde(default)]
    has_more: bool,
}

#[derive(Debug, Deserialize)]
struct AnthropicModel {
    id: String,
    max_input_tokens: Option<u64>,
    /// Present on some API versions; reserved for future output-cap hints.
    #[serde(default)]
    #[allow(dead_code)]
    max_tokens: Option<u64>,
}

async fn fetch_anthropic_context(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
    model: &str,
) -> Option<usize> {
    let ttl = catalog_cache_ttl();
    let cache_key = format!("anthropic|{}|{:x}", base, api_key_tag(api_key));
    {
        let guard = anthropic_catalog_cache().lock().ok()?;
        if let Some(entry) = guard.as_ref()
            && entry.cache_key == cache_key
            && !cache_stale(entry.fetched_at, ttl)
        {
            tracing::debug!("anthropic models catalog cache hit");
            return pick_anthropic(entry.models.as_ref(), model)
                .and_then(|m| m.max_input_tokens)
                .map(|n| n as usize);
        }
    }

    let mut all: Vec<AnthropicModel> = Vec::new();
    let mut after_id: Option<String> = None;

    loop {
        let mut url = format!("{base}/v1/models?limit=100");
        if let Some(ref id) = after_id {
            url.push_str("&after_id=");
            url.push_str(id);
        }

        let resp = client
            .get(&url)
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .send()
            .await
            .ok()?;

        if !resp.status().is_success() {
            tracing::debug!(status = %resp.status(), url = %url, "anthropic models request failed");
            return None;
        }

        let page: AnthropicModelsPage = resp.json().await.ok()?;
        if page.data.is_empty() {
            break;
        }

        let cursor = page.data.last().map(|m| m.id.clone());
        all.extend(page.data);

        if !page.has_more {
            break;
        }
        after_id = cursor;
    }

    let models = Arc::new(all);
    {
        if let Ok(mut guard) = anthropic_catalog_cache().lock() {
            *guard = Some(AnthropicCatalogEntry {
                cache_key,
                fetched_at: Instant::now(),
                models: Arc::clone(&models),
            });
        }
    }
    pick_anthropic(models.as_ref(), model)
        .and_then(|m| m.max_input_tokens)
        .map(|n| n as usize)
}

fn pick_anthropic<'a>(models: &'a [AnthropicModel], wanted: &str) -> Option<&'a AnthropicModel> {
    let w = wanted.to_lowercase();
    if let Some(m) = models.iter().find(|m| m.id.to_lowercase() == w) {
        return Some(m);
    }
    models.iter().find(|m| {
        let id = m.id.to_lowercase();
        id.starts_with(&w) && (id.len() == w.len() || id.as_bytes().get(w.len()) == Some(&b'-'))
    })
}

fn openai_context_from_catalog(value: &serde_json::Value, model: &str) -> Option<usize> {
    let data = value.get("data")?.as_array()?;
    let w = model.to_lowercase();
    for m in data {
        let id = m.get("id")?.as_str()?.to_lowercase();
        if id != w {
            continue;
        }
        // Different providers use different field names for context window.
        for key in &[
            "context_window",
            "context_length",
            "max_context_length",
            "max_input_tokens",
        ] {
            if let Some(cw) = m.get(*key).and_then(|x| x.as_u64()) {
                return Some(cw as usize);
            }
        }
    }
    None
}

async fn fetch_openai_context(
    client: &reqwest::Client,
    base: &str,
    api_key: &str,
    model: &str,
) -> Option<usize> {
    let ttl = catalog_cache_ttl();
    let cache_key = format!("openai|{}|{:x}", base, api_key_tag(api_key));
    {
        let guard = openai_catalog_cache().lock().ok()?;
        if let Some(entry) = guard.as_ref()
            && entry.cache_key == cache_key
            && !cache_stale(entry.fetched_at, ttl)
        {
            tracing::debug!("openai models catalog cache hit");
            return openai_context_from_catalog(entry.value.as_ref(), model);
        }
    }

    let url = if base.ends_with("/v1") {
        format!("{base}/models")
    } else {
        format!("{base}/v1/models")
    };
    let resp = client.get(&url).bearer_auth(api_key).send().await.ok()?;
    if !resp.status().is_success() {
        tracing::debug!(status = %resp.status(), url = %url, "openai models request failed");
        return None;
    }
    let v: serde_json::Value = resp.json().await.ok()?;
    let value = Arc::new(v);
    {
        if let Ok(mut guard) = openai_catalog_cache().lock() {
            *guard = Some(OpenAiCatalogEntry {
                cache_key,
                fetched_at: Instant::now(),
                value: Arc::clone(&value),
            });
        }
    }
    openai_context_from_catalog(value.as_ref(), model)
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

/// Fetch available model IDs from the active provider's live API.
///
/// There are deliberately no static fallbacks: a failed request must not look
/// like a successful but stale catalog.  However, successful results are
/// persisted to a temp-dir disk cache so that a cold-start within the TTL
/// window avoids a blocking network round-trip.
pub async fn fetch_provider_model_ids(
    config: &DcodeAiConfig,
) -> Result<Vec<String>, ModelCatalogError> {
    let provider = config.provider.default;
    let cache_tag = disk_cache_tag(config);

    // Try disk cache first (fast cold-start path).
    if let Some(ids) = disk_cache_read(&cache_tag) {
        tracing::debug!(provider = %provider.display_name(), "model catalog loaded from disk cache");
        return Ok(ids);
    }

    let client = http_client().map_err(|error| ModelCatalogError::Request {
        provider: provider.display_name(),
        message: format!("failed to build HTTP client: {error}"),
    })?;
    let ids = match provider {
        ProviderKind::OpenRouter => fetch_openrouter_model_ids(&client, config).await?,
        ProviderKind::Anthropic => fetch_anthropic_model_ids(&client, config).await?,
        ProviderKind::OpenAi => fetch_openai_model_ids(&client, config).await?,
        // Antigravity (Cloud Code Assist) has no OpenAI `/v1/models` endpoint;
        // it exposes its live catalog via `fetchAvailableModels`. Fall back to a
        // minimal known-good list only if that fetch fails (expired token/offline).
        ProviderKind::Antigravity => {
            let mut ids = fetch_antigravity_model_ids(&client)
                .await
                .unwrap_or_default();
            if ids.is_empty() {
                ids = antigravity_fallback_model_ids();
            }
            ids
        }
        ProviderKind::OpenCodeZen => fetch_opencodezen_model_ids(&client, config).await?,
    };
    let ids = finish_catalog(provider.display_name(), ids)?;

    // Persist to disk for next cold start.
    disk_cache_write(&cache_tag, &ids);
    Ok(ids)
}

/// Fetch the live Antigravity model catalog via Cloud Code Assist
/// `fetchAvailableModels`. The response is `{"models": {"<id>": {...}}}`, so the
/// model ids are the map keys (an array shape is handled defensively too).
async fn fetch_antigravity_model_ids(
    client: &reqwest::Client,
) -> Result<Vec<String>, ModelCatalogError> {
    let auth = dcode_ai_common::auth::AuthStore::load().map_err(|error| {
        ModelCatalogError::Authentication {
            provider: "Antigravity",
            message: error.to_string(),
        }
    })?;
    let ag = auth.antigravity.ok_or(ModelCatalogError::Authentication {
        provider: "Antigravity",
        message: "run `dcode-ai login antigravity`".into(),
    })?;
    let project = if ag.project_id.trim().is_empty() {
        "rising-fact-p41fc"
    } else {
        ag.project_id.as_str()
    };
    let user_agent = std::env::var("DCODE_ANTIGRAVITY_USER_AGENT")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "antigravity/1.104.0 windows/amd64".to_string());

    let resp = client
        .post("https://cloudcode-pa.googleapis.com/v1internal:fetchAvailableModels")
        .bearer_auth(&ag.access_token)
        .header("User-Agent", user_agent)
        .header(
            "X-Goog-Api-Client",
            "google-cloud-sdk vscode_cloudshelleditor/0.1",
        )
        .header(
            "Client-Metadata",
            r#"{"ideType":"ANTIGRAVITY","platform":"WINDOWS","pluginType":"GEMINI"}"#,
        )
        .json(&serde_json::json!({ "project": project }))
        .send()
        .await
        .map_err(|error| ModelCatalogError::Request {
            provider: "Antigravity",
            message: error.to_string(),
        })?;
    let value = response_json("Antigravity", resp).await?;

    let ids: Vec<String> = match value.get("models") {
        Some(serde_json::Value::Object(map)) => map.keys().cloned().collect(),
        Some(serde_json::Value::Array(arr)) => arr
            .iter()
            .filter_map(|m| {
                m.get("name")
                    .or_else(|| m.get("modelId"))
                    .or_else(|| m.get("id"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim_start_matches("models/").to_string())
            })
            .collect(),
        _ => {
            return Err(ModelCatalogError::InvalidResponse {
                provider: "Antigravity",
                message: "missing `models` field".into(),
            });
        }
    };
    Ok(ids)
}

/// Minimal known-good catalog used only when the live fetch fails (expired
/// token / offline). `gemini-2.5-flash` is confirmed available.
fn antigravity_fallback_model_ids() -> Vec<String> {
    ["gemini-2.5-flash", "gemini-3-pro-preview"]
        .iter()
        .map(|s| s.to_string())
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

// ── Disk cache for model ID catalogs ──────────────────────────────────────────

fn disk_cache_dir() -> std::path::PathBuf {
    // `-v2`: invalidates pre-live-catalog caches (e.g. the old hardcoded
    // Antigravity list) so users pick up the new model discovery immediately.
    std::env::temp_dir().join("dcode-ai-model-cache-v2")
}

fn disk_cache_tag(config: &DcodeAiConfig) -> String {
    let provider = config.provider.default.display_name();
    let key_hint = match config.provider.default {
        ProviderKind::OpenRouter => config
            .provider
            .openrouter
            .resolve_api_key()
            .map(|k| api_key_tag(&k))
            .unwrap_or(0),
        ProviderKind::Anthropic => config
            .provider
            .anthropic
            .resolve_api_key()
            .map(|k| api_key_tag(&k))
            .unwrap_or(0),
        ProviderKind::OpenAi | ProviderKind::Antigravity => config
            .provider
            .openai
            .resolve_api_key()
            .map(|k| api_key_tag(&k))
            .unwrap_or(0),
        ProviderKind::OpenCodeZen => config
            .provider
            .opencodezen
            .resolve_api_key()
            .map(|k| api_key_tag(&k))
            .unwrap_or(0),
    };
    format!("{provider}-{key_hint:x}")
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
    let ttl = catalog_cache_ttl().as_secs();
    if now.saturating_sub(entry.ts) > ttl {
        return None;
    }
    if entry.ids.is_empty() {
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
    let path = dir.join(format!("{tag}.json"));
    let _ = std::fs::write(path, json.to_string());
}

async fn response_json(
    provider: &'static str,
    response: reqwest::Response,
) -> Result<serde_json::Value, ModelCatalogError> {
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let detail = body.chars().take(512).collect::<String>();
        return Err(ModelCatalogError::Request {
            provider,
            message: format!("HTTP {status}: {detail}"),
        });
    }
    response
        .json()
        .await
        .map_err(|error| ModelCatalogError::InvalidResponse {
            provider,
            message: error.to_string(),
        })
}

fn ids_from_data(
    provider: &'static str,
    value: &serde_json::Value,
) -> Result<Vec<String>, ModelCatalogError> {
    let data = value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| ModelCatalogError::InvalidResponse {
            provider,
            message: "missing `data` array".into(),
        })?;
    Ok(data
        .iter()
        .filter_map(|model| model.get("id").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect())
}

async fn fetch_openrouter_model_ids(
    client: &reqwest::Client,
    config: &DcodeAiConfig,
) -> Result<Vec<String>, ModelCatalogError> {
    let base = config.provider.openrouter.base_url.trim_end_matches('/');
    let url = format!("{base}/v1/models");
    let key = config.provider.openrouter.resolve_api_key();
    let ttl = catalog_cache_ttl();

    // Check cache first
    {
        let guard = openrouter_catalog_cache().lock().ok();
        if let Some(Some(entry)) = guard.as_ref().map(|g| g.as_ref())
            && entry.url == url
            && !cache_stale(entry.fetched_at, ttl)
        {
            let mut ids: Vec<String> = entry.models.iter().map(|m| m.id.clone()).collect();
            ids.sort();
            return Ok(ids);
        }
    }

    let mut req = client.get(&url);
    if let Some(k) = key.as_deref().filter(|s| !s.is_empty()) {
        req = req.bearer_auth(k);
    }
    let resp = req
        .send()
        .await
        .map_err(|error| ModelCatalogError::Request {
            provider: "OpenRouter",
            message: error.to_string(),
        })?;
    let value = response_json("OpenRouter", resp).await?;
    let body: OpenRouterModelsResponse =
        serde_json::from_value(value).map_err(|error| ModelCatalogError::InvalidResponse {
            provider: "OpenRouter",
            message: error.to_string(),
        })?;
    let models = Arc::new(body.data);
    let mut ids: Vec<String> = models.iter().map(|m| m.id.clone()).collect();
    ids.sort();
    {
        if let Ok(mut guard) = openrouter_catalog_cache().lock() {
            *guard = Some(OpenRouterCatalogEntry {
                url,
                fetched_at: Instant::now(),
                models,
            });
        }
    }
    Ok(ids)
}

async fn fetch_anthropic_model_ids(
    client: &reqwest::Client,
    config: &DcodeAiConfig,
) -> Result<Vec<String>, ModelCatalogError> {
    let key = match config.provider.anthropic.resolve_api_key() {
        Some(k) => k,
        None => {
            return Err(ModelCatalogError::Authentication {
                provider: "Anthropic",
                message: format!(
                    "set {} or configure an API key",
                    config.provider.anthropic.api_key_env
                ),
            });
        }
    };
    let base = config.provider.anthropic.base_url.trim_end_matches('/');
    let ttl = catalog_cache_ttl();
    let cache_key = format!("anthropic|{}|{:x}", base, api_key_tag(&key));

    {
        let guard = anthropic_catalog_cache().lock().ok();
        if let Some(Some(entry)) = guard.as_ref().map(|g| g.as_ref())
            && entry.cache_key == cache_key
            && !cache_stale(entry.fetched_at, ttl)
        {
            let mut ids: Vec<String> = entry.models.iter().map(|m| m.id.clone()).collect();
            ids.sort();
            return Ok(ids);
        }
    }

    let mut all: Vec<AnthropicModel> = Vec::new();
    let mut after_id: Option<String> = None;
    loop {
        let mut url = format!("{base}/v1/models?limit=100");
        if let Some(ref id) = after_id {
            url.push_str("&after_id=");
            url.push_str(id);
        }
        let resp = client
            .get(&url)
            .header("x-api-key", &key)
            .header("anthropic-version", "2023-06-01")
            .send()
            .await
            .map_err(|error| ModelCatalogError::Request {
                provider: "Anthropic",
                message: error.to_string(),
            })?;
        let value = response_json("Anthropic", resp).await?;
        let page: AnthropicModelsPage =
            serde_json::from_value(value).map_err(|error| ModelCatalogError::InvalidResponse {
                provider: "Anthropic",
                message: error.to_string(),
            })?;
        if page.data.is_empty() {
            break;
        }
        let cursor = page.data.last().map(|m| m.id.clone());
        all.extend(page.data);
        if !page.has_more {
            break;
        }
        after_id = cursor;
    }

    let models = Arc::new(all);
    let mut ids: Vec<String> = models.iter().map(|m| m.id.clone()).collect();
    ids.sort();
    if let Ok(mut guard) = anthropic_catalog_cache().lock() {
        *guard = Some(AnthropicCatalogEntry {
            cache_key,
            fetched_at: Instant::now(),
            models,
        });
    }
    Ok(ids)
}

async fn fetch_openai_model_ids(
    client: &reqwest::Client,
    config: &DcodeAiConfig,
) -> Result<Vec<String>, ModelCatalogError> {
    let mut base_str = config.provider.openai.base_url.trim_end_matches('/');
    if matches!(config.provider.default, ProviderKind::Antigravity) && is_copilot_base_url(base_str)
    {
        base_str = "https://api.openai.com";
    }
    let base = base_str;
    let is_copilot =
        is_copilot_base_url(base) && matches!(config.provider.default, ProviderKind::OpenAi);
    if is_copilot {
        return fetch_copilot_model_ids(client, base).await;
    }

    let auth = dcode_ai_common::auth::AuthStore::load().ok();
    let oauth_access = auth.as_ref().and_then(|store| {
        if matches!(config.provider.default, ProviderKind::Antigravity) {
            store
                .antigravity
                .as_ref()
                .map(|oauth| oauth.access_token.clone())
        } else {
            store
                .openai_oauth
                .as_ref()
                .map(|oauth| oauth.access_token.clone())
        }
    });

    let chatgpt_catalog = matches!(config.provider.default, ProviderKind::OpenAi)
        && config.provider.openai.resolve_api_key().is_none();
    if chatgpt_catalog && let Some(token) = oauth_access.as_ref() {
        return fetch_chatgpt_codex_model_ids(client, token).await;
    }

    let key = if chatgpt_catalog {
        if let Some(token) = oauth_access {
            token
        } else if let Some(k) = config.provider.openai.resolve_api_key() {
            k
        } else {
            return Err(ModelCatalogError::Authentication {
                provider: "OpenAI",
                message: "run `dcode-ai login openai` or configure OPENAI_API_KEY".into(),
            });
        }
    } else if let Some(k) = config.provider.openai.resolve_api_key() {
        k
    } else if let Some(token) = oauth_access {
        token
    } else {
        return Err(ModelCatalogError::Authentication {
            provider: if matches!(config.provider.default, ProviderKind::Antigravity) {
                "Antigravity"
            } else {
                "OpenAI"
            },
            message: if matches!(config.provider.default, ProviderKind::Antigravity) {
                "run `dcode-ai login antigravity` or configure OPENAI_API_KEY".into()
            } else {
                "run `dcode-ai login openai` or configure OPENAI_API_KEY".into()
            },
        });
    };
    let ttl = catalog_cache_ttl();
    let cache_key = format!("openai|{}|{:x}", base, api_key_tag(&key));

    {
        let guard = openai_catalog_cache().lock().ok();
        if let Some(Some(entry)) = guard.as_ref().map(|g| g.as_ref())
            && entry.cache_key == cache_key
            && !cache_stale(entry.fetched_at, ttl)
            && let Some(arr) = entry.value.get("data").and_then(|d| d.as_array())
        {
            let mut ids: Vec<String> = arr
                .iter()
                .filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(String::from))
                .collect();
            ids.sort();
            return Ok(ids);
        }
    }

    let url = if base.ends_with("/v1") {
        format!("{base}/models")
    } else {
        format!("{base}/v1/models")
    };
    let resp = client
        .get(&url)
        .bearer_auth(&key)
        .send()
        .await
        .map_err(|error| ModelCatalogError::Request {
            provider: "OpenAI",
            message: error.to_string(),
        })?;
    let v = response_json("OpenAI", resp).await?;
    let value = Arc::new(v);
    let mut ids = Vec::new();
    if let Some(arr) = value.get("data").and_then(|d| d.as_array()) {
        ids = arr
            .iter()
            .filter_map(|m| m.get("id").and_then(|v| v.as_str()).map(String::from))
            .collect();
        ids.sort();
    }
    {
        if let Ok(mut guard) = openai_catalog_cache().lock() {
            *guard = Some(OpenAiCatalogEntry {
                cache_key,
                fetched_at: Instant::now(),
                value,
            });
        }
    }
    Ok(ids)
}

async fn fetch_chatgpt_codex_model_ids(
    client: &reqwest::Client,
    access_token: &str,
) -> Result<Vec<String>, ModelCatalogError> {
    let url = "https://chatgpt.com/backend-api/codex/models?client_version=0.111.0";
    let resp = client
        .get(url)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|error| ModelCatalogError::Request {
            provider: "OpenAI Codex",
            message: error.to_string(),
        })?;
    let value = response_json("OpenAI Codex", resp).await?;
    let models = value
        .get("models")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| ModelCatalogError::InvalidResponse {
            provider: "OpenAI Codex",
            message: "missing `models` array".into(),
        })?;
    let mut ids: Vec<String> = models
        .iter()
        .filter_map(|m| m.get("slug").and_then(|v| v.as_str()).map(String::from))
        .collect();
    ids.sort();
    finish_catalog("OpenAI Codex", ids)
}

async fn fetch_opencodezen_model_ids(
    client: &reqwest::Client,
    config: &DcodeAiConfig,
) -> Result<Vec<String>, ModelCatalogError> {
    let base = config.provider.opencodezen.base_url.trim_end_matches('/');

    let key = if let Some(k) = config.provider.opencodezen.resolve_api_key() {
        k
    } else if let Ok(auth) = dcode_ai_common::auth::AuthStore::load() {
        if let Some(oauth) = auth.opencodezen_oauth {
            oauth.access_token
        } else {
            return Err(ModelCatalogError::Authentication {
                provider: "MiniMax (OpenCode Zen)",
                message: "run `dcode-ai login opencodezen` or configure OPENCODE_API_KEY".into(),
            });
        }
    } else {
        return Err(ModelCatalogError::Authentication {
            provider: "MiniMax (OpenCode Zen)",
            message: "could not load the authentication store".into(),
        });
    };

    let url = if base.ends_with("/v1") {
        format!("{base}/models")
    } else {
        format!("{base}/v1/models")
    };
    let resp = client
        .get(&url)
        .bearer_auth(&key)
        .send()
        .await
        .map_err(|error| ModelCatalogError::Request {
            provider: "MiniMax (OpenCode Zen)",
            message: error.to_string(),
        })?;
    let value = response_json("MiniMax (OpenCode Zen)", resp).await?;
    ids_from_data("MiniMax (OpenCode Zen)", &value)
}

fn is_copilot_base_url(base_url: &str) -> bool {
    base_url.to_ascii_lowercase().contains("githubcopilot.com")
}

async fn fetch_copilot_model_ids(
    client: &reqwest::Client,
    base: &str,
) -> Result<Vec<String>, ModelCatalogError> {
    let auth = dcode_ai_common::auth::AuthStore::load().map_err(|error| {
        ModelCatalogError::Authentication {
            provider: "GitHub Copilot",
            message: error.to_string(),
        }
    })?;
    let github_token = auth.copilot.map(|auth| auth.github_token).ok_or_else(|| {
        ModelCatalogError::Authentication {
            provider: "GitHub Copilot",
            message: "run `dcode-ai login copilot`".into(),
        }
    })?;
    let token_response = client
        .get("https://api.github.com/copilot_internal/v2/token")
        .header("Authorization", format!("token {github_token}"))
        .header("User-Agent", "GitHubCopilotChat")
        .header("Editor-Version", "dcode-ai")
        .header("Editor-Plugin-Version", "dcode-ai")
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|error| ModelCatalogError::Request {
            provider: "GitHub Copilot",
            message: format!("token exchange failed: {error}"),
        })?;
    let token_status = token_response.status();
    let token_body =
        token_response
            .text()
            .await
            .map_err(|error| ModelCatalogError::InvalidResponse {
                provider: "GitHub Copilot",
                message: format!("failed to read token response: {error}"),
            })?;
    let access_token = parse_copilot_token_response(token_status, &token_body, github_token)?;
    let url = format!("{}/models", base.trim_end_matches('/'));
    let response = client
        .get(url)
        .bearer_auth(access_token)
        .header("Copilot-Integration-Id", "vscode-chat")
        .header("Editor-Version", "dcode-ai")
        .header("Editor-Plugin-Version", "dcode-ai")
        .send()
        .await
        .map_err(|error| ModelCatalogError::Request {
            provider: "GitHub Copilot",
            message: error.to_string(),
        })?;
    let value = response_json("GitHub Copilot", response).await?;
    ids_from_data("GitHub Copilot", &value)
}

fn parse_copilot_token_response(
    status: reqwest::StatusCode,
    body: &str,
    github_token: String,
) -> Result<String, ModelCatalogError> {
    // GitHub returns 404 from the internal token endpoint for some valid
    // accounts. Chat already uses the OAuth token directly in that case.
    if status == reqwest::StatusCode::NOT_FOUND || body.contains("\"status\":\"404\"") {
        return Ok(github_token);
    }
    if !status.is_success() {
        return Err(ModelCatalogError::Request {
            provider: "GitHub Copilot",
            message: format!("token exchange returned HTTP {status}: {body}"),
        });
    }
    let token_value: serde_json::Value =
        serde_json::from_str(body).map_err(|error| ModelCatalogError::InvalidResponse {
            provider: "GitHub Copilot",
            message: format!("invalid token response: {error}"),
        })?;
    token_value
        .get("token")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| ModelCatalogError::InvalidResponse {
            provider: "GitHub Copilot",
            message: "token response missing `token`".into(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openrouter_pick_exact() {
        let models = vec![
            OpenRouterModel {
                id: "openai/gpt-4o".into(),
                context_length: Some(128_000),
            },
            OpenRouterModel {
                id: "other/x".into(),
                context_length: Some(8_000),
            },
        ];
        let m = pick_openrouter(&models, "openai/gpt-4o").unwrap();
        assert_eq!(m.context_length, Some(128_000));
    }

    #[test]
    fn anthropic_pick_prefix() {
        let models = vec![AnthropicModel {
            id: "claude-3-5-sonnet-20241022".into(),
            max_input_tokens: Some(200_000),
            max_tokens: Some(8192),
        }];
        let m = pick_anthropic(&models, "claude-3-5-sonnet").unwrap();
        assert_eq!(m.max_input_tokens, Some(200_000));
    }

    #[test]
    fn openai_parse_context_window_from_cached_json() {
        let v: serde_json::Value = serde_json::json!({
            "data": [
                { "id": "gpt-4o", "context_window": 128000 }
            ]
        });
        assert_eq!(openai_context_from_catalog(&v, "gpt-4o"), Some(128_000));
        assert_eq!(openai_context_from_catalog(&v, "gpt-4o-mini"), None);
    }

    #[test]
    fn openai_context_window_alternative_field_names() {
        // Copilot and other OpenAI-compatible providers may use different field names.
        let ctx_length = serde_json::json!({
            "data": [{ "id": "model-a", "context_length": 65536 }]
        });
        assert_eq!(
            openai_context_from_catalog(&ctx_length, "model-a"),
            Some(65_536)
        );

        let max_input = serde_json::json!({
            "data": [{ "id": "model-b", "max_input_tokens": 200000 }]
        });
        assert_eq!(
            openai_context_from_catalog(&max_input, "model-b"),
            Some(200_000)
        );
    }

    #[test]
    fn ids_from_data_reads_provider_response() {
        let value = serde_json::json!({
            "data": [{"id": "new-model"}, {"id": "newer-model"}]
        });
        let ids = ids_from_data("test", &value).expect("catalog");
        assert_eq!(ids, ["new-model", "newer-model"]);
    }

    #[test]
    fn empty_live_catalog_fails_loudly() {
        let error = finish_catalog("test", Vec::new()).expect_err("empty catalog must fail");
        assert!(error.to_string().contains("empty model catalog"));
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
    fn copilot_token_404_uses_github_token_without_model_fallback() {
        let token = parse_copilot_token_response(
            reqwest::StatusCode::NOT_FOUND,
            r#"{"message":"Not Found","status":"404"}"#,
            "github-oauth-token".into(),
        )
        .expect("404 compatibility token");
        assert_eq!(token, "github-oauth-token");
    }

    #[test]
    fn copilot_token_success_uses_exchanged_token() {
        let token = parse_copilot_token_response(
            reqwest::StatusCode::OK,
            r#"{"token":"copilot-access-token"}"#,
            "github-oauth-token".into(),
        )
        .expect("exchanged token");
        assert_eq!(token, "copilot-access-token");
    }
}
