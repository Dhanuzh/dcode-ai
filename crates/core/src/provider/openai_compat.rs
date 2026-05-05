use std::path::Path;

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use dcode_ai_common::message::{ContentPart, Message, MessageContent, Role};
use dcode_ai_common::tool::{ToolCall, ToolDefinition};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::collections::BTreeMap;

use super::{ProviderError, StreamChunk};

fn extract_reasoning_text(value: &Value) -> Option<String> {
    if let Some(text) = value.as_str()
        && !text.is_empty()
    {
        return Some(text.to_string());
    }

    if let Some(obj) = value.as_object() {
        for key in [
            "text",
            "content",
            "reasoning",
            "reasoning_content",
            "thinking",
        ] {
            if let Some(v) = obj.get(key)
                && let Some(found) = extract_reasoning_text(v)
            {
                return Some(found);
            }
        }
    }

    if let Some(items) = value.as_array() {
        let merged = items
            .iter()
            .filter_map(extract_reasoning_text)
            .collect::<String>();
        if !merged.is_empty() {
            return Some(merged);
        }
    }

    None
}

fn extract_internal_reasoning_delta(delta: &Value) -> Option<String> {
    for key in [
        "reasoning_content",
        "reasoning",
        "thinking",
        "thinking_content",
        "reasoning_text",
        "reasoning_delta",
    ] {
        let Some(value) = delta.get(key) else {
            continue;
        };
        if let Some(text) = extract_reasoning_text(value) {
            return Some(text);
        }
    }
    None
}

pub fn openai_request_body(
    messages: &[Message],
    tools: &[ToolDefinition],
    model: &str,
    max_tokens: u32,
    temperature: f32,
    workspace_root: &Path,
) -> Result<Value, ProviderError> {
    let tools = if tools.is_empty() {
        None
    } else {
        Some(
            tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": tool.name,
                            "description": tool.description,
                            "parameters": tool.parameters,
                        }
                    })
                })
                .collect::<Vec<_>>(),
        )
    };

    Ok(json!({
        "model": model,
        "messages": to_openai_messages(messages, workspace_root)?,
        "tools": tools,
        "stream": true,
        "stream_options": {
            "include_usage": true
        },
        "max_tokens": max_tokens,
        "temperature": temperature,
    }))
}

pub fn spawn_openai_stream(
    response: reqwest::Response,
    provider_name: &'static str,
) -> tokio::sync::mpsc::Receiver<StreamChunk> {
    let mut byte_stream = response.bytes_stream();
    let (tx, rx) = tokio::sync::mpsc::channel(64);

    tokio::spawn(async move {
        let mut buffer = String::new();
        let mut tool_calls: BTreeMap<u64, ToolCallAccumulator> = BTreeMap::new();

        while let Some(item) = byte_stream.next().await {
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
                    flush_openai_tool_calls(&tx, &mut tool_calls).await;
                    let _ = tx.send(StreamChunk::Done).await;
                    return;
                }

                let Ok(event) = serde_json::from_str::<Value>(data) else {
                    continue;
                };

                if let Some(usage) = event.get("usage") {
                    let input_tokens = usage["prompt_tokens"].as_u64().unwrap_or(0);
                    let output_tokens = usage["completion_tokens"].as_u64().unwrap_or(0);
                    if input_tokens > 0 || output_tokens > 0 {
                        let _ = tx
                            .send(StreamChunk::Usage {
                                input_tokens,
                                output_tokens,
                            })
                            .await;
                    }
                }

                let Some(choices) = event["choices"].as_array() else {
                    continue;
                };

                for choice in choices {
                    let delta = &choice["delta"];
                    let reasoning = extract_internal_reasoning_delta(delta)
                        .or_else(|| extract_internal_reasoning_delta(choice));
                    if let Some(text) = reasoning {
                        let _ = tx.send(StreamChunk::InternalDelta(text)).await;
                    }
                    if let Some(text) = delta["content"].as_str()
                        && !text.is_empty()
                    {
                        let _ = tx.send(StreamChunk::TextDelta(text.to_string())).await;
                    }

                    if let Some(tool_deltas) = delta["tool_calls"].as_array() {
                        for tool_delta in tool_deltas {
                            let index = tool_delta["index"].as_u64().unwrap_or(0);
                            let entry = tool_calls.entry(index).or_default();
                            if let Some(id) = tool_delta["id"].as_str() {
                                entry.id = id.to_string();
                            }
                            if let Some(name) = tool_delta["function"]["name"].as_str() {
                                entry.name.push_str(name);
                            }
                            if let Some(arguments) = tool_delta["function"]["arguments"].as_str() {
                                entry.arguments.push_str(arguments);
                            }
                        }
                    }

                    if choice["finish_reason"].as_str() == Some("tool_calls") {
                        flush_openai_tool_calls(&tx, &mut tool_calls).await;
                    }
                }
            }
        }

        flush_openai_tool_calls(&tx, &mut tool_calls).await;
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
        _ => ProviderError::RequestFailed(body_text),
    }
}

#[derive(Default)]
struct ToolCallAccumulator {
    id: String,
    name: String,
    arguments: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reasoning_delta_accepts_multiple_shapes() {
        assert_eq!(
            extract_internal_reasoning_delta(&json!({"reasoning_content":"r1"})),
            Some("r1".into())
        );
        assert_eq!(
            extract_internal_reasoning_delta(&json!({"reasoning":"r2"})),
            Some("r2".into())
        );
        assert_eq!(
            extract_internal_reasoning_delta(&json!({"thinking":{"text":"r3"}})),
            Some("r3".into())
        );
        assert_eq!(
            extract_internal_reasoning_delta(&json!({"reasoning":[{"text":"a"},{"text":"b"}]})),
            Some("ab".into())
        );
        assert_eq!(
            extract_internal_reasoning_delta(&json!({"reasoning":{"content":"r4"}})),
            Some("r4".into())
        );
        assert_eq!(
            extract_internal_reasoning_delta(&json!({"reasoning_delta":{"content":"r5"}})),
            Some("r5".into())
        );
        assert_eq!(
            extract_internal_reasoning_delta(&json!({"content":"assistant text only"})),
            None
        );
    }
}

async fn flush_openai_tool_calls(
    tx: &tokio::sync::mpsc::Sender<StreamChunk>,
    tool_calls: &mut BTreeMap<u64, ToolCallAccumulator>,
) {
    let drained = std::mem::take(tool_calls);
    for (index, call) in drained {
        if call.name.is_empty() {
            continue;
        }

        if let Ok(input) = serde_json::from_str(&call.arguments) {
            let _ = tx
                .send(StreamChunk::ToolUse(ToolCall {
                    id: if call.id.is_empty() {
                        format!("tool-call-{index}")
                    } else {
                        call.id
                    },
                    name: call.name,
                    input,
                }))
                .await;
        }
    }
}

fn tool_content_string(content: &MessageContent) -> String {
    match content {
        MessageContent::Text(t) => t.clone(),
        MessageContent::Parts(_) => content.to_summary_text(),
    }
}

fn openai_user_content_value(
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
                        let b64 = B64.encode(bytes);
                        let url = format!("data:{media_type};base64,{b64}");
                        blocks.push(json!({
                            "type": "image_url",
                            "image_url": { "url": url }
                        }));
                    }
                }
            }
            Ok(Value::Array(blocks))
        }
    }
}

fn to_openai_messages(
    messages: &[Message],
    workspace_root: &Path,
) -> Result<Vec<Value>, ProviderError> {
    let mut out = Vec::new();

    for message in messages {
        match message.role {
            Role::System => out.push(json!({
                "role": "system",
                "content": tool_content_string(&message.content),
            })),
            Role::User => {
                let c = openai_user_content_value(&message.content, workspace_root)?;
                out.push(json!({
                    "role": "user",
                    "content": c,
                }));
            }
            Role::Assistant => {
                let mut value = json!({
                    "role": "assistant",
                    "content": if message.content.is_empty() && message.tool_calls.is_some() {
                        Value::Null
                    } else {
                        openai_user_content_value(&message.content, workspace_root)?
                    },
                });

                if let Some(calls) = &message.tool_calls {
                    value["tool_calls"] = Value::Array(
                        calls
                            .iter()
                            .map(|call| {
                                json!({
                                    "id": call.id,
                                    "type": "function",
                                    "function": {
                                        "name": call.name,
                                        "arguments": serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".into()),
                                    }
                                })
                            })
                            .collect(),
                    );
                }
                if let Some(reasoning_content) = &message.reasoning_content {
                    value["reasoning_content"] = json!(reasoning_content);
                }

                out.push(value);
            }
            Role::Tool => out.push(json!({
                "role": "tool",
                "tool_call_id": message.tool_call_id,
                "content": tool_content_string(&message.content),
            })),
        }
    }

    Ok(out)
}
