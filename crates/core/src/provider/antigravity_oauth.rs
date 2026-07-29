//! Antigravity OAuth provider — a second, independently-loggable identity.
//!
//! Two modes, tried in this order:
//! - **OAuth login** (preferred; `dcode-ai login antigravity-oauth`): the
//!   real "sign in with Google, no API key" experience. Routes through the
//!   SAME Cloud Code Assist backend (`cloudcode-pa.googleapis.com`) the
//!   "Antigravity" provider (`antigravity.rs`) already uses, via the same
//!   bundled OAuth client — just stored under its own credential slot
//!   (`antigravity_oauth`, separate from `antigravity`) so the two logins
//!   don't overwrite each other. Confirmed against Google's own `gemini-cli`:
//!   its "Sign in with Google" flow calls `loadCodeAssist` on this exact
//!   backend, not the plain Gemini Developer API — that API's OAuth support
//!   is documented as tuning/semantic-retrieval only, not general chat.
//! - **API key** (fallback, when no OAuth login exists): talks to
//!   `generativelanguage.googleapis.com` directly — Google's documented auth
//!   method for that specific API (aistudio.google.com/apikey).

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use dcode_ai_common::auth::{AntigravityOAuthAuth, AuthStore};
use dcode_ai_common::config::{DcodeAiConfig, OpenAiConfig};
use dcode_ai_common::message::{ContentPart, Message, MessageContent, Role};
use dcode_ai_common::tool::{ToolCall, ToolDefinition};
use futures_util::StreamExt;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap, HeaderValue};
use serde_json::{Value, json};

use super::antigravity::{
    ANTIGRAVITY_API_CLIENT, ANTIGRAVITY_CLIENT_METADATA, ANTIGRAVITY_DEFAULT_PROJECT,
    ANTIGRAVITY_ENDPOINT, antigravity_user_agent, spawn_gemini_stream,
};
use super::openai_compat::map_provider_error;
use super::{Provider, ProviderCapabilities, ProviderError, StreamChunk, retry};

/// Gemini API base — the model id and `:streamGenerateContent` action are
/// appended per-request (the API requires the model in the URL path, e.g.
/// `.../v1beta/models/gemini-2.5-flash:streamGenerateContent`).
const GEMINI_API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta/models";

/// Default model for the Gemini API.
const GEMINI_DEFAULT_MODEL: &str = "gemini-2.5-flash";

/// Process-global monotonic counter so tool-call ids are unique across turns.
static TOOL_CALL_SEQ: AtomicU64 = AtomicU64::new(1);

/// Maps a tool-call id → the Gemini `thoughtSignature` that accompanied the
/// model's `functionCall`.
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

pub struct AntigravityOAuthProvider {
    client: reqwest::Client,
    config: OpenAiConfig,
    max_tokens: u32,
    auth: AntigravityOAuthMode,
}

/// Which backend this provider talks to for a given login state.
enum AntigravityOAuthMode {
    /// The real Google-account login, via Cloud Code Assist.
    CloudCodeAssist,
    /// Plain Gemini Developer API key.
    ApiKey(String),
}

impl AntigravityOAuthProvider {
    pub fn from_config(config: &DcodeAiConfig) -> Result<Self, ProviderError> {
        let openai = config.provider.openai.clone();

        // Prefer a real Google OAuth login (Cloud Code Assist — the actual
        // "sign in with Google" experience) over a bare API key; the key
        // path stays available for anyone who specifically wants the plain
        // Gemini Developer API instead. Error only if neither is configured.
        let auth_store = AuthStore::load().ok().unwrap_or_default();
        let auth = if auth_store.antigravity_oauth.is_some() {
            AntigravityOAuthMode::CloudCodeAssist
        } else if let Some(key) = openai.resolve_api_key() {
            AntigravityOAuthMode::ApiKey(key)
        } else {
            return Err(ProviderError::Configuration(
                "missing Antigravity OAuth credentials — run `dcode-ai login antigravity-oauth` \
                 (recommended) or set an API key (aistudio.google.com/apikey)"
                    .to_string(),
            ));
        };

        // Cloud Code Assist gates on these headers (client-version check),
        // the same as the "Antigravity" provider's client — harmless extras
        // on the plain Gemini API key path, which ignores unrecognized
        // headers, so one client setup covers both modes.
        let mut headers = HeaderMap::new();
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
            max_tokens: config.model.max_tokens,
            auth,
        })
    }

    /// Resolve the bearer token for a request. Refreshes if expired.
    async fn access_token(&self) -> Result<String, ProviderError> {
        let auth_store = AuthStore::load().ok().unwrap_or_default();
        let oauth = auth_store.antigravity_oauth.ok_or_else(|| {
            ProviderError::Configuration(
                "missing Antigravity OAuth login; run `dcode-ai login antigravity-oauth`".into(),
            )
        })?;

        // Check if token is expired (with 5-minute buffer)
        if let Some(expires_at) = oauth.expires_at
            && chrono::Utc::now().timestamp() + 300 < expires_at
        {
            return Ok(oauth.access_token);
        }

        // Refresh the token
        let refreshed = self.refresh_token(&oauth).await?;
        Ok(refreshed.access_token)
    }

    /// Refresh the OAuth token using the refresh_token — same bundled client
    /// `login_antigravity_oauth` used to obtain it in the first place.
    async fn refresh_token(
        &self,
        oauth: &AntigravityOAuthAuth,
    ) -> Result<AntigravityOAuthAuth, ProviderError> {
        let client_id = dcode_ai_common::secrets::antigravity_client_id();
        let client_secret = dcode_ai_common::secrets::antigravity_client_secret();

        let params = [
            ("client_id", client_id.as_str()),
            ("client_secret", client_secret.as_str()),
            ("refresh_token", oauth.refresh_token.as_str()),
            ("grant_type", "refresh_token"),
        ];

        let resp = self
            .client
            .post("https://oauth2.googleapis.com/token")
            .form(&params)
            .send()
            .await
            .map_err(|e| ProviderError::RequestFailed(format!("token refresh failed: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(ProviderError::Configuration(format!(
                "token refresh failed {status}: {text}"
            )));
        }

        let v: serde_json::Value = resp.json().await.map_err(|e| {
            ProviderError::Configuration(format!("invalid token refresh response: {e}"))
        })?;

        let access_token = v
            .get("access_token")
            .and_then(|x| x.as_str())
            .ok_or_else(|| ProviderError::Configuration("missing access_token in refresh".into()))?
            .to_string();
        let expires_in = v.get("expires_in").and_then(|x| x.as_i64()).unwrap_or(3600);
        let expires_at = chrono::Utc::now().timestamp() + expires_in - 300;

        let mut auth_store = AuthStore::load().ok().unwrap_or_default();
        let new_oauth = AntigravityOAuthAuth {
            access_token: access_token.clone(),
            refresh_token: oauth.refresh_token.clone(),
            expires_at: Some(expires_at),
        };
        auth_store.antigravity_oauth = Some(new_oauth.clone());
        auth_store.save().ok();

        Ok(new_oauth)
    }

    /// Build the inner Gemini `generateContent` request plus the base model id.
    fn request_parts(
        &self,
        messages: &[Message],
        tools: &[ToolDefinition],
        model: &str,
        workspace_root: &Path,
    ) -> Result<(Value, String), ProviderError> {
        let contents = build_gemini_contents(messages, workspace_root)?;

        // Gemini 2.5+/3.x models think by default even with no thinkingConfig
        // sent at all, and thinking tokens are deducted from maxOutputTokens
        // — an unbounded default thinking budget can consume the entire
        // output budget and return zero visible text ("empty completion").
        // Always send a bounded config; this provider's catalog doesn't
        // encode an effort tier in the model name (unlike Antigravity/Vertex),
        // so default to "low" for every request.
        let generation_config = json!({
            "temperature": self.config.temperature,
            "maxOutputTokens": self.max_tokens,
            "thinkingConfig": super::antigravity::thinking_config(model, "low"),
        });

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

        Ok((request, model.to_string()))
    }
}

#[async_trait::async_trait]
impl Provider for AntigravityOAuthProvider {
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
            let configured = self.config.model.clone();
            if configured.is_empty() {
                GEMINI_DEFAULT_MODEL.to_string()
            } else {
                configured
            }
        } else {
            model.to_string()
        };

        let (request, base_model) = self.request_parts(messages, tools, &model, workspace_root)?;

        match &self.auth {
            AntigravityOAuthMode::CloudCodeAssist => {
                // Same envelope/endpoint/streaming shape as the "Antigravity"
                // provider's Cloud Code Assist path (antigravity.rs) — reused
                // rather than duplicated, see the module doc comment.
                let access_token = self.access_token().await?;
                let bearer =
                    HeaderValue::from_str(&format!("Bearer {access_token}")).map_err(|err| {
                        ProviderError::Configuration(format!(
                            "failed to build authorization header: {err}"
                        ))
                    })?;
                let body = json!({
                    "project": ANTIGRAVITY_DEFAULT_PROJECT,
                    "model": base_model,
                    "request": request,
                    "requestType": "agent",
                    "userAgent": "antigravity",
                    "requestId": format!("agent-{}", chrono::Utc::now().timestamp_millis()),
                });

                let response = retry::with_retry(retry::DEFAULT_MAX_ATTEMPTS, || async {
                    let resp = self
                        .client
                        .post(ANTIGRAVITY_ENDPOINT)
                        .header(AUTHORIZATION, bearer.clone())
                        .header(ACCEPT, "text/event-stream")
                        .json(&body)
                        .send()
                        .await
                        .map_err(ProviderError::from_reqwest_send)?;
                    let status = resp.status();
                    if status.as_u16() == 429 || status.is_server_error() {
                        let text = resp.text().await.unwrap_or_default();
                        return Err(map_provider_error(status, text));
                    }
                    if !status.is_success() {
                        let text = resp.text().await.unwrap_or_default();
                        return Err(ProviderError::RequestFailed(format!(
                            "Cloud Code Assist returned {status}: {text}"
                        )));
                    }
                    Ok(resp)
                })
                .await?;

                Ok(spawn_gemini_stream(response, "antigravity-oauth"))
            }
            AntigravityOAuthMode::ApiKey(api_key) => {
                let mut url = reqwest::Url::parse(&format!(
                    "{GEMINI_API_BASE}/{base_model}:streamGenerateContent"
                ))
                .map_err(|e| ProviderError::Configuration(format!("bad request URL: {e}")))?;
                url.query_pairs_mut().append_pair("alt", "sse");
                url.query_pairs_mut().append_pair("key", api_key);

                let response = retry::with_retry(retry::DEFAULT_MAX_ATTEMPTS, || async {
                    let resp = self
                        .client
                        .post(url.clone())
                        .header(ACCEPT, "text/event-stream")
                        .json(&request)
                        .send()
                        .await
                        .map_err(ProviderError::from_reqwest_send)?;
                    let status = resp.status();
                    if status.as_u16() == 429 || status.is_server_error() {
                        let text = resp.text().await.unwrap_or_default();
                        return Err(map_provider_error(status, text));
                    }
                    if !status.is_success() {
                        let text = resp.text().await.unwrap_or_default();
                        return Err(ProviderError::RequestFailed(format!(
                            "Gemini API returned {status}: {text}"
                        )));
                    }
                    Ok(resp)
                })
                .await?;

                // This path's own parser (unwrapped Gemini shape — no Cloud
                // Code Assist `response` envelope to unwrap), unchanged.
                let (tx, rx) = tokio::sync::mpsc::channel(256);
                tokio::spawn(async move {
                    let mut stream = response.bytes_stream();
                    let mut buf = String::new();
                    let mut usage_done = false;

                    while let Some(chunk_result) = stream.next().await {
                        let chunk = match chunk_result {
                            Ok(c) => c,
                            Err(_e) => {
                                let _ = tx.send(StreamChunk::Done).await;
                                return;
                            }
                        };
                        buf.push_str(&String::from_utf8_lossy(&chunk));

                        while let Some(newline_pos) = buf.find('\n') {
                            let line = buf[..newline_pos].to_string();
                            buf = buf[newline_pos + 1..].to_string();

                            let line = line.trim();
                            if line.is_empty() {
                                continue;
                            }

                            if let Some(data) = line.strip_prefix("data: ")
                                && let Ok(v) = serde_json::from_str::<serde_json::Value>(data)
                            {
                                process_gemini_chunk(&v, &tx, &mut usage_done).await;
                            }
                        }
                    }

                    if !usage_done {
                        let _ = tx.send(StreamChunk::Done).await;
                    }
                });

                Ok(rx)
            }
        }
    }
}

async fn process_gemini_chunk(
    v: &Value,
    tx: &tokio::sync::mpsc::Sender<StreamChunk>,
    usage_done: &mut bool,
) {
    // Check for usage metadata
    if let Some(usage) = v.get("usageMetadata") {
        let input_tokens = usage
            .get("promptTokenCount")
            .and_then(|t| t.as_u64())
            .unwrap_or(0);
        let output_tokens = usage
            .get("candidatesTokenCount")
            .and_then(|t| t.as_u64())
            .unwrap_or(0);
        let _ = tx
            .send(StreamChunk::Usage {
                input_tokens,
                output_tokens,
            })
            .await;
        let _ = tx.send(StreamChunk::Done).await;
        *usage_done = true;
        return;
    }

    // Process candidates
    if let Some(candidates) = v.get("candidates").and_then(|c| c.as_array()) {
        for candidate in candidates {
            if let Some(content) = candidate.get("content")
                && let Some(parts) = content.get("parts").and_then(|p| p.as_array())
            {
                for part in parts {
                    // Thinking text
                    if part.get("thought").and_then(|t| t.as_bool()) == Some(true) {
                        if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                            let _ = tx.send(StreamChunk::InternalDelta(text.to_string())).await;
                        }
                        continue;
                    }

                    // Regular text
                    if let Some(text) = part.get("text").and_then(|t| t.as_str()) {
                        let _ = tx.send(StreamChunk::TextDelta(text.to_string())).await;
                    }

                    // Function call
                    if let Some(function_call) = part.get("functionCall") {
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
                        let id = format!("gemini-oauth-{seq}-{name}");

                        // Capture thoughtSignature if present
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
    }
}

/// Build the Gemini `contents` array from a message list. System turns are
/// hoisted into `systemInstruction` separately (see [`build_system_instruction`]),
/// not emitted here. Assistant tool calls become `functionCall` parts (replaying
/// the captured `thoughtSignature` so a continued tool-call turn isn't rejected);
/// tool results become `functionResponse` parts keyed by the tool *name* (Gemini
/// has no notion of tool-call ids), resolved from the id→name map built while
/// walking the preceding assistant turn.
fn build_gemini_contents(
    messages: &[Message],
    workspace_root: &Path,
) -> Result<Vec<Value>, ProviderError> {
    let mut contents = Vec::new();
    let mut id_to_name: HashMap<String, String> = HashMap::new();

    for msg in messages {
        match msg.role {
            Role::System => continue,
            Role::User => {
                let parts = build_content_parts(&msg.content, workspace_root)?;
                contents.push(json!({ "role": "user", "parts": parts }));
            }
            Role::Assistant => {
                let mut parts = build_content_parts(&msg.content, workspace_root)?;
                if let Some(tool_calls) = &msg.tool_calls {
                    for tc in tool_calls {
                        id_to_name.insert(tc.id.clone(), tc.name.clone());
                        let mut fc_part = json!({
                            "functionCall": { "name": tc.name, "args": tc.arguments }
                        });
                        if let Some(sig) = recall_thought_signature(&tc.id) {
                            fc_part["thoughtSignature"] = json!(sig);
                        }
                        parts.push(fc_part);
                    }
                }
                // Skip empty assistant turns (compaction artifact) — an empty
                // `model` turn breaks Gemini's user/model alternation.
                if parts.is_empty() {
                    continue;
                }
                contents.push(json!({ "role": "model", "parts": parts }));
            }
            Role::Tool => {
                let name = msg
                    .tool_call_id
                    .as_ref()
                    .and_then(|id| id_to_name.get(id))
                    .cloned()
                    .unwrap_or_else(|| "tool".to_string());
                contents.push(json!({
                    "role": "user",
                    "parts": [{
                        "functionResponse": {
                            "name": name,
                            "response": { "output": content_text(&msg.content) },
                        }
                    }]
                }));
            }
        }
    }

    Ok(contents)
}

fn content_text(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(t) => t.clone(),
        MessageContent::Parts(_) => content.to_summary_text(),
    }
}

/// Build the system instruction from messages.
fn build_system_instruction(messages: &[Message]) -> Option<Value> {
    for msg in messages {
        if matches!(msg.role, Role::System)
            && let MessageContent::Text(text) = &msg.content
        {
            return Some(json!({
                "parts": [{"text": text}]
            }));
        }
    }
    None
}

/// Build Gemini `parts` for a message's content, embedding on-disk images
/// (referenced by workspace-relative path, per [`ContentPart::Image`]) as
/// base64 `inlineData` blocks. Tool-call parts are added by the caller, which
/// has the message's role available to decide `functionCall` vs `functionResponse`.
fn build_content_parts(
    content: &MessageContent,
    workspace_root: &Path,
) -> Result<Vec<Value>, ProviderError> {
    match content {
        MessageContent::Text(text) => Ok(vec![json!({ "text": text })]),
        MessageContent::Parts(content_parts) => {
            let mut parts = Vec::new();
            for part in content_parts {
                match part {
                    ContentPart::Text { text } => {
                        parts.push(json!({ "text": text }));
                    }
                    ContentPart::Image { media_type, path } => {
                        let full = workspace_root.join(path);
                        let bytes = std::fs::read(&full).map_err(|e| {
                            ProviderError::RequestFailed(format!(
                                "failed to read image {}: {e}",
                                full.display()
                            ))
                        })?;
                        let b64 = B64.encode(bytes);
                        parts.push(json!({
                            "inlineData": { "mimeType": media_type, "data": b64 }
                        }));
                    }
                }
            }
            if parts.is_empty() {
                parts.push(json!({ "text": "" }));
            }
            Ok(parts)
        }
    }
}

/// Build tools for Gemini API.
fn build_gemini_tools(tools: &[ToolDefinition]) -> Option<Value> {
    if tools.is_empty() {
        return None;
    }

    let declarations: Vec<Value> = tools
        .iter()
        .map(|tool| {
            let mut params = tool.parameters.clone();
            // Strip unsupported keys
            if let Some(obj) = params.as_object_mut() {
                obj.remove("$schema");
                obj.remove("additionalProperties");
            }
            json!({
                "functionDeclarations": [{
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": params,
                }]
            })
        })
        .collect();

    Some(json!(declarations))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contents_hoist_system_and_map_tool_results() {
        let messages = vec![Message::system("be terse"), Message::user("hi")];
        let contents = build_gemini_contents(&messages, Path::new(".")).expect("contents");
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");

        let system = build_system_instruction(&messages).expect("system");
        assert_eq!(system["parts"][0]["text"], "be terse");
    }

    #[test]
    fn tools_wrap_in_function_declarations() {
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
    }
}
