use std::path::Path;
use std::time::Duration;

use dcode_ai_common::auth::AuthStore;
use dcode_ai_common::config::{DcodeAiConfig, OpenAiConfig, ProviderKind};
use dcode_ai_common::message::Message;
use dcode_ai_common::tool::ToolDefinition;
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderValue};

use super::openai_compat::{map_provider_error, openai_request_body, spawn_openai_stream};
use super::{Provider, ProviderCapabilities, ProviderError, StreamChunk};

pub struct OpenAiProvider {
    client: reqwest::Client,
    config: OpenAiConfig,
    max_tokens: u32,
}

impl OpenAiProvider {
    pub fn from_config(
        config: &DcodeAiConfig,
        provider: ProviderKind,
    ) -> Result<Self, ProviderError> {
        let openai = match provider {
            ProviderKind::OpenAi | ProviderKind::Antigravity => config.provider.openai.clone(),
            ProviderKind::OpenCodeZen => config.provider.opencodezen.clone(),
            _ => {
                return Err(ProviderError::Configuration(format!(
                    "OpenAiProvider does not support provider {:?}",
                    provider
                )));
            }
        };
        let auth_store = AuthStore::load().ok().unwrap_or_default();
        let copilot_mode = is_copilot_base_url(&openai.base_url);

        let api_key = if let Some(key) = openai.resolve_api_key() {
            key
        } else if let Some(oauth) = auth_store.openai_oauth {
            oauth.access_token
        } else if let Some(oauth) = auth_store.opencodezen_oauth {
            oauth.access_token
        } else if copilot_mode {
            let github_token = auth_store.copilot.map(|c| c.github_token).ok_or_else(|| {
                ProviderError::Configuration(
                    "missing Copilot login; run `dcode-ai login copilot`".to_string(),
                )
            })?;
            fetch_copilot_access_token_blocking(github_token)?
        } else {
            return Err(ProviderError::Configuration(format!(
                "missing {} API key; set {} or provide `provider.{}.api_key` in config (or run `dcode-ai login opencodezen`)",
                provider.display_name(),
                openai.api_key_env,
                provider.to_config_key()
            )));
        };

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {api_key}")).map_err(|err| {
                ProviderError::Configuration(format!(
                    "failed to build OpenAI authorization header: {err}"
                ))
            })?,
        );

        if copilot_mode {
            headers.insert(
                "User-Agent",
                HeaderValue::from_static("GitHubCopilotChat/0.22.4"),
            );
            headers.insert("Editor-Version", HeaderValue::from_static("vscode/1.90.0"));
            headers.insert(
                "Editor-Plugin-Version",
                HeaderValue::from_static("copilot-chat/0.22.4"),
            );
            headers.insert(
                "Copilot-Integration-Id",
                HeaderValue::from_static("vscode-chat"),
            );
            headers.insert(
                "OpenAI-Intent",
                HeaderValue::from_static("conversation-panel"),
            );
        }

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .map_err(|err| {
                ProviderError::Configuration(format!("failed to build HTTP client: {err}"))
            })?;

        Ok(Self {
            client,
            config: openai,
            max_tokens: config.model.max_tokens,
        })
    }

    fn endpoint(&self) -> String {
        let base = self.config.base_url.trim_end_matches('/');
        if is_copilot_base_url(base) || base.ends_with("/v1") {
            format!("{base}/chat/completions")
        } else {
            format!("{base}/v1/chat/completions")
        }
    }
}

fn is_copilot_base_url(base_url: &str) -> bool {
    base_url.to_ascii_lowercase().contains("githubcopilot.com")
}

fn fetch_copilot_access_token_blocking(github_token: String) -> Result<String, ProviderError> {
    let run = move || async move { fetch_copilot_access_token(github_token).await };

    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        let j = std::thread::spawn(move || handle.block_on(run()));
        return j
            .join()
            .map_err(|_| ProviderError::RequestFailed("copilot token thread panicked".into()))?;
    }

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| ProviderError::RequestFailed(format!("failed to build runtime: {e}")))?;
    rt.block_on(run())
}

async fn fetch_copilot_access_token(github_token: String) -> Result<String, ProviderError> {
    let client = reqwest::Client::new();
    let resp = client
        .get("https://api.github.com/copilot_internal/v2/token")
        .header("Authorization", format!("token {github_token}"))
        .header("User-Agent", "GitHubCopilotChat/0.22.4")
        .header("Editor-Version", "vscode/1.90.0")
        .header("Editor-Plugin-Version", "copilot-chat/0.22.4")
        .header("Accept", "application/json")
        .timeout(Duration::from_secs(25))
        .send()
        .await
        .map_err(|e| ProviderError::RequestFailed(format!("copilot token request failed: {e}")))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        // Some GitHub accounts return 404 on this endpoint; in that case, use the
        // GitHub token directly as a best-effort fallback (same strategy as d-code).
        if status.as_u16() == 404 || text.contains("\"status\":\"404\"") {
            return Ok(github_token);
        }
        return Err(ProviderError::AuthError(format!(
            "copilot token error {status}: {text}"
        )));
    }

    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| ProviderError::RequestFailed(format!("copilot token parse error: {e}")))?;
    let token = v
        .get("token")
        .and_then(|x| x.as_str())
        .ok_or_else(|| ProviderError::AuthError("copilot token response missing token".into()))?;
    Ok(token.to_string())
}

#[async_trait::async_trait]
impl Provider for OpenAiProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        // OpenAI o-series: o1, o3, o4, etc.
        // Also: deepseek-r1, qwen-qwq, etc.
        let model = &self.config.model;
        let model_lower = model.to_ascii_lowercase();
        let supports_thinking_stream = model_lower.starts_with('o')
            || model_lower.contains("deepseek-r1")
            || model_lower.contains("qwq")
            || model_lower.contains("reasoning");
        ProviderCapabilities {
            supports_thinking_stream,
            supports_native_images: true,
            supports_video: model_lower.starts_with("gpt-4.1"),
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

        let body = openai_request_body(
            messages,
            tools,
            &model,
            self.max_tokens,
            self.config.temperature,
            workspace_root,
        )?;

        let response = self
            .client
            .post(self.endpoint())
            .json(&body)
            .send()
            .await
            .map_err(|err| ProviderError::RequestFailed(err.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body_text = response.text().await.unwrap_or_default();
            return Err(map_provider_error(status, body_text));
        }

        Ok(spawn_openai_stream(response, "openai"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::test_support::{collect_chunks, spawn_sse_server};
    use dcode_ai_common::message::Message;
    use dcode_ai_common::tool::ToolDefinition;
    use serde_json::json;

    #[tokio::test]
    async fn openai_provider_streams_text_tool_and_usage() {
        let body = concat!(
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello \"},\"index\":0,\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"type\":\"function\",\"function\":{\"name\":\"lookup\",\"arguments\":\"{\\\"path\\\":\\\"\"}}]},\"index\":0,\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"src\\\"}\"}}]},\"index\":0,\"finish_reason\":null}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"index\":0,\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: {\"choices\":[],\"usage\":{\"prompt_tokens\":11,\"completion_tokens\":7}}\n\n",
            "data: [DONE]\n\n"
        )
        .to_string();
        let base_url = spawn_sse_server(body, 200, |request| {
            assert_eq!(request.url(), "/v1/chat/completions");
            let auth = request
                .headers()
                .iter()
                .find(|header| header.field.equiv("authorization"))
                .expect("authorization header");
            assert_eq!(auth.value.as_str(), "Bearer openai-test-key");
        });

        let mut config = DcodeAiConfig::default();
        config.provider.openai.api_key = Some("openai-test-key".into());
        config.provider.openai.base_url = base_url;

        let provider =
            OpenAiProvider::from_config(&config, ProviderKind::OpenAi).expect("provider");
        let stream = provider
            .chat(
                &[Message::user("hello")],
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
        assert!(matches!(&chunks[0], StreamChunk::TextDelta(text) if text == "Hello "));
        assert!(
            matches!(&chunks[1], StreamChunk::ToolUse(call) if call.id == "call_1" && call.name == "lookup" && call.input == json!({"path":"src"}))
        );
        assert!(matches!(
            &chunks[2],
            StreamChunk::Usage {
                input_tokens: 11,
                output_tokens: 7
            }
        ));
        assert!(matches!(chunks.last(), Some(StreamChunk::Done)));
    }
}
