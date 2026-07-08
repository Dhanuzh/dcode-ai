//! Antigravity (Google Cloud Code Assist) provider.
//!
//! Antigravity is **not** OpenAI-compatible: it speaks Google's proprietary
//! Cloud Code Assist API — `streamGenerateContent` over the `v1internal`
//! surface at `cloudcode-pa.googleapis.com`, wrapping a standard Gemini
//! `generateContent` request inside an Antigravity envelope
//! (`{project, model, request, requestType, userAgent}`). Responses stream as
//! SSE where each `data:` line is a JSON object whose `response.candidates[…]`
//! carries Gemini `content.parts` (text / `functionCall` / thinking).
//!
//! Endpoint, headers, envelope and default project mirror the reference
//! Antigravity desktop client so the Google backend accepts our requests.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use dcode_ai_common::auth::{AuthStore, VertexAuth};
use dcode_ai_common::config::{DcodeAiConfig, OpenAiConfig};
use dcode_ai_common::message::{ContentPart, Message, MessageContent, Role};
use dcode_ai_common::tool::{ToolCall, ToolDefinition};
use futures_util::StreamExt;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue};
use serde_json::{Value, json};

use super::openai_compat::map_provider_error;
use super::{Provider, ProviderCapabilities, ProviderError, StreamChunk, retry};

/// Streaming Cloud Code Assist endpoint (prod). `alt=sse` makes the backend
/// emit server-sent events instead of a single JSON array.
const ANTIGRAVITY_ENDPOINT: &str =
    "https://cloudcode-pa.googleapis.com/v1internal:streamGenerateContent?alt=sse";

/// Fallback GCP project used by the reference Antigravity client when the
/// logged-in account has not been onboarded to its own project.
const ANTIGRAVITY_DEFAULT_PROJECT: &str = "rising-fact-p41fc";

// Google gates the Cloud Code Assist backend on the client version advertised
// in `User-Agent`; older strings (e.g. 1.11.5) are rejected with "out of date".
// This tracks a recent Antigravity build and is overridable via
// `DCODE_ANTIGRAVITY_USER_AGENT` so the gate can be bumped without a rebuild.
const ANTIGRAVITY_USER_AGENT: &str = "antigravity/1.104.0 windows/amd64";
const ANTIGRAVITY_API_CLIENT: &str = "google-cloud-sdk vscode_cloudshelleditor/0.1";
const ANTIGRAVITY_CLIENT_METADATA: &str =
    r#"{"ideType":"ANTIGRAVITY","platform":"WINDOWS","pluginType":"GEMINI"}"#;

/// Resolve the Antigravity `User-Agent`, honoring a `DCODE_ANTIGRAVITY_USER_AGENT`
/// override so a moved version gate can be fixed without recompiling.
fn antigravity_user_agent() -> String {
    std::env::var("DCODE_ANTIGRAVITY_USER_AGENT")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| ANTIGRAVITY_USER_AGENT.to_string())
}

const STREAM_CHUNK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

/// Cached gcloud Application Default Credentials access token. gcloud tokens
/// live ~1 hour; refresh after 50 minutes.
static ADC_TOKEN: OnceLock<Mutex<Option<(String, std::time::Instant)>>> = OnceLock::new();

/// Fetch an access token via `gcloud auth application-default
/// print-access-token` (the "Use a Google Cloud project" login path). gcloud
/// owns credential refresh; we only cache the short-lived token.
pub fn adc_access_token() -> Result<String, ProviderError> {
    let cache = ADC_TOKEN.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cache.lock()
        && let Some((token, fetched_at)) = guard.as_ref()
        && fetched_at.elapsed() < std::time::Duration::from_secs(50 * 60)
    {
        return Ok(token.clone());
    }

    // `gcloud` is a .cmd shim on Windows; route through the compat helper.
    let out = dcode_ai_common::provider_runtime::windows_compat_command("gcloud")
        .args(["auth", "application-default", "print-access-token"])
        .output()
        .map_err(|e| {
            ProviderError::Configuration(format!(
                "gcloud not found ({e}); install the Google Cloud SDK and run \
                 `gcloud auth application-default login`"
            ))
        })?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(ProviderError::Configuration(format!(
            "gcloud could not mint an ADC token: {} — run \
             `gcloud auth application-default login` first",
            err.trim()
        )));
    }
    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if token.is_empty() {
        return Err(ProviderError::Configuration(
            "gcloud returned an empty ADC token — run \
             `gcloud auth application-default login`"
                .to_string(),
        ));
    }
    if let Ok(mut guard) = cache.lock() {
        *guard = Some((token.clone(), std::time::Instant::now()));
    }
    Ok(token)
}

/// Vertex AI `streamGenerateContent` URL for a project/location/model.
/// The `global` location uses the location-less host.
fn vertex_endpoint(auth: &VertexAuth, model: &str) -> String {
    let host = if auth.location == "global" {
        "aiplatform.googleapis.com".to_string()
    } else {
        format!("{}-aiplatform.googleapis.com", auth.location)
    };
    format!(
        "https://{host}/v1/projects/{}/locations/{}/publishers/google/models/{model}:streamGenerateContent?alt=sse",
        auth.project_id, auth.location
    )
}

/// Process-global monotonic counter so tool-call ids are unique across turns
/// (they double as keys into the thought-signature cache).
static TOOL_CALL_SEQ: AtomicU64 = AtomicU64::new(1);

/// Maps a tool-call id → the Gemini `thoughtSignature` that accompanied the
/// model's `functionCall`. Gemini rejects a replayed function call whose part
/// lacks this signature ("Function call is missing a thought_signature"), so we
/// capture it as it streams out and re-attach it when the assistant turn is
/// sent back. Process-lifetime only (not persisted) — a tool call and its
/// replay always happen within one run, which is sufficient.
static SIGNATURE_CACHE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();

fn signature_cache() -> &'static Mutex<HashMap<String, String>> {
    SIGNATURE_CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn remember_thought_signature(id: &str, signature: &str) {
    if signature.is_empty() {
        return;
    }
    if let Ok(mut map) = signature_cache().lock() {
        map.insert(id.to_string(), signature.to_string());
    }
}

fn recall_thought_signature(id: &str) -> Option<String> {
    signature_cache().lock().ok()?.get(id).cloned()
}

pub struct AntigravityProvider {
    client: reqwest::Client,
    config: OpenAiConfig,
    project_id: String,
    max_tokens: u32,
    /// When set, requests go straight to Vertex AI in this project/location
    /// (the "Use a Google Cloud project" login) instead of the Antigravity
    /// Cloud Code Assist envelope endpoint.
    vertex: Option<VertexAuth>,
}

impl AntigravityProvider {
    pub fn from_config(config: &DcodeAiConfig) -> Result<Self, ProviderError> {
        // Antigravity reuses the `openai` config block for model/temperature and
        // an optional explicit API key (used for tests / advanced setups).
        let openai = config.provider.openai.clone();

        let auth_store = AuthStore::load().ok().unwrap_or_default();
        // Precedence: an explicit API key (tests / advanced setups; the
        // "local" sentinel from local-model presets doesn't count) beats the
        // Vertex project login, which beats Antigravity OAuth.
        let inline_key = openai.resolve_api_key().filter(|k| k != "local");
        let (access_token, project_id, vertex) = if let Some(key) = inline_key {
            (key, ANTIGRAVITY_DEFAULT_PROJECT.to_string(), None)
        } else if let Some(v) = auth_store.vertex.clone() {
            // "Use a Google Cloud project": ADC token, user's own project.
            let token = adc_access_token()?;
            (token, v.project_id.clone(), Some(v))
        } else {
            let oauth = auth_store.antigravity.ok_or_else(|| {
                ProviderError::Configuration(
                    "missing Antigravity login; run `dcode-ai login antigravity` \
                     (or `/login vertex <project-id>` for a Google Cloud project)"
                        .to_string(),
                )
            })?;
            let resolved = super::openai::resolve_antigravity_auth(oauth)?;
            let project = if resolved.project_id.trim().is_empty() {
                ANTIGRAVITY_DEFAULT_PROJECT.to_string()
            } else {
                resolved.project_id.clone()
            };
            (resolved.access_token, project, None)
        };

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {access_token}")).map_err(|err| {
                ProviderError::Configuration(format!(
                    "failed to build Antigravity authorization header: {err}"
                ))
            })?,
        );
        headers.insert(
            "User-Agent",
            HeaderValue::from_str(&antigravity_user_agent()).map_err(|err| {
                ProviderError::Configuration(format!("invalid Antigravity user-agent: {err}"))
            })?,
        );
        headers.insert(
            "X-Goog-Api-Client",
            HeaderValue::from_static(ANTIGRAVITY_API_CLIENT),
        );
        headers.insert(
            "Client-Metadata",
            HeaderValue::from_static(ANTIGRAVITY_CLIENT_METADATA),
        );

        let client = reqwest::Client::builder()
            .default_headers(headers)
            .connect_timeout(std::time::Duration::from_secs(15))
            .build()
            .map_err(|err| {
                ProviderError::Configuration(format!("failed to build HTTP client: {err}"))
            })?;

        Ok(Self {
            client,
            config: openai,
            project_id,
            max_tokens: config.model.max_tokens,
            vertex,
        })
    }

    /// Build the inner Gemini `generateContent` request plus the base model
    /// id. The caller wraps it in the Antigravity envelope or posts it
    /// directly to Vertex AI, depending on the login mode.
    fn request_parts(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        workspace_root: &Path,
    ) -> Result<(Value, String), ProviderError> {
        let contents = build_gemini_contents(messages, workspace_root)?;

        // Antigravity exposes reasoning effort via a model-name suffix
        // (`gemini-3.5-flash-low`), but the backend wants the *base* model id
        // plus a `thinkingConfig`. Sending the suffixed name with no thinking
        // config makes the model spend its whole output budget thinking and
        // return no visible answer → dcode's "empty completion" retry loop.
        let (base_model, tier) = split_model_tier(model);

        let mut generation_config = json!({
            "temperature": self.config.temperature,
            "maxOutputTokens": self.max_tokens,
        });
        if let Some(tier) = tier {
            generation_config["thinkingConfig"] = thinking_config(&base_model, tier);
        }

        let mut request = json!({
            "contents": contents,
            "generationConfig": generation_config,
        });

        if let Some(system) = build_system_instruction(messages) {
            request["systemInstruction"] = system;
        }
        if let Some(tools) = build_gemini_tools(tools) {
            request["tools"] = tools;
        }

        Ok((request, base_model))
    }
}

#[async_trait::async_trait]
impl Provider for AntigravityProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            supports_thinking_stream: true,
            supports_native_images: true,
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

        let (request, base_model) = self.request_parts(messages, tools, &model, workspace_root)?;
        let (url, body) = if let Some(vertex) = &self.vertex {
            // Direct Vertex AI call: plain generateContent body, project and
            // model addressed in the URL, billed to the user's GCP project.
            (vertex_endpoint(vertex, &base_model), request)
        } else {
            (
                ANTIGRAVITY_ENDPOINT.to_string(),
                json!({
                    "project": self.project_id,
                    "model": base_model,
                    "request": request,
                    "requestType": "agent",
                    "userAgent": "antigravity",
                    "requestId": format!("agent-{}", chrono::Utc::now().timestamp_millis()),
                }),
            )
        };

        let response = retry::with_retry(retry::DEFAULT_MAX_ATTEMPTS, || async {
            let resp = self
                .client
                .post(&url)
                .header(ACCEPT, "text/event-stream")
                .json(&body)
                .send()
                .await
                .map_err(ProviderError::from_reqwest_send)?;
            if resp.status().as_u16() == 429 {
                let text = resp.text().await.unwrap_or_default();
                return Err(map_provider_error(
                    reqwest::StatusCode::TOO_MANY_REQUESTS,
                    text,
                ));
            }
            Ok(resp)
        })
        .await?;

        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(map_provider_error(status, text));
        }

        Ok(spawn_gemini_stream(response, "antigravity"))
    }
}

/// Plain-text view of a message body (assistant / tool turns).
fn content_text(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(t) => t.clone(),
        MessageContent::Parts(_) => content.to_summary_text(),
    }
}

/// Split a reasoning-effort suffix (`-minimal|-low|-medium|-high`) off a model
/// id, returning the base model and the tier. Antigravity encodes effort in the
/// catalog model name, but generateContent expects the base id + thinkingConfig.
fn split_model_tier(model: &str) -> (String, Option<&'static str>) {
    for tier in ["minimal", "low", "medium", "high"] {
        if let Some(base) = model.strip_suffix(&format!("-{tier}")) {
            return (base.to_string(), Some(tier));
        }
    }
    (model.to_string(), None)
}

/// Build the Gemini `thinkingConfig` for a base model + effort tier. Gemini 3
/// uses `thinkingLevel` strings; Gemini 2.5 and Claude (via Antigravity) use a
/// numeric `thinkingBudget`.
fn thinking_config(base_model: &str, tier: &str) -> Value {
    let m = base_model.to_ascii_lowercase();
    if m.contains("gemini-3") {
        json!({
            "includeThoughts": true,
            "thinkingLevel": tier.to_ascii_uppercase(),
        })
    } else if m.contains("claude") {
        let budget = match tier {
            "high" => 32_768,
            "medium" => 16_384,
            _ => 8_192,
        };
        json!({ "includeThoughts": true, "thinkingBudget": budget })
    } else {
        // Gemini 2.5 and other numeric-budget families.
        let budget = match tier {
            "minimal" => 512,
            "low" => 4_096,
            "medium" => 8_192,
            _ => 16_384,
        };
        json!({ "includeThoughts": true, "thinkingBudget": budget })
    }
}

/// Build Gemini `parts` for a user turn, embedding on-disk images as
/// `inlineData` blocks.
fn gemini_parts_from_content(
    content: &MessageContent,
    workspace_root: &Path,
) -> Result<Vec<Value>, ProviderError> {
    match content {
        MessageContent::Text(s) => Ok(vec![json!({ "text": s })]),
        MessageContent::Parts(parts) => {
            let mut out = Vec::new();
            for p in parts {
                match p {
                    ContentPart::Text { text } => out.push(json!({ "text": text })),
                    ContentPart::Image { media_type, path } => {
                        let full = workspace_root.join(path);
                        let bytes = std::fs::read(&full).map_err(|e| {
                            ProviderError::RequestFailed(format!(
                                "failed to read image {}: {e}",
                                full.display()
                            ))
                        })?;
                        let b64 = B64.encode(bytes);
                        out.push(json!({
                            "inlineData": { "mimeType": media_type, "data": b64 }
                        }));
                    }
                }
            }
            if out.is_empty() {
                out.push(json!({ "text": "" }));
            }
            Ok(out)
        }
    }
}

/// Convert dcode conversation history to Gemini `contents`.
///
/// System turns are hoisted into `systemInstruction` (Gemini only accepts
/// `user`/`model` roles). Assistant tool calls become `functionCall` parts;
/// tool results become `functionResponse` parts keyed by the tool *name*
/// (Gemini has no notion of tool-call ids), resolved from the id→name map of
/// the preceding assistant turn.
fn build_gemini_contents(
    messages: &[Message],
    workspace_root: &Path,
) -> Result<Vec<Value>, ProviderError> {
    let mut out = Vec::new();
    let mut id_to_name: HashMap<String, String> = HashMap::new();

    for message in messages {
        match message.role {
            Role::System => continue,
            Role::User => out.push(json!({
                "role": "user",
                "parts": gemini_parts_from_content(&message.content, workspace_root)?,
            })),
            Role::Assistant => {
                let mut parts = Vec::new();
                let text = content_text(&message.content);
                if !text.is_empty() {
                    parts.push(json!({ "text": text }));
                }
                if let Some(calls) = &message.tool_calls {
                    for call in calls {
                        id_to_name.insert(call.id.clone(), call.name.clone());
                        let mut fc_part = json!({
                            "functionCall": { "name": call.name, "args": call.arguments }
                        });
                        // Gemini requires the original thoughtSignature on the
                        // functionCall part when the assistant turn is replayed.
                        if let Some(sig) = recall_thought_signature(&call.id) {
                            fc_part["thoughtSignature"] = json!(sig);
                        }
                        parts.push(fc_part);
                    }
                }
                // Skip empty assistant turns (common compaction artifact) — an
                // empty `model` turn breaks Gemini's user/model alternation.
                if parts.is_empty() {
                    continue;
                }
                out.push(json!({ "role": "model", "parts": parts }));
            }
            Role::Tool => {
                let name = message
                    .tool_call_id
                    .as_ref()
                    .and_then(|id| id_to_name.get(id))
                    .cloned()
                    .unwrap_or_else(|| "tool".to_string());
                out.push(json!({
                    "role": "user",
                    "parts": [{
                        "functionResponse": {
                            "name": name,
                            "response": { "output": content_text(&message.content) },
                        }
                    }],
                }));
            }
        }
    }

    Ok(normalize_gemini_contents(out))
}

/// Enforce Gemini's `contents` constraints that an arbitrary (post-compaction)
/// dcode history may violate:
///  - turns must alternate `user`/`model` — merge consecutive same-role turns,
///  - a `functionCall` (model) turn must follow a `user`/`functionResponse`
///    turn, and the sequence must begin with `user`; a leading `model` turn
///    (e.g. a summary the compactor stored as an assistant message) otherwise
///    triggers "function call turn must come immediately after a user turn".
fn normalize_gemini_contents(turns: Vec<Value>) -> Vec<Value> {
    let mut merged: Vec<Value> = Vec::with_capacity(turns.len());
    for turn in turns {
        let same_role = merged
            .last()
            .is_some_and(|last| last["role"] == turn["role"]);
        if same_role {
            let last = merged.last_mut().expect("checked non-empty");
            let mut parts = last["parts"].as_array().cloned().unwrap_or_default();
            if let Some(next) = turn["parts"].as_array() {
                parts.extend(next.iter().cloned());
            }
            last["parts"] = Value::Array(parts);
        } else {
            merged.push(turn);
        }
    }
    // Guarantee a user-first sequence so any leading model/functionCall turn is
    // preceded by a user turn.
    if merged.first().is_some_and(|first| first["role"] == "model") {
        merged.insert(
            0,
            json!({ "role": "user", "parts": [{ "text": "Continue." }] }),
        );
    }
    merged
}

/// Collect all system turns into a Gemini `systemInstruction` object.
fn build_system_instruction(messages: &[Message]) -> Option<Value> {
    let joined = messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| content_text(&m.content))
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
    if joined.is_empty() {
        None
    } else {
        Some(json!({ "parts": [{ "text": joined }] }))
    }
}

/// Translate dcode tool definitions to a Gemini `tools` array.
fn build_gemini_tools(tools: &[ToolDefinition]) -> Option<Value> {
    if tools.is_empty() {
        return None;
    }
    let decls = tools
        .iter()
        .map(|tool| {
            json!({
                "name": tool.name,
                "description": tool.description,
                "parameters": sanitize_schema(&tool.parameters),
            })
        })
        .collect::<Vec<_>>();
    Some(json!([{ "functionDeclarations": decls }]))
}

/// Strip JSON-Schema keys that Gemini's `functionDeclarations` validator
/// rejects (`$schema`, `additionalProperties`), recursively.
fn sanitize_schema(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                if k == "$schema" || k == "additionalProperties" {
                    continue;
                }
                out.insert(k.clone(), sanitize_schema(v));
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(sanitize_schema).collect()),
        other => other.clone(),
    }
}

/// Spawn a task that parses the Antigravity SSE stream (Gemini
/// `generateContent` chunks wrapped in a `response` envelope) into
/// [`StreamChunk`]s.
fn spawn_gemini_stream(
    response: reqwest::Response,
    provider_name: &'static str,
) -> tokio::sync::mpsc::Receiver<StreamChunk> {
    let mut byte_stream = response.bytes_stream();
    let (tx, rx) = tokio::sync::mpsc::channel(64);

    tokio::spawn(async move {
        let mut buffer = String::new();

        loop {
            let item = match tokio::time::timeout(STREAM_CHUNK_TIMEOUT, byte_stream.next()).await {
                Ok(Some(item)) => item,
                Ok(None) => break,
                Err(_elapsed) => {
                    let _ = tx
                        .send(StreamChunk::Error(format!(
                            "{provider_name} stream timed out (no data for {}s)",
                            STREAM_CHUNK_TIMEOUT.as_secs()
                        )))
                        .await;
                    break;
                }
            };
            let chunk = match item {
                Ok(chunk) => chunk,
                Err(err) => {
                    let _ = tx
                        .send(StreamChunk::Error(format!(
                            "{provider_name} stream error: {err}"
                        )))
                        .await;
                    break;
                }
            };

            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(nl) = buffer.find('\n') {
                let raw = buffer[..nl].to_string();
                buffer.drain(..=nl);
                let line = raw.trim_end_matches('\r').trim();

                if line.is_empty() || line.starts_with(':') {
                    continue;
                }
                if !line.starts_with("data:") {
                    continue;
                }
                let data = line["data:".len()..].trim();
                if data == "[DONE]" {
                    let _ = tx.send(StreamChunk::Done).await;
                    return;
                }

                let Ok(event) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                // Antigravity wraps the Gemini payload in a `response` envelope;
                // some surfaces omit it, so fall back to the root object.
                let root = event.get("response").unwrap_or(&event);

                emit_gemini_candidates(&tx, root).await;

                if let Some(usage) = root.get("usageMetadata") {
                    let input_tokens = usage["promptTokenCount"].as_u64().unwrap_or(0);
                    let output_tokens = usage["candidatesTokenCount"].as_u64().unwrap_or(0);
                    if input_tokens > 0 || output_tokens > 0 {
                        let _ = tx
                            .send(StreamChunk::Usage {
                                input_tokens,
                                output_tokens,
                            })
                            .await;
                    }
                }
            }
        }

        let _ = tx.send(StreamChunk::Done).await;
    });

    rx
}

/// Emit text / thinking / tool-call chunks from a Gemini response object.
async fn emit_gemini_candidates(tx: &tokio::sync::mpsc::Sender<StreamChunk>, root: &Value) {
    let Some(candidates) = root.get("candidates").and_then(|c| c.as_array()) else {
        return;
    };
    for candidate in candidates {
        let Some(parts) = candidate
            .pointer("/content/parts")
            .and_then(|p| p.as_array())
        else {
            continue;
        };
        for part in parts {
            if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                if text.is_empty() {
                    continue;
                }
                let is_thought = part.get("thought").and_then(|t| t.as_bool()) == Some(true);
                let chunk = if is_thought {
                    StreamChunk::InternalDelta(text.to_string())
                } else {
                    StreamChunk::TextDelta(text.to_string())
                };
                let _ = tx.send(chunk).await;
            } else if let Some(function_call) = part.get("functionCall") {
                let name = function_call
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default()
                    .to_string();
                if name.is_empty() {
                    continue;
                }
                let input = function_call
                    .get("args")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let seq = TOOL_CALL_SEQ.fetch_add(1, Ordering::Relaxed);
                let id = format!("antigravity-{seq}-{name}");
                // Capture the thoughtSignature (sibling of functionCall on the
                // part) so it can be replayed on the next turn.
                if let Some(sig) = part.get("thoughtSignature").and_then(|s| s.as_str()) {
                    remember_thought_signature(&id, sig);
                }
                let _ = tx
                    .send(StreamChunk::ToolUse(ToolCall { id, name, input }))
                    .await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::test_support::{collect_chunks, spawn_sse_server};
    use dcode_ai_common::message::MessageToolCall;

    #[test]
    fn vertex_endpoint_global_uses_locationless_host() {
        let auth = VertexAuth {
            project_id: "my-proj".into(),
            location: "global".into(),
        };
        assert_eq!(
            vertex_endpoint(&auth, "gemini-3-pro-preview"),
            "https://aiplatform.googleapis.com/v1/projects/my-proj/locations/global/publishers/google/models/gemini-3-pro-preview:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn vertex_endpoint_regional_prefixes_host() {
        let auth = VertexAuth {
            project_id: "p1".into(),
            location: "us-central1".into(),
        };
        assert_eq!(
            vertex_endpoint(&auth, "gemini-2.5-pro"),
            "https://us-central1-aiplatform.googleapis.com/v1/projects/p1/locations/us-central1/publishers/google/models/gemini-2.5-pro:streamGenerateContent?alt=sse"
        );
    }

    #[test]
    fn contents_hoist_system_and_map_tool_results() {
        let messages = vec![
            Message::system("be terse"),
            Message::user("hi"),
            Message::assistant_with_tool_calls(
                "",
                vec![MessageToolCall {
                    id: "call_1".into(),
                    name: "read_file".into(),
                    arguments: json!({ "path": "Cargo.toml" }),
                }],
            ),
            Message::tool("call_1", "ok"),
        ];

        let contents = build_gemini_contents(&messages, Path::new(".")).expect("contents");
        // system excluded from contents
        assert_eq!(contents.len(), 3);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[1]["role"], "model");
        assert_eq!(contents[1]["parts"][0]["functionCall"]["name"], "read_file");
        // tool result resolved back to the function name
        assert_eq!(contents[2]["role"], "user");
        assert_eq!(
            contents[2]["parts"][0]["functionResponse"]["name"],
            "read_file"
        );

        let system = build_system_instruction(&messages).expect("system");
        assert_eq!(system["parts"][0]["text"], "be terse");
    }

    #[test]
    fn splits_effort_suffix_and_builds_thinking_config() {
        assert_eq!(
            split_model_tier("gemini-3.5-flash-low"),
            ("gemini-3.5-flash".to_string(), Some("low"))
        );
        assert_eq!(
            split_model_tier("gemini-2.5-flash"),
            ("gemini-2.5-flash".to_string(), None)
        );
        assert_eq!(
            thinking_config("gemini-3.5-flash", "low")["thinkingLevel"],
            "LOW"
        );
        assert_eq!(
            thinking_config("gemini-3-pro", "high")["thinkingLevel"],
            "HIGH"
        );
        assert_eq!(
            thinking_config("gemini-2.5-flash", "high")["thinkingBudget"],
            16_384
        );
        assert_eq!(
            thinking_config("claude-sonnet-4-6", "medium")["thinkingBudget"],
            16_384
        );
    }

    #[test]
    fn normalize_merges_consecutive_model_turns_and_forces_user_first() {
        // Post-compaction shape: a summary stored as an assistant turn, followed
        // by another assistant turn carrying a leftover function call.
        let messages = vec![
            Message::assistant("summary of prior work"),
            Message::assistant_with_tool_calls(
                "",
                vec![MessageToolCall {
                    id: "c1".into(),
                    name: "list_directory".into(),
                    arguments: json!({ "path": "." }),
                }],
            ),
            Message::user("continue"),
        ];
        let contents = build_gemini_contents(&messages, Path::new(".")).expect("contents");

        // Leading synthetic user turn, then a single merged model turn, then user.
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[1]["role"], "model");
        let parts = contents[1]["parts"].as_array().unwrap();
        assert!(parts.iter().any(|p| p.get("text").is_some()));
        assert!(parts.iter().any(|p| p.get("functionCall").is_some()));
        assert_eq!(contents.last().unwrap()["role"], "user");
        // No two consecutive turns share a role.
        for w in contents.windows(2) {
            assert_ne!(w[0]["role"], w[1]["role"]);
        }
    }

    #[test]
    fn tools_wrap_in_function_declarations_and_strip_unsupported_keys() {
        let tools = vec![ToolDefinition {
            name: "read_file".into(),
            description: "Read a file".into(),
            parameters: json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "additionalProperties": false,
                "properties": { "path": { "type": "string" } }
            }),
        }];
        let value = build_gemini_tools(&tools).expect("tools");
        let decl = &value[0]["functionDeclarations"][0];
        assert_eq!(decl["name"], "read_file");
        assert!(decl["parameters"].get("$schema").is_none());
        assert!(decl["parameters"].get("additionalProperties").is_none());
        assert_eq!(decl["parameters"]["properties"]["path"]["type"], "string");
    }

    #[tokio::test]
    async fn stream_parses_text_thinking_tool_and_usage() {
        let body = concat!(
            "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"thought\":true,\"text\":\"pondering\"}]}}]}}\n\n",
            "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"text\":\"Hello \"}]}}]}}\n\n",
            "data: {\"response\":{\"candidates\":[{\"content\":{\"parts\":[{\"functionCall\":{\"name\":\"lookup\",\"args\":{\"path\":\"src\"}}}]}}]}}\n\n",
            "data: {\"response\":{\"usageMetadata\":{\"promptTokenCount\":11,\"candidatesTokenCount\":7}}}\n\n",
        )
        .to_string();
        let base_url = spawn_sse_server(body, 200, |_| {});
        let response = reqwest::Client::new()
            .get(base_url)
            .send()
            .await
            .expect("mock response");

        let chunks = collect_chunks(spawn_gemini_stream(response, "antigravity")).await;

        assert!(matches!(&chunks[0], StreamChunk::InternalDelta(t) if t == "pondering"));
        assert!(matches!(&chunks[1], StreamChunk::TextDelta(t) if t == "Hello "));
        assert!(
            matches!(&chunks[2], StreamChunk::ToolUse(call) if call.name == "lookup" && call.input == json!({"path":"src"}))
        );
        assert!(matches!(
            &chunks[3],
            StreamChunk::Usage {
                input_tokens: 11,
                output_tokens: 7
            }
        ));
        assert!(matches!(chunks.last(), Some(StreamChunk::Done)));
    }
}
