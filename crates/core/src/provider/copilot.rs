//! GitHub Copilot as a first-class provider.
//!
//! Auth: the GitHub OAuth token from `dcode-ai login copilot` (or
//! `COPILOT_GITHUB_TOKEN` in CI) is sent directly as the bearer token to the
//! Copilot API. dcode-ai deliberately does not call GitHub's internal
//! `copilot_internal/v2/token` exchange: GitHub rejects third-party clients
//! there (HTTP 403, citing its terms on scraping).

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use dcode_ai_common::auth::AuthStore;
use dcode_ai_common::config::{CopilotConfig, DcodeAiConfig};
use dcode_ai_common::message::{ContentPart, Message, MessageContent};
use dcode_ai_common::tool::ToolDefinition;
use serde_json::Value;

use super::anthropic_compat::{
    AnthropicRequestOptions, anthropic_request_body_with, spawn_anthropic_stream,
};
use super::openai_compat::{
    map_provider_error, openai_request_body, openai_responses_request_body,
    spawn_openai_responses_stream, spawn_openai_stream,
};
use super::{Provider, ProviderCapabilities, ProviderError, StreamChunk, retry};

/// GitHub token + Copilot API base, with the identity headers Copilot expects.
pub struct CopilotAuthSource {
    config: CopilotConfig,
    github_token: String,
    http: reqwest::Client,
}

impl CopilotAuthSource {
    pub fn from_config(config: &CopilotConfig) -> Result<Self, ProviderError> {
        let github_token = match config.resolve_github_token() {
            Some(token) => token,
            None => AuthStore::load()
                .ok()
                .and_then(|store| store.copilot)
                .map(|auth| auth.github_token)
                .ok_or_else(|| {
                    ProviderError::Configuration(
                        "not signed in to GitHub Copilot; run `dcode-ai login copilot`".into(),
                    )
                })?,
        };
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| {
                ProviderError::Configuration(format!("failed to build HTTP client: {e}"))
            })?;
        Ok(Self {
            config: config.clone(),
            github_token,
            http,
        })
    }

    fn api_base(&self) -> &str {
        self.config.base_url.trim_end_matches('/')
    }

    fn apply_headers(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request
            .bearer_auth(&self.github_token)
            .header("User-Agent", &self.config.user_agent)
            .header("Editor-Version", &self.config.editor_version)
            .header("Editor-Plugin-Version", &self.config.editor_plugin_version)
            .header("Copilot-Integration-Id", &self.config.integration_id)
            .header("Openai-Intent", "conversation-panel")
    }
}

pub struct CopilotProvider {
    auth: CopilotAuthSource,
    config: CopilotConfig,
    max_tokens: u32,
    client: reqwest::Client,
    /// Per-model API paths and limits, fetched once from `GET /models` on
    /// first use.
    models: tokio::sync::OnceCell<HashMap<String, ModelInfo>>,
}

/// What `GET /models` says about one model.
#[derive(Debug, Clone, Default)]
struct ModelInfo {
    /// API paths it accepts (`/chat/completions`, `/responses`, `/v1/messages`).
    endpoints: Vec<String>,
    max_output_tokens: Option<u32>,
    /// Accepted `reasoning_effort` / `output_config.effort` values.
    efforts: Vec<String>,
}

/// Which Copilot API a model is served on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CopilotApi {
    ChatCompletions,
    Responses,
    /// Anthropic Messages API (Claude models): native thinking blocks with
    /// signatures, prompt caching and stop reasons.
    Messages,
}

impl CopilotProvider {
    pub fn from_config(config: &DcodeAiConfig) -> Result<Self, ProviderError> {
        let copilot = config.provider.copilot.clone();
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .build()
            .map_err(|e| {
                ProviderError::Configuration(format!("failed to build HTTP client: {e}"))
            })?;
        Ok(Self {
            auth: CopilotAuthSource::from_config(&copilot)?,
            config: copilot,
            max_tokens: config.model.max_tokens,
            client,
            models: tokio::sync::OnceCell::new(),
        })
    }

    async fn model_info(&self, model: &str) -> Option<ModelInfo> {
        let models = self
            .models
            .get_or_init(|| async {
                match fetch_models(&self.auth).await {
                    Ok(value) => model_infos(&value),
                    Err(e) => {
                        tracing::warn!("Copilot model list unavailable, assuming chat API: {e}");
                        HashMap::new()
                    }
                }
            })
            .await;
        models.get(model).cloned()
    }

    /// The model id to send: `model` itself, or — for a Claude id Copilot does
    /// not serve (e.g. the `sonnet` alias resolving to the Anthropic API's
    /// newest id) — Copilot's newest model of the same family.
    async fn served_model(&self, model: &str) -> String {
        if self.model_info(model).await.is_some() {
            return model.to_string();
        }
        let models = self.models.get().cloned().unwrap_or_default();
        match closest_claude_model(models.keys().map(String::as_str), model) {
            Some(m) => {
                tracing::info!("Copilot does not serve `{model}`; using `{m}`");
                m
            }
            None => model.to_string(),
        }
    }
}

/// Copilot's newest Claude model in the same family (opus / sonnet / haiku /
/// fable) as `wanted`, when `wanted` itself is not offered.
fn closest_claude_model<'a>(
    available: impl Iterator<Item = &'a str>,
    wanted: &str,
) -> Option<String> {
    let w = wanted.to_ascii_lowercase();
    if !w.starts_with("claude") {
        return None;
    }
    let family = ["opus", "sonnet", "haiku", "fable"]
        .into_iter()
        .find(|f| w.contains(f))?;
    let version = |id: &str| -> Vec<u32> {
        id.split(|c: char| !c.is_ascii_digit())
            .filter_map(|p| p.parse().ok())
            .collect()
    };
    available
        .filter(|id| {
            let id = id.to_ascii_lowercase();
            id.starts_with("claude") && id.contains(family)
        })
        .max_by_key(|id| version(id))
        .map(str::to_string)
}

/// Claude models go to the native Messages API when Copilot serves them
/// there; otherwise prefer Chat Completions and use Responses only for
/// models served there alone (e.g. the codex family). Without endpoint data,
/// codex models are assumed Responses-only.
fn pick_api(endpoints: Option<&[String]>, model: &str) -> CopilotApi {
    let is_claude = model.to_ascii_lowercase().starts_with("claude");
    match endpoints {
        Some(paths) if !paths.is_empty() => {
            if is_claude && paths.iter().any(|p| p == "/v1/messages") {
                CopilotApi::Messages
            } else if paths.iter().any(|p| p == "/chat/completions") {
                CopilotApi::ChatCompletions
            } else if paths.iter().any(|p| p == "/responses") {
                CopilotApi::Responses
            } else {
                CopilotApi::ChatCompletions
            }
        }
        _ if model.to_ascii_lowercase().contains("codex") => CopilotApi::Responses,
        _ => CopilotApi::ChatCompletions,
    }
}

#[async_trait::async_trait]
impl Provider for CopilotProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        let m = self.config.model.to_ascii_lowercase();
        ProviderCapabilities {
            supports_thinking_stream: m.contains("claude")
                || m.starts_with("o1")
                || m.starts_with("o3")
                || m.starts_with("o4")
                || m.contains("gpt-5")
                || m.contains("gemini"),
            supports_native_images: dcode_ai_common::model_caps::model_accepts_native_images(
                dcode_ai_common::config::ProviderKind::Copilot,
                &m,
            ),
            supports_video: false,
        }
    }

    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        workspace_root: &Path,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamChunk>, ProviderError> {
        let model = if model.is_empty() {
            self.config.model.clone()
        } else {
            model.to_string()
        };
        let model = self.served_model(&model).await;
        let info = self.model_info(&model).await;
        let api = pick_api(info.as_ref().map(|i| i.endpoints.as_slice()), &model);
        let (body, path) = match api {
            CopilotApi::Messages => {
                // Adaptive thinking needs output headroom; stay within the
                // model's advertised cap.
                let cap = info
                    .as_ref()
                    .and_then(|i| i.max_output_tokens)
                    .unwrap_or(32_000);
                let max_tokens = self.max_tokens.max(32_000).min(cap);
                let effort = self.config.reasoning_effort.clone().filter(|e| {
                    info.as_ref()
                        .is_none_or(|i| i.efforts.is_empty() || i.efforts.contains(e))
                });
                let options = AnthropicRequestOptions {
                    thinking: true,
                    thinking_budget: 8_192,
                    effort,
                };
                (
                    anthropic_request_body_with(
                        messages,
                        tools,
                        &model,
                        max_tokens,
                        self.config.temperature,
                        workspace_root,
                        &options,
                    )?,
                    "v1/messages",
                )
            }
            CopilotApi::ChatCompletions => (
                openai_request_body(
                    messages,
                    tools,
                    &model,
                    self.max_tokens,
                    self.config.temperature,
                    workspace_root,
                )?,
                "chat/completions",
            ),
            CopilotApi::Responses => (
                openai_responses_request_body(
                    messages,
                    tools,
                    &model,
                    self.max_tokens,
                    workspace_root,
                )?,
                "responses",
            ),
        };

        let mut body = body;
        // Reasoning effort for OpenAI-style models that advertise it.
        if let Some(effort) = self.config.reasoning_effort.as_deref()
            && info
                .as_ref()
                .is_some_and(|i| i.efforts.iter().any(|e| e == effort))
        {
            match api {
                CopilotApi::ChatCompletions => body["reasoning_effort"] = effort.into(),
                CopilotApi::Responses => body["reasoning"]["effort"] = effort.into(),
                CopilotApi::Messages => {}
            }
        }
        let url = format!("{}/{path}", self.auth.api_base());
        // Copilot rejects image content unless the request is marked as a
        // vision request.
        let vision = messages.iter().any(|m| {
            matches!(&m.content, MessageContent::Parts(parts)
                if parts.iter().any(|p| matches!(p, ContentPart::Image { .. })))
        });
        let model = model.as_str();
        let response = retry::with_retry(retry::DEFAULT_MAX_ATTEMPTS, || async {
            let mut req = self.auth.apply_headers(self.client.post(&url));
            if vision {
                req = req.header("Copilot-Vision-Request", "true");
            }
            let resp = req
                .json(&body)
                .send()
                .await
                .map_err(ProviderError::from_reqwest_send)?;
            let status = resp.status();
            if status.is_success() {
                Ok(resp)
            } else {
                let text = resp.text().await.unwrap_or_default();
                Err(map_copilot_error(status, text, model))
            }
        })
        .await?;
        Ok(match api {
            CopilotApi::ChatCompletions => spawn_openai_stream(response, "copilot"),
            CopilotApi::Responses => spawn_openai_responses_stream(response, "copilot"),
            CopilotApi::Messages => spawn_anthropic_stream(response, "copilot"),
        })
    }
}

fn map_copilot_error(status: reqwest::StatusCode, body: String, model: &str) -> ProviderError {
    match status.as_u16() {
        401 => ProviderError::AuthError(
            "GitHub login for Copilot is no longer valid; run `dcode-ai login copilot`".into(),
        ),
        403 => ProviderError::AuthError(format!(
            "Copilot refused the request (no Copilot seat, or blocked by organization policy): {body}"
        )),
        400 if body.contains("model_not_supported") || body.contains("not supported") => {
            ProviderError::ModelNotFound(format!(
                "Copilot does not offer `{model}` on this plan (run /models): {body}"
            ))
        }
        _ => map_provider_error(status, body),
    }
}

async fn fetch_models(auth: &CopilotAuthSource) -> Result<Value, ProviderError> {
    let resp = auth
        .apply_headers(auth.http.get(format!("{}/models", auth.api_base())))
        .send()
        .await
        .map_err(ProviderError::from_reqwest_send)?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(map_copilot_error(status, body, "-"));
    }
    serde_json::from_str(&body)
        .map_err(|e| ProviderError::Other(format!("invalid Copilot model list: {e}")))
}

/// Chat models available to this Copilot account, from `GET {api}/models`.
pub async fn list_copilot_models(config: &CopilotConfig) -> Result<Vec<String>, ProviderError> {
    let auth = CopilotAuthSource::from_config(config)?;
    let ids = chat_model_ids(&fetch_models(&auth).await?);
    if ids.is_empty() {
        return Err(ProviderError::Other(
            "Copilot returned no chat models for this account".into(),
        ));
    }
    Ok(ids)
}

fn model_infos(value: &Value) -> HashMap<String, ModelInfo> {
    let strings = |v: &Value| -> Vec<String> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| p.as_str().map(str::to_string))
            .collect()
    };
    value["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m["id"].as_str()?;
            Some((
                id.to_string(),
                ModelInfo {
                    endpoints: strings(&m["supported_endpoints"]),
                    max_output_tokens: m["capabilities"]["limits"]["max_output_tokens"]
                        .as_u64()
                        .map(|n| n as u32),
                    efforts: strings(&m["capabilities"]["supports"]["reasoning_effort"]),
                },
            ))
        })
        .collect()
}

fn chat_model_ids(value: &Value) -> Vec<String> {
    let mut ids: Vec<String> = value["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| {
            let kind = m["capabilities"]["type"].as_str();
            kind.is_none() || kind == Some("chat")
        })
        .filter(|m| m["model_picker_enabled"].as_bool() != Some(false))
        .filter_map(|m| m["id"].as_str().map(str::to_string))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unknown_claude_ids_map_to_copilots_newest_in_family() {
        let ids = [
            "claude-sonnet-4.5",
            "claude-sonnet-5",
            "claude-opus-4.8",
            "gpt-5-mini",
        ];
        assert_eq!(
            closest_claude_model(ids.into_iter(), "claude-sonnet-5-5").as_deref(),
            Some("claude-sonnet-5")
        );
        assert_eq!(
            closest_claude_model(ids.into_iter(), "claude-opus-5-5").as_deref(),
            Some("claude-opus-4.8")
        );
        assert_eq!(
            closest_claude_model(ids.into_iter(), "claude-haiku-5-5"),
            None
        );
        assert_eq!(closest_claude_model(ids.into_iter(), "gpt-9"), None);
    }

    #[test]
    fn model_list_keeps_enabled_chat_models() {
        let ids = chat_model_ids(&json!({"data": [
            {"id": "gpt-4.1", "capabilities": {"type": "chat"}, "model_picker_enabled": true},
            {"id": "claude-sonnet-4.5", "capabilities": {"type": "chat"}},
            {"id": "text-embedding-3-small", "capabilities": {"type": "embeddings"}},
            {"id": "gpt-4o-mini-hidden", "capabilities": {"type": "chat"}, "model_picker_enabled": false},
        ]}));
        assert_eq!(ids, vec!["claude-sonnet-4.5", "gpt-4.1"]);
    }

    #[test]
    fn codex_models_route_to_responses_api() {
        let infos = model_infos(&json!({"data": [
            {"id": "gpt-5.3-codex", "supported_endpoints": ["/responses", "ws:/responses"]},
            {"id": "gpt-5-mini", "supported_endpoints": ["/chat/completions", "/responses"]},
            {"id": "gpt-4.1"},
            {"id": "claude-sonnet-5", "supported_endpoints": ["/v1/messages", "/chat/completions"],
             "capabilities": {"limits": {"max_output_tokens": 64000},
                              "supports": {"reasoning_effort": ["low", "high"]}}},
        ]}));
        let api = |m: &str| pick_api(infos.get(m).map(|i| i.endpoints.as_slice()), m);
        assert_eq!(api("claude-sonnet-5"), CopilotApi::Messages);
        assert_eq!(infos["claude-sonnet-5"].max_output_tokens, Some(64_000));
        assert_eq!(infos["claude-sonnet-5"].efforts, vec!["low", "high"]);
        assert_eq!(api("gpt-5.3-codex"), CopilotApi::Responses);
        assert_eq!(api("gpt-5-mini"), CopilotApi::ChatCompletions);
        assert_eq!(api("gpt-4.1"), CopilotApi::ChatCompletions);
        // No endpoint data at all: codex is still known to be Responses-only.
        assert_eq!(pick_api(None, "gpt-6-codex"), CopilotApi::Responses);
    }
}
