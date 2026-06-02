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

const THINK_OPEN: &str = "<think>";
const THINK_CLOSE: &str = "</think>";

/// Splits a streamed `content` field into reasoning vs visible-text segments by
/// tracking `<think>…</think>` tags across chunk boundaries. Some providers
/// (e.g. DeepSeek via aggregators) inline reasoning as `<think>` tags in the
/// content instead of a separate `reasoning_content` field; without this the
/// tags leak into the assistant reply.
#[derive(Default)]
struct ThinkSplitter {
    in_think: bool,
    carry: String,
}

impl ThinkSplitter {
    /// Returns ordered `(is_reasoning, text)` segments for one content delta.
    fn feed(&mut self, input: &str) -> Vec<(bool, String)> {
        let mut work = std::mem::take(&mut self.carry);
        work.push_str(input);
        let mut out: Vec<(bool, String)> = Vec::new();
        loop {
            let marker = if self.in_think {
                THINK_CLOSE
            } else {
                THINK_OPEN
            };
            if let Some(pos) = work.find(marker) {
                let before = work[..pos].to_string();
                if !before.is_empty() {
                    out.push((self.in_think, before));
                }
                self.in_think = !self.in_think;
                work = work[pos + marker.len()..].to_string();
            } else {
                // Hold back a tail that could be the start of a split marker.
                let keep = partial_marker_tail_len(&work, marker);
                let split = work.len() - keep;
                if split > 0 {
                    out.push((self.in_think, work[..split].to_string()));
                }
                self.carry = work[split..].to_string();
                break;
            }
        }
        out
    }

    /// Drain any held-back tail (an incomplete marker that never completed) as
    /// visible text, for the end of the stream.
    fn flush(&mut self) -> Option<String> {
        if self.carry.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.carry))
        }
    }
}

/// Largest `k` such that the last `k` bytes of `s` equal the first `k` bytes of
/// `marker` (a possible split-marker prefix). Markers are ASCII, so the split
/// always lands on a char boundary.
fn partial_marker_tail_len(s: &str, marker: &str) -> usize {
    let max = marker.len().saturating_sub(1).min(s.len());
    (1..=max)
        .rev()
        .find(|&k| s.as_bytes()[s.len() - k..] == marker.as_bytes()[..k])
        .unwrap_or(0)
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
        "messages": to_openai_messages(messages, workspace_root, model_supports_vision(model))?,
        "tools": tools,
        "stream": true,
        "stream_options": {
            "include_usage": true
        },
        "max_tokens": max_tokens,
        "temperature": temperature,
    }))
}

#[allow(clippy::too_many_arguments)]
pub fn openai_responses_request_body(
    messages: &[Message],
    tools: &[ToolDefinition],
    model: &str,
    max_tokens: u32,
    temperature: f32,
    stream: bool,
    codex_chatgpt_backend: bool,
    workspace_root: &Path,
) -> Result<Value, ProviderError> {
    let mut instructions = Vec::new();
    let mut input_items = Vec::new();

    for message in messages {
        match message.role {
            Role::System => instructions.push(tool_content_string(&message.content)),
            _ => input_items.extend(message_to_responses_input_items(message, workspace_root)?),
        }
    }

    let mut body = json!({
        "model": model,
        "input": input_items,
        "stream": stream,
        "store": false,
    });
    if !codex_chatgpt_backend {
        body["max_output_tokens"] = json!(max_tokens);
        body["temperature"] = json!(temperature);
    }

    if !instructions.is_empty() {
        body["instructions"] = json!(instructions.join("\n\n"));
    }

    if !tools.is_empty() {
        body["tools"] = Value::Array(
            tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    })
                })
                .collect(),
        );
        body["tool_choice"] = json!("auto");
    }

    Ok(body)
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
        let mut think = ThinkSplitter::default();

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
                    if let Some(tail) = think.flush() {
                        let _ = tx.send(StreamChunk::TextDelta(tail)).await;
                    }
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
                        for (is_reasoning, seg) in think.feed(text) {
                            let chunk = if is_reasoning {
                                StreamChunk::InternalDelta(seg)
                            } else {
                                StreamChunk::TextDelta(seg)
                            };
                            let _ = tx.send(chunk).await;
                        }
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

        if let Some(tail) = think.flush() {
            let _ = tx.send(StreamChunk::TextDelta(tail)).await;
        }
        flush_openai_tool_calls(&tx, &mut tool_calls).await;
        let _ = tx.send(StreamChunk::Done).await;
    });

    rx
}

pub fn spawn_openai_responses_stream(
    response: reqwest::Response,
    provider_name: &'static str,
    streaming: bool,
) -> tokio::sync::mpsc::Receiver<StreamChunk> {
    let (tx, rx) = tokio::sync::mpsc::channel(64);

    tokio::spawn(async move {
        if streaming {
            let mut byte_stream = response.bytes_stream();
            let mut buffer = String::new();
            let mut current_event = String::new();
            let mut tool_calls: BTreeMap<String, ToolCallAccumulator> = BTreeMap::new();

            while let Some(item) = byte_stream.next().await {
                let chunk = match item {
                    Ok(chunk) => chunk,
                    Err(err) => {
                        let _ = tx
                            .send(StreamChunk::Error(format!(
                                "{provider_name} stream error: {err}"
                            )))
                            .await;
                        let _ = tx.send(StreamChunk::Done).await;
                        return;
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
                    if let Some(ev) = line.strip_prefix("event:") {
                        current_event = ev.trim().to_string();
                        continue;
                    }
                    if !line.starts_with("data:") {
                        continue;
                    }
                    let data = line["data:".len()..].trim();
                    if data == "[DONE]" {
                        flush_responses_tool_calls(&tx, &mut tool_calls).await;
                        let _ = tx.send(StreamChunk::Done).await;
                        return;
                    }

                    let Ok(event) = serde_json::from_str::<Value>(data) else {
                        continue;
                    };
                    let event_type = if current_event.is_empty() {
                        event
                            .get("type")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string()
                    } else {
                        current_event.clone()
                    };
                    match event_type.as_str() {
                        "response.output_text.delta" => {
                            if let Some(delta) = event.get("delta").and_then(|v| v.as_str())
                                && !delta.is_empty()
                            {
                                let _ = tx.send(StreamChunk::TextDelta(delta.to_string())).await;
                            }
                        }
                        "response.output_text.done" => {
                            if let Some(text) = event.get("text").and_then(|v| v.as_str())
                                && !text.is_empty()
                            {
                                let _ = tx.send(StreamChunk::TextDelta(text.to_string())).await;
                            }
                        }
                        "response.reasoning_text.delta"
                        | "response.reasoning_summary_text.delta" => {
                            if let Some(delta) = event.get("delta").and_then(|v| v.as_str())
                                && !delta.is_empty()
                            {
                                let _ =
                                    tx.send(StreamChunk::InternalDelta(delta.to_string())).await;
                            }
                        }
                        "response.output_item.added" => {
                            let item = event.get("item").unwrap_or(&event);
                            if item.get("type").and_then(|v| v.as_str()) == Some("function_call") {
                                let item_id = item
                                    .get("id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                if item_id.is_empty() {
                                    continue;
                                }
                                let call_id = item
                                    .get("call_id")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                let name = item
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("")
                                    .to_string();
                                tool_calls.entry(item_id).or_insert(ToolCallAccumulator {
                                    id: call_id,
                                    name,
                                    arguments: String::new(),
                                });
                            }
                        }
                        "response.function_call_arguments.delta" => {
                            if let Some(item_id) = event.get("item_id").and_then(|v| v.as_str())
                                && let Some(acc) = tool_calls.get_mut(item_id)
                                && let Some(delta) = event.get("delta").and_then(|v| v.as_str())
                            {
                                acc.arguments.push_str(delta);
                            }
                        }
                        "response.function_call_arguments.done" => {
                            if let Some(item_id) = event.get("item_id").and_then(|v| v.as_str())
                                && let Some(acc) = tool_calls.get_mut(item_id)
                            {
                                if let Some(args) = event.get("arguments").and_then(|v| v.as_str())
                                {
                                    acc.arguments = args.to_string();
                                }
                                if acc.name.is_empty()
                                    && let Some(name) = event.get("name").and_then(|v| v.as_str())
                                {
                                    acc.name = name.to_string();
                                }
                            }
                        }
                        "response.completed" => {
                            if let Some(resp) = event.get("response")
                                && let Some(usage) = resp.get("usage")
                            {
                                let input_tokens = usage["input_tokens"].as_u64().unwrap_or(0);
                                let output_tokens = usage["output_tokens"].as_u64().unwrap_or(0);
                                if input_tokens > 0 || output_tokens > 0 {
                                    let _ = tx
                                        .send(StreamChunk::Usage {
                                            input_tokens,
                                            output_tokens,
                                        })
                                        .await;
                                }
                            }
                            flush_responses_tool_calls(&tx, &mut tool_calls).await;
                            let _ = tx.send(StreamChunk::Done).await;
                            return;
                        }
                        "error" => {
                            let msg = event
                                .get("message")
                                .and_then(|v| v.as_str())
                                .unwrap_or("responses stream error");
                            let _ = tx.send(StreamChunk::Error(msg.to_string())).await;
                            let _ = tx.send(StreamChunk::Done).await;
                            return;
                        }
                        _ => {}
                    }
                }
            }
            flush_responses_tool_calls(&tx, &mut tool_calls).await;
            let _ = tx.send(StreamChunk::Done).await;
            return;
        }

        let event = match response.json::<Value>().await {
            Ok(v) => v,
            Err(err) => {
                let _ = tx
                    .send(StreamChunk::Error(format!(
                        "{provider_name} response parse error: {err}"
                    )))
                    .await;
                let _ = tx.send(StreamChunk::Done).await;
                return;
            }
        };

        if let Some(text) = responses_output_text(&event)
            && !text.is_empty()
        {
            let _ = tx.send(StreamChunk::TextDelta(text)).await;
        }

        if let Some(reasoning) = responses_reasoning_text(&event)
            && !reasoning.is_empty()
        {
            let _ = tx.send(StreamChunk::InternalDelta(reasoning)).await;
        }

        for call in responses_tool_calls(&event) {
            let _ = tx.send(StreamChunk::ToolUse(call)).await;
        }

        let input_tokens = event
            .pointer("/usage/input_tokens")
            .and_then(|v| v.as_u64())
            .or_else(|| {
                event
                    .pointer("/usage/prompt_tokens")
                    .and_then(|v| v.as_u64())
            })
            .unwrap_or(0);
        let output_tokens = event
            .pointer("/usage/output_tokens")
            .and_then(|v| v.as_u64())
            .or_else(|| {
                event
                    .pointer("/usage/completion_tokens")
                    .and_then(|v| v.as_u64())
            })
            .unwrap_or(0);
        if input_tokens > 0 || output_tokens > 0 {
            let _ = tx
                .send(StreamChunk::Usage {
                    input_tokens,
                    output_tokens,
                })
                .await;
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
        _ => ProviderError::RequestFailed(body_text),
    }
}

fn responses_output_text(event: &Value) -> Option<String> {
    if let Some(top) = event.get("output_text").and_then(|v| v.as_str())
        && !top.is_empty()
    {
        return Some(top.to_string());
    }

    let mut out = String::new();
    let items = event.get("output").and_then(|v| v.as_array())?;
    for item in items {
        if item.get("type").and_then(|v| v.as_str()) != Some("message") {
            continue;
        }
        let Some(content) = item.get("content").and_then(|v| v.as_array()) else {
            continue;
        };
        for block in content {
            let typ = block
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            if (typ == "output_text" || typ == "text")
                && let Some(text) = block.get("text").and_then(|v| v.as_str())
            {
                out.push_str(text);
            }
        }
    }

    if out.is_empty() { None } else { Some(out) }
}

fn responses_reasoning_text(event: &Value) -> Option<String> {
    let mut out = String::new();
    let items = event.get("output").and_then(|v| v.as_array())?;
    for item in items {
        if item.get("type").and_then(|v| v.as_str()) != Some("reasoning") {
            continue;
        }
        if let Some(summary) = item.get("summary").and_then(|v| v.as_array()) {
            for block in summary {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    out.push_str(text);
                }
            }
        }
        if let Some(content) = item.get("content").and_then(|v| v.as_array()) {
            for block in content {
                if let Some(text) = block.get("text").and_then(|v| v.as_str()) {
                    out.push_str(text);
                }
            }
        }
    }
    if out.is_empty() { None } else { Some(out) }
}

fn responses_tool_calls(event: &Value) -> Vec<ToolCall> {
    let mut out = Vec::new();
    let Some(items) = event.get("output").and_then(|v| v.as_array()) else {
        return out;
    };

    for (idx, item) in items.iter().enumerate() {
        if item.get("type").and_then(|v| v.as_str()) != Some("function_call") {
            continue;
        }
        let Some(name) = item.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let call_id = item
            .get("call_id")
            .and_then(|v| v.as_str())
            .or_else(|| item.get("id").and_then(|v| v.as_str()))
            .map(str::to_string)
            .unwrap_or_else(|| format!("tool-call-{idx}"));

        let input = if let Some(args_str) = item.get("arguments").and_then(|v| v.as_str()) {
            serde_json::from_str(args_str).unwrap_or_else(|_| json!({ "raw": args_str }))
        } else if let Some(args_obj) = item.get("arguments") {
            args_obj.clone()
        } else {
            json!({})
        };

        out.push(ToolCall {
            id: call_id,
            name: name.to_string(),
            input,
        });
    }

    out
}

async fn flush_responses_tool_calls(
    tx: &tokio::sync::mpsc::Sender<StreamChunk>,
    tool_calls: &mut BTreeMap<String, ToolCallAccumulator>,
) {
    let drained = std::mem::take(tool_calls);
    for (index, call) in drained {
        if call.name.is_empty() {
            continue;
        }
        let input = if !call.arguments.is_empty() {
            serde_json::from_str(&call.arguments)
                .unwrap_or_else(|_| json!({ "raw": call.arguments }))
        } else {
            json!({})
        };
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

#[derive(Default)]
struct ToolCallAccumulator {
    id: String,
    name: String,
    arguments: String,
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

/// Whether a model accepts image content parts. Defaults to true; known
/// text-only models (the DeepSeek family) are excluded so we don't send
/// `image_url` blocks they reject with a deserialize error.
fn model_supports_vision(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    !m.contains("deepseek")
}

fn openai_user_content_value(
    content: &MessageContent,
    workspace_root: &Path,
    supports_vision: bool,
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
                    ContentPart::Image { .. } if !supports_vision => {
                        blocks.push(json!({
                            "type": "text",
                            "text": "[image attached, but the current model does not support image input]",
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

fn responses_user_content(
    content: &MessageContent,
    workspace_root: &Path,
) -> Result<Vec<Value>, ProviderError> {
    match content {
        MessageContent::Text(s) => Ok(vec![json!({
            "type": "input_text",
            "text": s,
        })]),
        MessageContent::Parts(parts) => {
            let mut out = Vec::new();
            for p in parts {
                match p {
                    ContentPart::Text { text } => out.push(json!({
                        "type": "input_text",
                        "text": text,
                    })),
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
                        out.push(json!({
                            "type": "input_image",
                            "image_url": url,
                        }));
                    }
                }
            }
            Ok(out)
        }
    }
}

fn message_to_responses_input_items(
    message: &Message,
    workspace_root: &Path,
) -> Result<Vec<Value>, ProviderError> {
    let mut items = Vec::new();

    match message.role {
        Role::User => {
            items.push(json!({
                "type": "message",
                "role": "user",
                "content": responses_user_content(&message.content, workspace_root)?,
            }));
        }
        Role::Assistant => {
            if let Some(calls) = &message.tool_calls {
                if !message.content.is_empty() {
                    items.push(json!({
                        "type": "message",
                        "role": "assistant",
                        "content": [{
                            "type": "output_text",
                            "text": tool_content_string(&message.content),
                        }],
                    }));
                }
                for call in calls {
                    items.push(json!({
                        "type": "function_call",
                        "call_id": call.id,
                        "name": call.name,
                        "arguments": serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".into()),
                    }));
                }
            } else {
                items.push(json!({
                    "type": "message",
                    "role": "assistant",
                    "content": [{
                        "type": "output_text",
                        "text": tool_content_string(&message.content),
                    }],
                }));
            }
        }
        Role::Tool => {
            items.push(json!({
                "type": "function_call_output",
                "call_id": message.tool_call_id.clone().unwrap_or_default(),
                "output": tool_content_string(&message.content),
            }));
        }
        Role::System => {}
    }

    Ok(items)
}

fn to_openai_messages(
    messages: &[Message],
    workspace_root: &Path,
    supports_vision: bool,
) -> Result<Vec<Value>, ProviderError> {
    let mut out = Vec::new();

    for message in messages {
        match message.role {
            Role::System => out.push(json!({
                "role": "system",
                "content": tool_content_string(&message.content),
            })),
            Role::User => {
                let c =
                    openai_user_content_value(&message.content, workspace_root, supports_vision)?;
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
                        openai_user_content_value(&message.content, workspace_root, supports_vision)?
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

#[cfg(test)]
mod tests {
    use super::*;
    use dcode_ai_common::message::MessageToolCall;
    use serde_json::json;

    fn split_all(chunks: &[&str]) -> (String, String) {
        let mut s = ThinkSplitter::default();
        let (mut reasoning, mut text) = (String::new(), String::new());
        for c in chunks {
            for (is_reasoning, seg) in s.feed(c) {
                if is_reasoning {
                    reasoning.push_str(&seg);
                } else {
                    text.push_str(&seg);
                }
            }
        }
        if let Some(tail) = s.flush() {
            text.push_str(&tail);
        }
        (reasoning, text)
    }

    #[test]
    fn think_splitter_routes_inline_tags_to_reasoning() {
        let (r, t) = split_all(&["<think>planning</think>hello world"]);
        assert_eq!(r, "planning");
        assert_eq!(t, "hello world");
    }

    #[test]
    fn think_splitter_handles_tags_split_across_chunks() {
        // Markers arrive split across delta boundaries.
        let (r, t) = split_all(&["<thi", "nk>deep ", "thought</thi", "nk>visible"]);
        assert_eq!(r, "deep thought");
        assert_eq!(t, "visible");
    }

    #[test]
    fn think_splitter_passes_plain_text_through() {
        let (r, t) = split_all(&["just ", "text"]);
        assert!(r.is_empty());
        assert_eq!(t, "just text");
    }

    #[test]
    fn model_vision_excludes_deepseek() {
        assert!(model_supports_vision("gpt-4o"));
        assert!(model_supports_vision("claude-opus-4"));
        assert!(!model_supports_vision("deepseek-chat"));
        assert!(!model_supports_vision("DeepSeek-R1"));
    }

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

    #[test]
    fn responses_request_body_maps_tools_and_history() {
        let body = openai_responses_request_body(
            &[
                Message::system("sys"),
                Message::user("hello"),
                Message::assistant_with_tool_calls(
                    "",
                    vec![MessageToolCall {
                        id: "call_1".into(),
                        name: "read_file".into(),
                        arguments: json!({"path":"Cargo.toml"}),
                    }],
                ),
                Message::tool("call_1", "ok"),
            ],
            &[ToolDefinition {
                name: "read_file".into(),
                description: "Read file".into(),
                parameters: json!({"type":"object"}),
            }],
            "gpt-5-codex",
            1024,
            0.1,
            true,
            false,
            std::path::Path::new("."),
        )
        .expect("responses body");

        assert_eq!(body["model"], "gpt-5-codex");
        assert_eq!(body["instructions"], "sys");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["name"], "read_file");
        let input = body["input"].as_array().expect("input array");
        assert!(input.iter().any(|v| v["type"] == "function_call"));
        assert!(input.iter().any(|v| v["type"] == "function_call_output"));
    }

    #[test]
    fn responses_parser_extracts_text_tool_and_usage() {
        let event = json!({
            "output": [
                {
                    "type": "message",
                    "content": [
                        {"type":"output_text", "text":"Hello "}
                    ]
                },
                {
                    "type":"function_call",
                    "id":"fc_1",
                    "call_id":"call_1",
                    "name":"lookup",
                    "arguments":"{\"path\":\"src\"}"
                }
            ],
            "usage": {
                "input_tokens": 12,
                "output_tokens": 8
            }
        });

        assert_eq!(responses_output_text(&event), Some("Hello ".into()));
        let calls = responses_tool_calls(&event);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_1");
        assert_eq!(calls[0].name, "lookup");
        assert_eq!(calls[0].input, json!({"path":"src"}));
    }

    #[test]
    fn responses_body_omits_unsupported_fields_for_codex_chatgpt_backend() {
        let body = openai_responses_request_body(
            &[Message::user("hello")],
            &[],
            "gpt-5-codex",
            2048,
            0.2,
            true,
            true,
            std::path::Path::new("."),
        )
        .expect("responses body");

        assert_eq!(body["stream"], json!(true));
        assert!(body.get("max_output_tokens").is_none());
        assert!(body.get("temperature").is_none());
    }
}
