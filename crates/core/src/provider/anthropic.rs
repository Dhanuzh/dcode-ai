use std::path::Path;
use std::time::{Duration, Instant};

use dcode_ai_common::auth::AuthStore;
use dcode_ai_common::config::{AnthropicConfig, DcodeAiConfig};
use dcode_ai_common::message::Message;
use dcode_ai_common::provider_runtime::{has_ant_cli, windows_compat_command_tokio};
use dcode_ai_common::tool::ToolDefinition;
use reqwest::header::{HeaderMap, HeaderValue};

use super::anthropic_compat::{
    AnthropicRequestOptions, anthropic_request_body_with, map_provider_error,
    spawn_anthropic_stream,
};
use super::{Provider, ProviderCapabilities, ProviderError, StreamChunk, retry};

/// Beta header required on `/v1/messages` when authenticating with an OAuth
/// bearer token instead of an API key.
const OAUTH_BETA: &str = "oauth-2025-04-20";

/// How long a token printed by `ant` is reused before asking again.
/// `ant auth print-credentials` refreshes the token itself when needed, so a
/// short reuse window keeps long sessions alive without a subprocess per call.
const ANT_TOKEN_REUSE: Duration = Duration::from_secs(5 * 60);

/// Floor for `max_tokens` on adaptive-thinking models: thinking spends from
/// the same budget, so the small global default (8,192) truncates answers and
/// tool calls. Streaming is always on, so a large cap has no timeout cost.
const ADAPTIVE_MIN_MAX_TOKENS: u32 = 64_000;

/// Where the Anthropic credential comes from.
enum AnthropicCredential {
    /// Anthropic OAuth via the official `ant` CLI profile (`ant auth login`).
    AntOAuth(AntTokenSource),
    /// A bearer token supplied by the environment (`ANTHROPIC_AUTH_TOKEN`,
    /// e.g. from `eval "$(ant auth print-credentials --env)"` or WIF).
    Bearer(String),
    /// Legacy `sk-ant-*` API key (env, config, or credentials store). Only
    /// used when no OAuth source is available.
    ApiKey(String),
}

struct AntTokenSource {
    profile: Option<String>,
    cached: tokio::sync::Mutex<Option<(String, Instant)>>,
}

impl AntTokenSource {
    fn new(profile: Option<String>) -> Self {
        Self {
            profile,
            cached: tokio::sync::Mutex::new(None),
        }
    }

    /// Current access token; `force` bypasses the cache (after a 401).
    /// The mutex makes concurrent callers share one `ant` invocation.
    async fn token(&self, force: bool) -> Result<String, ProviderError> {
        let mut cached = self.cached.lock().await;
        if !force
            && let Some((token, fetched)) = cached.as_ref()
            && fetched.elapsed() < ANT_TOKEN_REUSE
        {
            return Ok(token.clone());
        }
        let token = self.fetch().await?;
        *cached = Some((token.clone(), Instant::now()));
        Ok(token)
    }

    async fn fetch(&self) -> Result<String, ProviderError> {
        let mut cmd = windows_compat_command_tokio("ant");
        cmd.args(["auth", "print-credentials", "--access-token"]);
        // `--profile` is a global flag; the env var is position-independent.
        if let Some(profile) = &self.profile {
            cmd.env("ANTHROPIC_PROFILE", profile);
        }
        cmd.stdin(std::process::Stdio::null());
        let output = tokio::time::timeout(Duration::from_secs(30), cmd.output())
            .await
            .map_err(|_| {
                ProviderError::AuthError("`ant auth print-credentials` timed out after 30s".into())
            })?
            .map_err(|err| ProviderError::Configuration(format!("failed to run `ant`: {err}")))?;
        let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !output.status.success() || token.is_empty() || token.starts_with('{') {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(ProviderError::AuthError(format!(
                "not signed in to Anthropic{}; run `dcode-ai login anthropic` (or `ant auth login`){}",
                self.profile
                    .as_deref()
                    .map(|p| format!(" (profile `{p}`)"))
                    .unwrap_or_default(),
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!(": {stderr}")
                }
            )));
        }
        Ok(token)
    }
}

pub struct AnthropicProvider {
    client: reqwest::Client,
    config: AnthropicConfig,
    max_tokens: u32,
    credential: AnthropicCredential,
    thinking: bool,
    thinking_budget: u32,
}

impl AnthropicProvider {
    pub fn from_config(config: &DcodeAiConfig) -> Result<Self, ProviderError> {
        Self::from_config_with(config, has_ant_cli())
    }

    /// Credential precedence: `ANTHROPIC_AUTH_TOKEN` → Anthropic OAuth via
    /// `ant` → legacy API key. OAuth is the supported sign-in; an API key is
    /// only picked up when `ant` is not installed, so a stale exported key
    /// can't silently shadow the user's OAuth profile.
    fn from_config_with(
        config: &DcodeAiConfig,
        ant_available: bool,
    ) -> Result<Self, ProviderError> {
        let anthropic = config.provider.anthropic.clone();
        let env_bearer = std::env::var("ANTHROPIC_AUTH_TOKEN")
            .ok()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty());

        let credential = if let Some(token) = env_bearer {
            AnthropicCredential::Bearer(token)
        } else if ant_available {
            AnthropicCredential::AntOAuth(AntTokenSource::new(anthropic.oauth_profile.clone()))
        } else {
            let key = anthropic.resolve_api_key().or_else(|| {
                AuthStore::load()
                    .ok()
                    .and_then(|s| s.anthropic)
                    .map(|a| a.token)
                    .filter(|t| t.trim_start().starts_with("sk-ant-"))
            });
            match key {
                Some(key) => AnthropicCredential::ApiKey(key),
                None => {
                    return Err(ProviderError::Configuration(
                        "not signed in to Anthropic: install the `ant` CLI and run `dcode-ai login anthropic` (Anthropic OAuth)".to_string(),
                    ));
                }
            }
        };

        let mut headers = HeaderMap::new();
        headers.insert("anthropic-version", HeaderValue::from_static("2023-06-01"));
        let client = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|err| {
                ProviderError::Configuration(format!("failed to build HTTP client: {err}"))
            })?;

        Ok(Self {
            client,
            config: anthropic,
            max_tokens: config.model.max_tokens,
            credential,
            thinking: config.model.enable_thinking,
            thinking_budget: config.model.thinking_budget,
        })
    }

    fn endpoint(&self) -> String {
        format!("{}/v1/messages", self.config.base_url.trim_end_matches('/'))
    }

    /// Apply auth headers for the configured credential.
    async fn authorize(
        &self,
        request: reqwest::RequestBuilder,
        force_refresh: bool,
    ) -> Result<reqwest::RequestBuilder, ProviderError> {
        Ok(match &self.credential {
            AnthropicCredential::ApiKey(key) => request.header("x-api-key", key),
            AnthropicCredential::Bearer(token) => request
                .bearer_auth(token)
                .header("anthropic-beta", OAUTH_BETA),
            AnthropicCredential::AntOAuth(source) => {
                let token = source.token(force_refresh).await?;
                request
                    .bearer_auth(token)
                    .header("anthropic-beta", OAUTH_BETA)
            }
        })
    }
}

#[async_trait::async_trait]
impl Provider for AnthropicProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        let family = super::anthropic_compat::ClaudeFamily::for_model(&self.config.model);
        let model = &self.config.model;
        let supports_thinking_stream =
            family.is_adaptive() || model.contains("sonnet") || model.contains("opus");
        ProviderCapabilities {
            supports_thinking_stream,
            supports_native_images: true,
            ..Default::default()
        }
    }

    async fn chat(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        workspace_root: &Path,
    ) -> Result<tokio::sync::mpsc::Receiver<StreamChunk>, ProviderError> {
        let model = super::anthropic_compat::normalize_claude_model_id(if model.is_empty() {
            &self.config.model
        } else {
            model
        });

        let options = AnthropicRequestOptions {
            thinking: self.thinking,
            thinking_budget: self.thinking_budget,
            effort: self.config.effort.clone(),
        };
        let max_tokens = if super::anthropic_compat::ClaudeFamily::for_model(&model).is_adaptive() {
            self.max_tokens.max(ADAPTIVE_MIN_MAX_TOKENS)
        } else {
            self.max_tokens
        };
        let body = anthropic_request_body_with(
            messages,
            tools,
            &model,
            max_tokens,
            self.config.temperature,
            workspace_root,
            &options,
        )?;

        // Retry the initial request on transient rate limits (honoring the
        // server's retry-after) before streaming begins. An OAuth 401 forces
        // one token refresh (the cached token may have expired) and retries.
        let endpoint = self.endpoint();
        let send = |force_refresh: bool| {
            let endpoint = endpoint.clone();
            let body = &body;
            async move {
                retry::with_retry(retry::DEFAULT_MAX_ATTEMPTS, || async {
                    let request = self
                        .authorize(self.client.post(&endpoint), force_refresh)
                        .await?;
                    let resp = request
                        .json(body)
                        .send()
                        .await
                        .map_err(ProviderError::from_reqwest_send)?;

                    let status = resp.status();
                    if status.is_success() {
                        Ok(resp)
                    } else {
                        let body_text = resp.text().await.unwrap_or_default();
                        Err(map_provider_error(status, body_text))
                    }
                })
                .await
            }
        };
        let response = match send(false).await {
            Err(ProviderError::AuthError(_))
                if matches!(self.credential, AnthropicCredential::AntOAuth(_)) =>
            {
                send(true).await?
            }
            other => other?,
        };

        Ok(spawn_anthropic_stream(response, "anthropic"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::StopReason;
    use crate::provider::test_support::{collect_chunks, spawn_sse_server};
    use dcode_ai_common::message::Message;
    use dcode_ai_common::tool::ToolDefinition;
    use serde_json::json;

    #[tokio::test]
    async fn anthropic_provider_streams_text_tool_and_usage() {
        let body = concat!(
            "event: message_start\n",
            "data: {\"message\":{\"usage\":{\"input_tokens\":13}}}\n\n",
            "event: content_block_delta\n",
            "data: {\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello from Claude\"}}\n\n",
            "event: content_block_start\n",
            "data: {\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"lookup\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\\\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"src\\\"}\"}}\n\n",
            "event: content_block_stop\n",
            "data: {}\n\n",
            "event: message_delta\n",
            "data: {\"usage\":{\"output_tokens\":5}}\n\n"
        )
        .to_string();
        let base_url = spawn_sse_server(body, 200, |request| {
            assert_eq!(request.url(), "/v1/messages");
            assert!(
                request
                    .headers()
                    .iter()
                    .any(|header| header.field.equiv("x-api-key")
                        && header.value.as_str() == "sk-ant-api03-test")
            );
            assert!(
                request
                    .headers()
                    .iter()
                    .any(|header| header.field.equiv("anthropic-version"))
            );
        });

        let mut config = DcodeAiConfig::default();
        config.provider.anthropic.api_key = Some("sk-ant-api03-test".into());
        config.provider.anthropic.base_url = base_url;

        let provider = AnthropicProvider::from_config_with(&config, false).expect("provider");
        let stream = provider
            .chat(
                &[Message::system("be helpful"), Message::user("hello")],
                &[ToolDefinition {
                    name: "lookup".into(),
                    description: "Lookup a path".into(),
                    parameters: json!({
                        "type": "object",
                        "properties": {
                            "path": {"type": "string"}
                        }
                    }),
                }],
                "",
                std::path::Path::new("."),
            )
            .await
            .expect("chat stream");

        let chunks = collect_chunks(stream).await;
        assert!(matches!(&chunks[0], StreamChunk::TextDelta(text) if text == "Hello from Claude"));
        assert!(
            matches!(&chunks[1], StreamChunk::ToolUse(call) if call.id == "toolu_1" && call.name == "lookup" && call.input == json!({"path":"src"}))
        );
        assert!(matches!(
            &chunks[2],
            StreamChunk::Usage {
                input_tokens: 13,
                output_tokens: 5
            }
        ));
        assert!(matches!(chunks.last(), Some(StreamChunk::Done)));
    }

    async fn stream_from(body: &'static str) -> Vec<StreamChunk> {
        let base_url = spawn_sse_server(body.to_string(), 200, |_| {});
        let mut config = DcodeAiConfig::default();
        config.provider.anthropic.api_key = Some("sk-ant-api03-test".into());
        config.provider.anthropic.base_url = base_url;
        let provider = AnthropicProvider::from_config_with(&config, false).expect("provider");
        let stream = provider
            .chat(&[Message::user("hi")], &[], "", std::path::Path::new("."))
            .await
            .expect("chat stream");
        collect_chunks(stream).await
    }

    #[tokio::test]
    async fn thinking_block_is_captured_with_signature_for_replay() {
        let chunks = stream_from(concat!(
            "event: content_block_start\n",
            "data: {\"content_block\":{\"type\":\"thinking\",\"thinking\":\"\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"plan it\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"delta\":{\"type\":\"signature_delta\",\"signature\":\"SIG\"}}\n\n",
            "event: content_block_stop\n",
            "data: {}\n\n",
            "event: message_delta\n",
            "data: {\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":3}}\n\n",
        ))
        .await;
        assert!(matches!(&chunks[0], StreamChunk::InternalDelta(t) if t == "plan it"));
        assert!(chunks.iter().any(|c| matches!(c,
            StreamChunk::ReplayBlock(b) if b == &json!({"type":"thinking","thinking":"plan it","signature":"SIG"}))));
        assert!(
            chunks
                .iter()
                .any(|c| matches!(c, StreamChunk::Stop(StopReason::EndTurn)))
        );
        assert!(matches!(chunks.last(), Some(StreamChunk::Done)));
    }

    #[tokio::test]
    async fn refusal_stop_reason_carries_category() {
        let chunks = stream_from(concat!(
            "event: message_delta\n",
            "data: {\"delta\":{\"stop_reason\":\"refusal\",\"stop_details\":{\"type\":\"refusal\",\"category\":\"cyber\"}}}\n\n",
        ))
        .await;
        assert!(chunks.iter().any(|c| matches!(c,
            StreamChunk::Stop(StopReason::Refusal { category: Some(cat) }) if cat == "cyber")));
    }

    #[tokio::test]
    async fn truncated_tool_json_fails_loudly() {
        let chunks = stream_from(concat!(
            "event: content_block_start\n",
            "data: {\"content_block\":{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"write_file\"}}\n\n",
            "event: content_block_delta\n",
            "data: {\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"path\\\":\"}}\n\n",
            "event: content_block_stop\n",
            "data: {}\n\n",
        ))
        .await;
        assert!(!chunks.iter().any(|c| matches!(c, StreamChunk::ToolUse(_))));
        assert!(
            chunks
                .iter()
                .any(|c| matches!(c, StreamChunk::Error(m) if m.contains("write_file")))
        );
    }

    #[tokio::test]
    async fn tool_with_empty_input_gets_empty_object() {
        let chunks = stream_from(concat!(
            "event: content_block_start\n",
            "data: {\"content_block\":{\"type\":\"tool_use\",\"id\":\"t1\",\"name\":\"git_status\"}}\n\n",
            "event: content_block_stop\n",
            "data: {}\n\n",
        ))
        .await;
        assert!(chunks.iter().any(|c| matches!(c,
            StreamChunk::ToolUse(call) if call.name == "git_status" && call.input == json!({}))));
    }
}
