//! A model on this machine — Ollama or LM Studio — through the
//! OpenAI-compatible chat API both serve. Free, private and offline; nothing
//! to sign in to. dcode-ai never downloads a model: it points to
//! `ollama pull …` when none is there.

use std::path::Path;
use std::time::Duration;

use dcode_ai_common::config::{DcodeAiConfig, LOCAL_SERVERS, LocalConfig, ProviderKind};
use dcode_ai_common::message::Message;
use dcode_ai_common::tool::ToolDefinition;
use serde_json::Value;

use super::openai_compat::{openai_request_body, spawn_openai_stream};
use super::{Provider, ProviderCapabilities, ProviderError, StreamChunk};

/// Models good at coding with tools, suggested when none is installed.
pub const SUGGESTED: &str = "qwen3-coder, qwen2.5-coder, llama3.1, mistral-small or gpt-oss";

pub struct LocalProvider {
    config: LocalConfig,
    client: reqwest::Client,
    /// The server found on first use: (name, base URL).
    server: tokio::sync::OnceCell<(String, String)>,
}

fn http(timeout: Duration) -> Result<reqwest::Client, ProviderError> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(timeout)
        .build()
        .map_err(|e| ProviderError::Configuration(format!("failed to build HTTP client: {e}")))
}

/// Model ids from an OpenAI-style `GET /models` answer.
pub fn model_ids(v: &Value) -> Vec<String> {
    v["data"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| m["id"].as_str().map(str::to_string))
                // Embedding models can't chat.
                .filter(|id| !id.to_ascii_lowercase().contains("embed"))
                .collect()
        })
        .unwrap_or_default()
}

/// The models a server at `base` has.
pub async fn list_models_at(
    client: &reqwest::Client,
    base: &str,
) -> Result<Vec<String>, ProviderError> {
    let url = format!("{}/models", base.trim_end_matches('/'));
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| ProviderError::Configuration(format!("{url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(ProviderError::Configuration(format!(
            "{url} answered {}",
            resp.status()
        )));
    }
    let v: Value = resp
        .json()
        .await
        .map_err(|e| ProviderError::Configuration(format!("{url}: {e}")))?;
    Ok(model_ids(&v))
}

/// The configured server, or the first of Ollama / LM Studio that answers.
pub async fn find_server(
    client: &reqwest::Client,
    configured: &str,
) -> Result<(String, String), ProviderError> {
    let configured = configured.trim().trim_end_matches('/');
    if !configured.is_empty() {
        list_models_at(client, configured).await?;
        return Ok(("local server".into(), configured.to_string()));
    }
    for (name, base) in LOCAL_SERVERS {
        if list_models_at(client, base).await.is_ok() {
            return Ok((name.to_string(), base.to_string()));
        }
    }
    Err(ProviderError::Configuration(
        "no local model server found — start Ollama (`ollama serve`) or LM Studio's server, \
or set [provider.local] base_url"
            .into(),
    ))
}

/// The local models available now (for `/models`): (server name, models).
pub async fn list_local_models(cfg: &LocalConfig) -> Result<(String, Vec<String>), ProviderError> {
    let client = http(Duration::from_secs(5))?;
    let (name, base) = find_server(&client, &cfg.base_url).await?;
    Ok((name, list_models_at(&client, &base).await?))
}

/// Whether a local model server answers right now (quick; for onboarding).
pub async fn local_server_running() -> bool {
    match http(Duration::from_millis(800)) {
        Ok(c) => find_server(&c, "").await.is_ok(),
        Err(_) => false,
    }
}

/// A clearer error for what local servers commonly answer.
fn explain(status: reqwest::StatusCode, body: &str, model: &str) -> ProviderError {
    let lower = body.to_ascii_lowercase();
    if lower.contains("does not support tools") || lower.contains("tool use is not supported") {
        return ProviderError::Configuration(format!(
            "`{model}` can't call tools, which dcode-ai needs — pick one that can ({SUGGESTED})"
        ));
    }
    if status == reqwest::StatusCode::NOT_FOUND
        || (lower.contains("model") && lower.contains("not found"))
    {
        return ProviderError::Configuration(format!(
            "the local server has no model `{model}` — `ollama pull {model}`, or /models to pick one"
        ));
    }
    super::openai_compat::map_provider_error(status, body.to_string())
}

impl LocalProvider {
    pub fn from_config(config: &DcodeAiConfig) -> Result<Self, ProviderError> {
        Ok(Self {
            config: config.provider.local.clone(),
            // Local models can be slow to start; the stream has its own
            // no-data timeout.
            client: http(Duration::from_secs(60 * 30))?,
            server: tokio::sync::OnceCell::new(),
        })
    }

    async fn server(&self) -> Result<&(String, String), ProviderError> {
        self.server
            .get_or_try_init(|| async {
                let probe = http(Duration::from_secs(5))?;
                find_server(&probe, &self.config.base_url).await
            })
            .await
    }

    /// The model to use: the one asked for, else the configured one, else
    /// the server's first.
    async fn pick_model(&self, base: &str, asked: &str) -> Result<String, ProviderError> {
        let asked = asked.trim().trim_start_matches("local:");
        if !asked.is_empty() {
            return Ok(asked.to_string());
        }
        if !self.config.model.trim().is_empty() {
            return Ok(self.config.model.trim().to_string());
        }
        let probe = http(Duration::from_secs(5))?;
        list_models_at(&probe, base)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| {
                ProviderError::Configuration(format!(
                    "the local server has no models yet — `ollama pull qwen3-coder` (or another: {SUGGESTED})"
                ))
            })
    }
}

#[async_trait::async_trait]
impl Provider for LocalProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        let m = self.config.model.to_ascii_lowercase();
        ProviderCapabilities {
            // `<think>` tags (qwen3, deepseek-r1) are split out by the stream.
            supports_thinking_stream: m.contains("qwen3")
                || m.contains("deepseek-r1")
                || m.contains("gpt-oss"),
            supports_native_images: dcode_ai_common::model_caps::model_accepts_native_images(
                ProviderKind::Local,
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
        let (_, base) = self.server().await?;
        let model = self.pick_model(base, model).await?;
        let body = openai_request_body(
            messages,
            tools,
            &model,
            self.config.max_tokens,
            self.config.temperature,
            workspace_root,
        )?;
        let url = format!("{}/chat/completions", base.trim_end_matches('/'));
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(ProviderError::from_reqwest_send)?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(explain(status, &text, &model));
        }
        Ok(spawn_openai_stream(resp, "local"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn models_are_listed_without_embedding_ones() {
        let v = serde_json::json!({"data": [
            {"id": "qwen3-coder:30b"}, {"id": "nomic-embed-text"}, {"id": "llama3.1"}
        ]});
        assert_eq!(model_ids(&v), vec!["qwen3-coder:30b", "llama3.1"]);
        assert!(model_ids(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn common_server_errors_say_what_to_do() {
        let e = explain(
            reqwest::StatusCode::BAD_REQUEST,
            r#"{"error":{"message":"registry.ollama.ai/library/gemma:2b does not support tools"}}"#,
            "gemma:2b",
        );
        assert!(e.to_string().contains("can't call tools"), "{e}");
        let e = explain(
            reqwest::StatusCode::NOT_FOUND,
            r#"{"error":{"message":"model \"x\" not found, try pulling it first"}}"#,
            "x",
        );
        assert!(e.to_string().contains("ollama pull x"), "{e}");
    }

    /// A tiny OpenAI-compatible server: `/models` and a streamed reply.
    #[tokio::test]
    async fn chats_with_a_server_and_picks_its_first_model() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let seen2 = seen.clone();
        tokio::spawn(async move {
            loop {
                let Ok((mut s, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = vec![0u8; 65536];
                let n = s.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let body = if req.starts_with("GET /v1/models") {
                    r#"{"data":[{"id":"tiny-coder"}]}"#.to_string()
                } else {
                    *seen2.lock().unwrap() = req.clone();
                    "data: {\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\ndata: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n".to_string()
                };
                let ctype = if req.starts_with("GET") {
                    "application/json"
                } else {
                    "text/event-stream"
                };
                let _ = s
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\ncontent-type: {ctype}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .await;
            }
        });
        let mut cfg = DcodeAiConfig::default();
        cfg.provider.local.base_url = base;
        let p = LocalProvider::from_config(&cfg).unwrap();
        let mut rx = p
            .chat(&[Message::user("hi")], &[], "", std::path::Path::new("."))
            .await
            .unwrap();
        let mut text = String::new();
        while let Some(c) = rx.recv().await {
            if let StreamChunk::TextDelta(t) = c {
                text.push_str(&t);
            }
        }
        assert_eq!(text, "hello");
        assert!(seen.lock().unwrap().contains("\"model\":\"tiny-coder\""));
    }
}
