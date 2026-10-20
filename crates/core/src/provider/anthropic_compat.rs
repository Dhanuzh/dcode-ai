use std::path::Path;

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use dcode_ai_common::message::{ContentPart, Message, MessageContent, Role};
use dcode_ai_common::tool::{ToolCall, ToolDefinition};
use futures_util::StreamExt;
use serde_json::{Value, json};

use super::{ProviderError, StopReason, StreamChunk};

/// How a Claude model family accepts reasoning and sampling parameters.
/// The Messages API rejects (HTTP 400) parameters a family no longer supports,
/// so the request shape must follow the model, not the user's config alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeFamily {
    /// Fable 5.x, Mythos, Opus 5.x, Sonnet 5.x, Haiku 5.x, Opus 4.7/4.8:
    /// adaptive thinking only; `temperature`/`top_p`/`budget_tokens` are 400s.
    AdaptiveNoSampling,
    /// Opus 4.6 / Sonnet 4.6: adaptive thinking; sampling still accepted.
    AdaptiveWithSampling,
    /// Haiku 4.5 and older: `{type: "enabled", budget_tokens}` thinking.
    Legacy,
}

/// Anthropic model ids use hyphenated versions (`claude-sonnet-4-6`); users
/// and other catalogs often write `claude-sonnet-4.6`, which the API 404s.
pub fn normalize_claude_model_id(model: &str) -> String {
    let m = model.trim();
    if m.to_ascii_lowercase().starts_with("claude-") && m.contains('.') {
        m.replace('.', "-")
    } else {
        m.to_string()
    }
}

impl ClaudeFamily {
    pub fn for_model(model: &str) -> Self {
        let m = normalize_claude_model_id(model).to_ascii_lowercase();
        const ADAPTIVE_NO_SAMPLING: &[&str] = &[
            "fable", "mythos", "opus-5", "sonnet-5", "haiku-5", "opus-4-7", "opus-4-8",
        ];
        if ADAPTIVE_NO_SAMPLING.iter().any(|p| m.contains(p)) {
            ClaudeFamily::AdaptiveNoSampling
        } else if m.contains("opus-4-6") || m.contains("sonnet-4-6") {
            ClaudeFamily::AdaptiveWithSampling
        } else {
            ClaudeFamily::Legacy
        }
    }

    pub fn is_adaptive(self) -> bool {
        !matches!(self, ClaudeFamily::Legacy)
    }
}

/// Reasoning knobs for an Anthropic request.
#[derive(Debug, Clone, Default)]
pub struct AnthropicRequestOptions {
    /// Ask for reasoning (adaptive on current models, budgeted on legacy ones).
    /// Models that always think ignore `false`; we simply omit the parameter.
    pub thinking: bool,
    /// Legacy-family thinking budget (min 1024, must be < max_tokens).
    pub thinking_budget: u32,
    /// `output_config.effort`: low | medium | high | xhigh | max.
    pub effort: Option<String>,
}

pub fn anthropic_request_body(
    messages: &[Message],
    tools: &[ToolDefinition],
    model: &str,
    max_tokens: u32,
    temperature: f32,
    workspace_root: &Path,
) -> Result<Value, ProviderError> {
    anthropic_request_body_with(
        messages,
        tools,
        model,
        max_tokens,
        temperature,
        workspace_root,
        &AnthropicRequestOptions::default(),
    )
}

pub fn anthropic_request_body_with(
    messages: &[Message],
    tools: &[ToolDefinition],
    model: &str,
    max_tokens: u32,
    temperature: f32,
    workspace_root: &Path,
    options: &AnthropicRequestOptions,
) -> Result<Value, ProviderError> {
    let family = ClaudeFamily::for_model(model);
    let (system, mut anthropic_messages) = to_anthropic_messages(messages, workspace_root)?;

    // Prompt caching: the system prompt and tool schemas are the large, static
    // prefix that repeats verbatim on every turn. Marking them with an
    // `ephemeral` cache_control breakpoint lets Anthropic serve them from cache
    // (~0.1x input cost, GA under anthropic-version 2023-06-01), which is the
    // dominant token saving in a multi-turn agent loop.
    //
    // A third breakpoint on the final message caches the *conversation* prefix
    // incrementally: each turn's breakpoint sits one message later, and Anthropic
    // reuses the longest previously-cached prefix. (Max 4 breakpoints; we use 3.)
    if let Some(last) = anthropic_messages.last_mut() {
        mark_last_block_cacheable(last);
    }
    let tools = if tools.is_empty() {
        None
    } else {
        let mut defs = tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.parameters,
                })
            })
            .collect::<Vec<_>>();
        if let Some(last) = defs.last_mut() {
            last["cache_control"] = json!({ "type": "ephemeral" });
        }
        Some(defs)
    };

    let mut body = json!({
        "model": model,
        "max_tokens": max_tokens,
        "messages": anthropic_messages,
        "stream": true,
    });
    // Omit `tools` when there are none: `"tools": null` is rejected by some
    // Messages endpoints (GitHub Copilot: "tools must be an array"), which
    // broke tool-less requests such as compaction summaries.
    if let Some(tools) = tools {
        body["tools"] = json!(tools);
    }

    match family {
        ClaudeFamily::AdaptiveNoSampling | ClaudeFamily::AdaptiveWithSampling => {
            if options.thinking {
                // `summarized` so the TUI's thinking stream has text; the
                // default on current models is `omitted` (empty blocks).
                body["thinking"] = json!({ "type": "adaptive", "display": "summarized" });
            } else if family == ClaudeFamily::AdaptiveWithSampling {
                body["temperature"] = json!(temperature);
            }
            if let Some(effort) = options.effort.as_deref().filter(|e| !e.is_empty()) {
                body["output_config"] = json!({ "effort": effort });
            }
        }
        ClaudeFamily::Legacy => {
            let budget = options.thinking_budget.max(1024);
            if options.thinking && budget < max_tokens {
                // Sampling must stay at its default while thinking is on.
                body["thinking"] = json!({ "type": "enabled", "budget_tokens": budget });
            } else {
                body["temperature"] = json!(temperature);
            }
        }
    }

    // Emit `system` as a cacheable text block when present; omit it entirely
    // otherwise (sending `"system": null` is wasteful and blocks the cache).
    if let Some(system) = system {
        body["system"] = json!([{
            "type": "text",
            "text": system,
            "cache_control": { "type": "ephemeral" },
        }]);
    }

    Ok(body)
}

/// Attach an `ephemeral` cache_control breakpoint to the last content block of
/// an Anthropic message. String content is promoted to a single text block so
/// the breakpoint has somewhere to live (Anthropic requires cache_control on a
/// block, not on the bare `content` string).
fn mark_last_block_cacheable(message: &mut Value) {
    let Some(content) = message.get_mut("content") else {
        return;
    };

    match content {
        Value::String(text) => {
            *content = json!([{
                "type": "text",
                "text": text,
                "cache_control": { "type": "ephemeral" },
            }]);
        }
        Value::Array(blocks) => {
            if let Some(last) = blocks.last_mut() {
                last["cache_control"] = json!({ "type": "ephemeral" });
            }
        }
        _ => {}
    }
}

/// Per-chunk timeout: if no bytes arrive within this window the stream is
/// considered dead. Protects against server hangs and network drops that
/// would otherwise block until the OS TCP keepalive fires (~2-5 min).
const STREAM_CHUNK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);

pub fn spawn_anthropic_stream(
    response: reqwest::Response,
    provider_name: &'static str,
) -> tokio::sync::mpsc::Receiver<StreamChunk> {
    let mut byte_stream = response.bytes_stream();
    let (tx, rx) = tokio::sync::mpsc::channel(64);

    tokio::spawn(async move {
        let mut buffer = String::new();
        let mut event_type = String::new();
        let mut tool_id = String::new();
        let mut tool_name = String::new();
        let mut tool_input = String::new();
        let mut input_tokens: u64 = 0;
        // Thinking block being streamed: (text, signature). Echoed back
        // verbatim on the next request so the model keeps its reasoning.
        let mut thinking_block: Option<(String, String)> = None;

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

                if line.is_empty() {
                    event_type.clear();
                    continue;
                }

                if let Some(event) = line.strip_prefix("event:") {
                    event_type = event.trim().to_string();
                    continue;
                }

                if !line.starts_with("data:") {
                    continue;
                }

                let data = line["data:".len()..].trim();
                if data == "[DONE]" {
                    break;
                }

                let Ok(event) = serde_json::from_str::<Value>(data) else {
                    continue;
                };

                match event_type.as_str() {
                    "message_start" => {
                        // Keep cached-prefix tokens separate from fresh input so
                        // each can be priced at its own tier downstream. `input_tokens`
                        // here is the non-cached input only.
                        let usage = &event["message"]["usage"];
                        input_tokens = usage["input_tokens"].as_u64().unwrap_or(0);
                        let cache_read = usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
                        let cache_creation =
                            usage["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                        if cache_read > 0 || cache_creation > 0 {
                            let _ = tx
                                .send(StreamChunk::CacheUsage {
                                    read_tokens: cache_read,
                                    creation_tokens: cache_creation,
                                })
                                .await;
                        }
                    }
                    "content_block_start" => {
                        let block = &event["content_block"];
                        match block["type"].as_str().unwrap_or("") {
                            "tool_use" => {
                                tool_id = block["id"].as_str().unwrap_or("").to_string();
                                tool_name = block["name"].as_str().unwrap_or("").to_string();
                                tool_input.clear();
                            }
                            "thinking" => {
                                thinking_block = Some((
                                    block["thinking"].as_str().unwrap_or("").to_string(),
                                    block["signature"].as_str().unwrap_or("").to_string(),
                                ));
                            }
                            "redacted_thinking" => {
                                let _ = tx.send(StreamChunk::ReplayBlock(block.clone())).await;
                            }
                            _ => {}
                        }
                    }
                    "content_block_delta" => {
                        let delta = &event["delta"];
                        match delta["type"].as_str().unwrap_or("") {
                            "thinking_delta" => {
                                if let Some(text) = delta["thinking"].as_str()
                                    && !text.is_empty()
                                {
                                    if let Some((thinking, _)) = thinking_block.as_mut() {
                                        thinking.push_str(text);
                                    }
                                    let _ =
                                        tx.send(StreamChunk::InternalDelta(text.to_string())).await;
                                }
                            }
                            "signature_delta" => {
                                if let (Some((_, signature)), Some(part)) =
                                    (thinking_block.as_mut(), delta["signature"].as_str())
                                {
                                    signature.push_str(part);
                                }
                            }
                            "text_delta" => {
                                if let Some(text) = delta["text"].as_str()
                                    && !text.is_empty()
                                {
                                    let _ = tx.send(StreamChunk::TextDelta(text.to_string())).await;
                                }
                            }
                            "input_json_delta" => {
                                if let Some(partial) = delta["partial_json"].as_str() {
                                    tool_input.push_str(partial);
                                }
                            }
                            _ => {}
                        }
                    }
                    "content_block_stop" => {
                        if let Some((thinking, signature)) = thinking_block.take() {
                            // Unsigned blocks (proxies, some gateways) can't be
                            // replayed — the API rejects them — so drop them.
                            if signature.is_empty() {
                                continue;
                            }
                            let _ = tx
                                .send(StreamChunk::ReplayBlock(json!({
                                    "type": "thinking",
                                    "thinking": thinking,
                                    "signature": signature,
                                })))
                                .await;
                        } else if !flush_anthropic_tool_call(
                            &tx,
                            &mut tool_id,
                            &mut tool_name,
                            &mut tool_input,
                        )
                        .await
                        {
                            return;
                        }
                    }
                    "error" => {
                        let err = &event["error"];
                        let message = format!(
                            "{provider_name} stream error: {} {}",
                            err["type"].as_str().unwrap_or("error"),
                            err["message"].as_str().unwrap_or(data)
                        );
                        let _ = tx.send(StreamChunk::Error(message)).await;
                        return;
                    }
                    "message_delta" => {
                        if let Some(reason) = event["delta"]["stop_reason"].as_str() {
                            let stop = match reason {
                                "end_turn" | "stop_sequence" => StopReason::EndTurn,
                                "tool_use" => StopReason::ToolUse,
                                "max_tokens" => StopReason::MaxTokens,
                                "refusal" => StopReason::Refusal {
                                    category: event["delta"]["stop_details"]["category"]
                                        .as_str()
                                        .or_else(|| event["stop_details"]["category"].as_str())
                                        .map(str::to_string),
                                },
                                other => StopReason::Other(other.to_string()),
                            };
                            let _ = tx.send(StreamChunk::Stop(stop)).await;
                        }
                        let output_tokens = event["usage"]["output_tokens"].as_u64().unwrap_or(0);
                        if input_tokens > 0 || output_tokens > 0 {
                            let _ = tx
                                .send(StreamChunk::Usage {
                                    input_tokens,
                                    output_tokens,
                                })
                                .await;
                            input_tokens = 0;
                        }
                    }
                    _ => {}
                }
            }
        }

        if !flush_anthropic_tool_call(&tx, &mut tool_id, &mut tool_name, &mut tool_input).await {
            return;
        }
        let _ = tx.send(StreamChunk::Done).await;
    });

    rx
}

pub fn map_provider_error(status: reqwest::StatusCode, body_text: String) -> ProviderError {
    match status.as_u16() {
        401 | 403 => ProviderError::AuthError(body_text),
        404 => ProviderError::ModelNotFound(body_text),
        429 => ProviderError::RateLimited {
            retry_after_ms: 1000,
        },
        // 5xx ("Internal server error", 502/503/504, Anthropic's 529
        // "Overloaded") is the provider's own infrastructure failing, not a
        // bad request — almost always clears on retry.
        500..=599 => ProviderError::Transient(body_text),
        _ => ProviderError::RequestFailed(body_text),
    }
}

/// Emit the buffered tool call. Returns `false` (after sending a
/// `StreamChunk::Error`) when the accumulated input is not valid JSON, e.g.
/// cut off by `max_tokens` — a truncated tool call must never run or vanish.
async fn flush_anthropic_tool_call(
    tx: &tokio::sync::mpsc::Sender<StreamChunk>,
    tool_id: &mut String,
    tool_name: &mut String,
    tool_input: &mut String,
) -> bool {
    if tool_name.is_empty() {
        return true;
    }

    let raw = tool_input.trim();
    let parsed = if raw.is_empty() {
        Ok(json!({}))
    } else {
        serde_json::from_str::<Value>(raw)
    };
    let ok = match parsed {
        Ok(input) => {
            let _ = tx
                .send(StreamChunk::ToolUse(ToolCall {
                    id: tool_id.clone(),
                    name: tool_name.clone(),
                    input,
                }))
                .await;
            true
        }
        Err(err) => {
            let _ = tx
                .send(StreamChunk::Error(format!(
                    "tool call `{tool_name}` arrived with invalid JSON input ({err}); the response was likely truncated"
                )))
                .await;
            false
        }
    };

    tool_id.clear();
    tool_name.clear();
    tool_input.clear();
    ok
}

fn tool_content_string(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(t) => t.clone(),
        MessageContent::Parts(_) => content.to_summary_text(),
    }
}

fn user_content_value(
    content: &MessageContent,
    workspace_root: &Path,
) -> Result<Value, ProviderError> {
    match content {
        MessageContent::Text(s) => Ok(json!(s)),
        MessageContent::Parts(parts) => {
            let mut blocks = Vec::new();
            for p in parts {
                match p {
                    ContentPart::Text { text } => {
                        blocks.push(json!({
                            "type": "text",
                            "text": text,
                        }));
                    }
                    ContentPart::Image { media_type, path } => {
                        let full = workspace_root.join(path);
                        let bytes = std::fs::read(&full).map_err(|e| {
                            ProviderError::RequestFailed(format!(
                                "failed to read image {}: {e}",
                                full.display()
                            ))
                        })?;
                        let data = B64.encode(bytes);
                        blocks.push(json!({
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": media_type,
                                "data": data,
                            }
                        }));
                    }
                }
            }
            Ok(Value::Array(blocks))
        }
    }
}

fn is_anthropic_replay_block(block: &Value) -> bool {
    matches!(
        block["type"].as_str(),
        Some("thinking") | Some("redacted_thinking")
    )
}

fn to_anthropic_messages(
    messages: &[Message],
    workspace_root: &Path,
) -> Result<(Option<String>, Vec<Value>), ProviderError> {
    let mut system_parts = Vec::new();
    let mut out = Vec::new();
    let mut i = 0;

    while i < messages.len() && messages[i].role == Role::System {
        match &messages[i].content {
            MessageContent::Text(t) => system_parts.push(t.clone()),
            MessageContent::Parts(_) => system_parts.push(messages[i].content.to_summary_text()),
        }
        i += 1;
    }

    while i < messages.len() {
        let message = &messages[i];
        match message.role {
            Role::User => {
                let content = user_content_value(&message.content, workspace_root)?;
                out.push(json!({
                    "role": "user",
                    "content": content,
                }));
                i += 1;
            }
            Role::Assistant => {
                let mut blocks = Vec::new();
                // Thinking blocks must lead the assistant turn, unchanged.
                if let Some(replay) = &message.provider_blocks {
                    blocks.extend(
                        replay
                            .iter()
                            .filter(|b| is_anthropic_replay_block(b))
                            .cloned(),
                    );
                }
                if let MessageContent::Text(t) = &message.content {
                    if !t.is_empty() {
                        blocks.push(json!({
                            "type": "text",
                            "text": t,
                        }));
                    }
                } else {
                    let v = user_content_value(&message.content, workspace_root)?;
                    if let Value::Array(arr) = v {
                        blocks.extend(arr);
                    }
                }
                if let Some(calls) = &message.tool_calls {
                    for call in calls {
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": call.id,
                            "name": call.name,
                            "input": call.arguments,
                        }));
                    }
                }

                let content_out = if blocks.is_empty() {
                    match &message.content {
                        MessageContent::Text(t) => json!(t),
                        MessageContent::Parts(_) => {
                            user_content_value(&message.content, workspace_root)?
                        }
                    }
                } else {
                    Value::Array(blocks)
                };

                out.push(json!({
                    "role": "assistant",
                    "content": content_out,
                }));
                i += 1;
            }
            Role::Tool => {
                let mut results = Vec::new();
                while i < messages.len() && messages[i].role == Role::Tool {
                    let tool_message = &messages[i];
                    results.push(json!({
                        "type": "tool_result",
                        "tool_use_id": tool_message.tool_call_id.as_deref().unwrap_or(""),
                        "content": tool_content_string(&tool_message.content),
                    }));
                    i += 1;
                }
                out.push(json!({
                    "role": "user",
                    "content": results,
                }));
            }
            Role::System => {
                i += 1;
            }
        }
    }

    let system = if system_parts.is_empty() {
        None
    } else {
        Some(system_parts.join("\n\n"))
    };

    Ok((system, out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD as B64};
    use dcode_ai_common::message::{ContentPart, Message};
    use tempfile::tempdir;

    #[test]
    fn user_multimodal_message_serializes_image_base64_block() {
        let dir = tempdir().unwrap();
        let workspace = dir.path();
        let rel = ".dcode-ai/sessions/s1/attachments/x.png";
        let att_dir = workspace.join(".dcode-ai/sessions/s1/attachments");
        std::fs::create_dir_all(&att_dir).unwrap();
        let png = B64
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==")
            .unwrap();
        std::fs::write(att_dir.join("x.png"), png).unwrap();

        let messages = vec![Message::user_with_parts(vec![
            ContentPart::Text {
                text: "describe".into(),
            },
            ContentPart::Image {
                media_type: "image/png".into(),
                path: rel.into(),
            },
        ])];

        let body = anthropic_request_body(
            &messages,
            &[],
            "claude-3-7-sonnet-latest",
            128,
            1.0,
            workspace,
        )
        .expect("body");
        let content = body["messages"][0]["content"].as_array().expect("array");
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "base64");
        assert_eq!(content[1]["source"]["media_type"], "image/png");
        assert!(content[1]["source"]["data"].as_str().unwrap().len() > 8);
    }

    #[test]
    fn request_body_marks_cache_breakpoints_on_system_and_last_tool() {
        let messages = vec![Message::system("be terse"), Message::user("hi")];
        let tools = vec![
            ToolDefinition {
                name: "a".into(),
                description: "first".into(),
                parameters: json!({"type": "object"}),
            },
            ToolDefinition {
                name: "b".into(),
                description: "second".into(),
                parameters: json!({"type": "object"}),
            },
        ];

        let body = anthropic_request_body(
            &messages,
            &tools,
            "claude-sonnet-4-6",
            256,
            1.0,
            std::path::Path::new("."),
        )
        .expect("body");

        // System prompt is a cacheable text block.
        assert_eq!(body["system"][0]["type"], "text");
        assert_eq!(body["system"][0]["text"], "be terse");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");

        // Only the final tool carries the breakpoint (caches the whole tools block).
        let tools_out = body["tools"].as_array().expect("tools array");
        assert!(tools_out[0].get("cache_control").is_none());
        assert_eq!(tools_out[1]["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn request_body_caches_final_message_block() {
        let messages = vec![Message::system("sys"), Message::user("latest question")];
        let body = anthropic_request_body(
            &messages,
            &[],
            "claude-sonnet-4-6",
            256,
            1.0,
            std::path::Path::new("."),
        )
        .expect("body");

        // The final user message's text was promoted to a cacheable block.
        let last = body["messages"].as_array().unwrap().last().unwrap();
        let block = &last["content"][0];
        assert_eq!(block["type"], "text");
        assert_eq!(block["text"], "latest question");
        assert_eq!(block["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn request_body_omits_system_when_absent() {
        let body = anthropic_request_body(
            &[Message::user("hi")],
            &[],
            "claude-sonnet-4-6",
            256,
            1.0,
            std::path::Path::new("."),
        )
        .expect("body");
        assert!(body.get("system").is_none());
    }

    #[test]
    fn tool_less_requests_omit_tools() {
        let body = body_for("claude-sonnet-5", &opts(false, None));
        assert!(body.get("tools").is_none(), "{body}");
    }

    fn opts(thinking: bool, effort: Option<&str>) -> AnthropicRequestOptions {
        AnthropicRequestOptions {
            thinking,
            thinking_budget: 4096,
            effort: effort.map(str::to_string),
        }
    }

    fn body_for(model: &str, options: &AnthropicRequestOptions) -> Value {
        anthropic_request_body_with(
            &[Message::user("hi")],
            &[],
            model,
            8192,
            0.7,
            Path::new("."),
            options,
        )
        .expect("body")
    }

    #[test]
    fn dotted_claude_ids_are_hyphenated() {
        assert_eq!(
            normalize_claude_model_id("claude-sonnet-4.6"),
            "claude-sonnet-4-6"
        );
        assert_eq!(
            normalize_claude_model_id("claude-opus-5-5"),
            "claude-opus-5-5"
        );
        assert_eq!(normalize_claude_model_id("gpt-4.1"), "gpt-4.1");
        assert_eq!(
            ClaudeFamily::for_model("claude-sonnet-4.6"),
            ClaudeFamily::AdaptiveWithSampling
        );
    }

    #[test]
    fn claude_family_detection() {
        for m in [
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
            "claude-fable-5-1",
            "claude-opus-4-8",
            "claude-opus-4-7",
        ] {
            assert_eq!(
                ClaudeFamily::for_model(m),
                ClaudeFamily::AdaptiveNoSampling,
                "{m}"
            );
        }
        assert_eq!(
            ClaudeFamily::for_model("claude-sonnet-4-6"),
            ClaudeFamily::AdaptiveWithSampling
        );
        assert_eq!(
            ClaudeFamily::for_model("claude-haiku-4-5"),
            ClaudeFamily::Legacy
        );
    }

    #[test]
    fn current_models_get_adaptive_thinking_effort_and_no_sampling() {
        let body = body_for("claude-opus-5-5", &opts(true, Some("high")));
        assert!(
            body.get("temperature").is_none(),
            "sampling params 400 on 5.x"
        );
        assert_eq!(
            body["thinking"],
            json!({"type": "adaptive", "display": "summarized"})
        );
        assert_eq!(body["output_config"]["effort"], "high");

        let body = body_for("claude-opus-5-5", &opts(false, None));
        assert!(body.get("thinking").is_none());
        assert!(body.get("temperature").is_none());
        assert!(body.get("output_config").is_none());
    }

    #[test]
    fn sonnet_4_6_keeps_temperature_only_without_thinking() {
        let body = body_for("claude-sonnet-4-6", &opts(false, None));
        assert!(body.get("temperature").is_some());
        let body = body_for("claude-sonnet-4-6", &opts(true, None));
        assert!(body.get("temperature").is_none());
        assert_eq!(body["thinking"]["type"], "adaptive");
    }

    #[test]
    fn legacy_models_use_budgeted_thinking() {
        let body = body_for("claude-haiku-4-5", &opts(true, None));
        assert_eq!(
            body["thinking"],
            json!({"type": "enabled", "budget_tokens": 4096})
        );
        assert!(body.get("temperature").is_none());
        assert_eq!(body["max_tokens"], 8192);

        let body = body_for("claude-haiku-4-5", &opts(false, None));
        assert!(body.get("thinking").is_none());
        assert!(body.get("temperature").is_some());
    }

    #[test]
    fn assistant_replay_blocks_lead_the_turn_and_foreign_blocks_are_dropped() {
        let assistant = Message::assistant("answer").with_provider_blocks(vec![
            json!({"type": "thinking", "thinking": "t", "signature": "sig"}),
            json!({"type": "reasoning", "encrypted_content": "openai-only"}),
        ]);
        let body = anthropic_request_body_with(
            &[Message::user("q"), assistant, Message::user("next")],
            &[],
            "claude-opus-5-5",
            8192,
            1.0,
            Path::new("."),
            &AnthropicRequestOptions::default(),
        )
        .expect("body");
        let content = body["messages"][1]["content"].as_array().expect("blocks");
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "thinking");
        assert_eq!(content[0]["signature"], "sig");
        assert_eq!(content[1]["type"], "text");
    }
}
