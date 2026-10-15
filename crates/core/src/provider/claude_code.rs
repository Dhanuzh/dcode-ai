//! Claude through the user's own signed-in `claude` binary (Claude Code).
//!
//! The binary is used purely as a model endpoint: its built-in tools, MCP
//! servers, settings, skills and CLAUDE.md discovery are all switched off, and
//! its system prompt is replaced by dcode-ai's. dcode-ai's own tools are
//! described in that system prompt and invoked through a small text protocol
//! (`<tool_call>{json}</tool_call>`), so every tool call still goes through
//! dcode-ai's approval policy, sandbox, freshness guard and undo.
//!
//! One long-lived `claude -p --input-format stream-json` process serves a
//! conversation. Each `chat` sends only the messages Claude hasn't seen yet;
//! if history was rewritten (compaction, rewind, a different system prompt or
//! tool set), the process is restarted and the history is replayed as a
//! transcript.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use dcode_ai_common::config::{AnthropicConfig, DcodeAiConfig};
use dcode_ai_common::message::{Message, MessageContent, Role};
use dcode_ai_common::provider_runtime::{has_claude_cli, windows_compat_command_tokio};
use dcode_ai_common::tool::{ToolCall, ToolDefinition};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};

use super::anthropic_compat::{ClaudeFamily, normalize_claude_model_id};
use super::{Provider, ProviderCapabilities, ProviderError, StopReason, StreamChunk};

const TOOL_OPEN: &str = "<tool_call>";
const TOOL_CLOSE: &str = "</tool_call>";
/// Start of a tool result. Only dcode-ai writes these; if the model starts one
/// itself it is inventing results, so the turn is cut there.
const RESULT_OPEN: &str = "<tool_result";

/// Longest silence tolerated between two stdout lines. Thinking on hard tasks
/// can be quiet for a while, so this is generous.
const LINE_TIMEOUT: Duration = Duration::from_secs(600);

pub struct ClaudeCodeProvider {
    config: AnthropicConfig,
    session: Arc<tokio::sync::Mutex<Option<CcSession>>>,
}

struct CcSession {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    stderr_tail: Arc<std::sync::Mutex<String>>,
    /// Model + system prompt (incl. tool schemas) this process was started with.
    key: u64,
    /// Hashes of the conversation messages Claude already has in context.
    sent: Vec<u64>,
    /// Keeps the system-prompt file alive for the process lifetime.
    _prompt_file: tempfile::NamedTempFile,
}

impl ClaudeCodeProvider {
    pub fn from_config(config: &DcodeAiConfig) -> Result<Self, ProviderError> {
        if !has_claude_cli() {
            return Err(ProviderError::Configuration(
                "Claude account sign-in needs Claude Code (`claude`) installed: https://claude.com/claude-code — then run `dcode-ai login anthropic`".to_string(),
            ));
        }
        Ok(Self {
            config: config.provider.anthropic.clone(),
            session: Arc::new(tokio::sync::Mutex::new(None)),
        })
    }
}

#[async_trait::async_trait]
impl Provider for ClaudeCodeProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            supports_thinking_stream: ClaudeFamily::for_model(&self.config.model).is_adaptive(),
            // Input is sent as text; image parts are referenced by path only.
            supports_native_images: false,
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
        let model = normalize_claude_model_id(if model.is_empty() {
            &self.config.model
        } else {
            model
        });

        let system_len = messages
            .iter()
            .take_while(|m| m.role == Role::System)
            .count();
        let base_system = messages[..system_len]
            .iter()
            .map(|m| m.content.to_summary_text())
            .collect::<Vec<_>>()
            .join("\n\n");
        let system_prompt = build_system_prompt(&base_system, tools);
        let conversation = &messages[system_len..];
        let hashes: Vec<u64> = conversation.iter().map(message_hash).collect();
        let key = hash_of(&(&model, &system_prompt, &self.config.effort));

        let mut slot = self.session.lock().await;
        let reusable = slot.as_mut().and_then(|s| {
            if s.key != key || !matches!(s.child.try_wait(), Ok(None)) {
                return None;
            }
            new_messages_since(conversation, &hashes, &s.sent)
        });

        let (mut session, payload) = match reusable {
            Some(new_messages) => {
                let session = slot.take().expect("checked above");
                (session, render_new_messages(new_messages, conversation))
            }
            None => {
                if let Some(mut old) = slot.take() {
                    let _ = old.child.start_kill();
                }
                let session = spawn_session(
                    &model,
                    self.config.effort.as_deref(),
                    &system_prompt,
                    key,
                    workspace_root,
                )
                .await?;
                (session, render_replay(conversation))
            }
        };
        drop(slot);

        if payload.trim().is_empty() {
            return Err(ProviderError::Other(
                "nothing new to send to Claude Code".into(),
            ));
        }

        let line = json!({
            "type": "user",
            "message": { "role": "user", "content": payload },
        });
        let mut bytes =
            serde_json::to_vec(&line).map_err(|e| ProviderError::Other(e.to_string()))?;
        bytes.push(b'\n');
        if let Err(err) = session.stdin.write_all(&bytes).await {
            let _ = session.child.start_kill();
            return Err(ProviderError::Transient(format!(
                "Claude Code process closed its input: {err}{}",
                stderr_suffix(&session.stderr_tail)
            )));
        }
        let _ = session.stdin.flush().await;
        session.sent = hashes;

        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let slot = self.session.clone();
        tokio::spawn(async move {
            let finished = pump_turn(&mut session, &tx).await;
            if finished {
                // Turn completed cleanly: keep the process for the next turn.
                *slot.lock().await = Some(session);
            } else {
                let _ = session.child.start_kill();
            }
        });
        Ok(rx)
    }
}

async fn spawn_session(
    model: &str,
    effort: Option<&str>,
    system_prompt: &str,
    key: u64,
    workspace_root: &Path,
) -> Result<CcSession, ProviderError> {
    let mut prompt_file = tempfile::Builder::new()
        .prefix("dcode-ai-claude-system-")
        .suffix(".md")
        .tempfile()
        .map_err(|e| ProviderError::Other(format!("system prompt file: {e}")))?;
    prompt_file
        .write_all(system_prompt.as_bytes())
        .and_then(|_| prompt_file.flush())
        .map_err(|e| ProviderError::Other(format!("system prompt file: {e}")))?;

    let mut cmd = windows_compat_command_tokio("claude");
    cmd.args([
        "-p",
        "--input-format",
        "stream-json",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        // Model only: no built-in tools, MCP, settings, skills or session files.
        "--tools",
        "",
        "--strict-mcp-config",
        "--disable-slash-commands",
        "--no-session-persistence",
        "--setting-sources",
        "",
        "--model",
        model,
    ]);
    cmd.arg("--system-prompt-file").arg(prompt_file.path());
    if let Some(effort) = effort.filter(|e| !e.trim().is_empty())
        && ClaudeFamily::for_model(model).is_adaptive()
    {
        cmd.args(["--effort", effort]);
    }
    // Use the signed-in Claude account, never an API key that happens to be
    // exported in this shell.
    cmd.env_remove("ANTHROPIC_API_KEY")
        .env_remove("ANTHROPIC_AUTH_TOKEN")
        .current_dir(workspace_root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd.spawn().map_err(|e| {
        ProviderError::Configuration(format!("failed to start Claude Code (`claude`): {e}"))
    })?;
    let stdin = child.stdin.take().expect("piped stdin");
    let stdout = BufReader::new(child.stdout.take().expect("piped stdout")).lines();
    let stderr_tail = Arc::new(std::sync::Mutex::new(String::new()));
    if let Some(stderr) = child.stderr.take() {
        let tail = stderr_tail.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let mut t = tail.lock().unwrap_or_else(|e| e.into_inner());
                t.push_str(&line);
                t.push('\n');
                if t.len() > 4096 {
                    let cut = t.len() - 4096;
                    let cut = (cut..t.len()).find(|&i| t.is_char_boundary(i)).unwrap_or(0);
                    t.drain(..cut);
                }
            }
        });
    }

    Ok(CcSession {
        child,
        stdin,
        stdout,
        stderr_tail,
        key,
        sent: Vec::new(),
        _prompt_file: prompt_file,
    })
}

/// Stream one turn's events to `tx`. Returns `true` when the turn ended with a
/// `result` event and the process can be reused.
async fn pump_turn(session: &mut CcSession, tx: &tokio::sync::mpsc::Sender<StreamChunk>) -> bool {
    let mut splitter = ToolCallSplitter::default();
    let mut tool_calls = 0usize;
    let mut input_tokens = 0u64;

    loop {
        let line = match tokio::time::timeout(LINE_TIMEOUT, session.stdout.next_line()).await {
            Ok(Ok(Some(line))) => line,
            Ok(Ok(None)) => {
                let _ = tx
                    .send(StreamChunk::Error(format!(
                        "Claude Code exited unexpectedly{}",
                        stderr_suffix(&session.stderr_tail)
                    )))
                    .await;
                return false;
            }
            Ok(Err(err)) => {
                let _ = tx
                    .send(StreamChunk::Error(format!("Claude Code read error: {err}")))
                    .await;
                return false;
            }
            Err(_) => {
                let _ = tx
                    .send(StreamChunk::Error(format!(
                        "Claude Code produced no output for {}s",
                        LINE_TIMEOUT.as_secs()
                    )))
                    .await;
                return false;
            }
        };
        let Ok(event) = serde_json::from_str::<Value>(&line) else {
            continue;
        };

        match event["type"].as_str().unwrap_or("") {
            "stream_event" => {
                let inner = &event["event"];
                match inner["type"].as_str().unwrap_or("") {
                    "message_start" => {
                        let usage = &inner["message"]["usage"];
                        input_tokens = usage["input_tokens"].as_u64().unwrap_or(0);
                        let read = usage["cache_read_input_tokens"].as_u64().unwrap_or(0);
                        let creation = usage["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                        if (read > 0 || creation > 0)
                            && tx
                                .send(StreamChunk::CacheUsage {
                                    read_tokens: read,
                                    creation_tokens: creation,
                                })
                                .await
                                .is_err()
                        {
                            return false;
                        }
                    }
                    "content_block_delta" => {
                        let delta = &inner["delta"];
                        let sent = match delta["type"].as_str().unwrap_or("") {
                            "text_delta" => {
                                let text = delta["text"].as_str().unwrap_or("");
                                match emit_segments(tx, splitter.feed(text), &mut tool_calls).await
                                {
                                    Flow::Continue => true,
                                    Flow::Abort => false,
                                    Flow::Cut => {
                                        // Claude's context now holds invented
                                        // results, so this process is dropped;
                                        // the next turn replays true history.
                                        if tool_calls > 0 {
                                            let _ = tx
                                                .send(StreamChunk::Stop(StopReason::ToolUse))
                                                .await;
                                        }
                                        let _ = tx.send(StreamChunk::Done).await;
                                        return false;
                                    }
                                }
                            }
                            "thinking_delta" => match delta["thinking"].as_str() {
                                Some(t) if !t.is_empty() => tx
                                    .send(StreamChunk::InternalDelta(t.to_string()))
                                    .await
                                    .is_ok(),
                                _ => true,
                            },
                            _ => true,
                        };
                        if !sent {
                            return false;
                        }
                    }
                    "message_delta" => {
                        let output_tokens = inner["usage"]["output_tokens"].as_u64().unwrap_or(0);
                        if (input_tokens > 0 || output_tokens > 0)
                            && tx
                                .send(StreamChunk::Usage {
                                    input_tokens,
                                    output_tokens,
                                })
                                .await
                                .is_err()
                        {
                            return false;
                        }
                        input_tokens = 0;
                        if inner["delta"]["stop_reason"].as_str() == Some("refusal") {
                            let category = inner["delta"]["stop_details"]["category"]
                                .as_str()
                                .map(str::to_string);
                            let _ = tx
                                .send(StreamChunk::Stop(StopReason::Refusal { category }))
                                .await;
                        } else if inner["delta"]["stop_reason"].as_str() == Some("max_tokens") {
                            let _ = tx.send(StreamChunk::Stop(StopReason::MaxTokens)).await;
                        }
                    }
                    _ => {}
                }
            }
            "result" => {
                if event["is_error"].as_bool().unwrap_or(false)
                    || event["subtype"].as_str().is_some_and(|s| s != "success")
                {
                    let detail = event["result"]
                        .as_str()
                        .or_else(|| event["subtype"].as_str())
                        .unwrap_or("unknown error");
                    let _ = tx
                        .send(StreamChunk::FatalError(format!(
                            "Claude Code error: {detail}"
                        )))
                        .await;
                    return false;
                }
                // An unterminated <tool_call> means the call was cut off.
                match splitter.finish() {
                    Ok(Some(rest)) => {
                        let _ = tx.send(StreamChunk::TextDelta(rest)).await;
                    }
                    Ok(None) => {}
                    Err(partial) => {
                        let _ = tx
                            .send(StreamChunk::Error(format!(
                                "Claude ended the turn inside an unterminated tool call: {}",
                                truncate(&partial, 200)
                            )))
                            .await;
                        return false;
                    }
                }
                if tool_calls > 0 {
                    let _ = tx.send(StreamChunk::Stop(StopReason::ToolUse)).await;
                }
                let _ = tx.send(StreamChunk::Done).await;
                return true;
            }
            _ => {}
        }
    }
}

enum Flow {
    Continue,
    /// Fabricated tool results began: end the turn now with what was real.
    Cut,
    /// Receiver gone or malformed tool call: the turn failed.
    Abort,
}

/// Forward splitter output to the agent.
async fn emit_segments(
    tx: &tokio::sync::mpsc::Sender<StreamChunk>,
    segments: Vec<Segment>,
    tool_calls: &mut usize,
) -> Flow {
    for segment in segments {
        let chunk = match segment {
            Segment::FabricatedResult => return Flow::Cut,
            Segment::Text(text) => StreamChunk::TextDelta(text),
            Segment::ToolCall(raw) => match parse_tool_call(&raw) {
                Ok((name, input)) => {
                    *tool_calls += 1;
                    StreamChunk::ToolUse(ToolCall {
                        id: format!("cc_{}_{}", std::process::id(), next_call_seq()),
                        name,
                        input,
                    })
                }
                Err(err) => {
                    let _ = tx
                        .send(StreamChunk::Error(format!(
                            "Claude emitted a malformed tool call ({err}): {}",
                            truncate(&raw, 200)
                        )))
                        .await;
                    return Flow::Abort;
                }
            },
        };
        if tx.send(chunk).await.is_err() {
            return Flow::Abort;
        }
    }
    Flow::Continue
}

fn next_call_seq() -> u64 {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

fn parse_tool_call(raw: &str) -> Result<(String, Value), String> {
    let body = raw.trim();
    // Tolerate a fenced block inside the tags.
    let body = body
        .strip_prefix("```json")
        .or_else(|| body.strip_prefix("```"))
        .map(|b| b.trim_end().trim_end_matches("```").trim())
        .unwrap_or(body);
    let value: Value = serde_json::from_str(body).map_err(|e| e.to_string())?;
    let name = value["name"]
        .as_str()
        .filter(|n| !n.is_empty())
        .ok_or("missing \"name\"")?
        .to_string();
    let input = match value.get("input").or_else(|| value.get("arguments")) {
        None | Some(Value::Null) => json!({}),
        Some(Value::Object(map)) => Value::Object(map.clone()),
        Some(other) => return Err(format!("\"input\" must be an object, got {other}")),
    };
    Ok((name, input))
}

#[derive(Debug, PartialEq)]
enum Segment {
    Text(String),
    ToolCall(String),
    /// The model began writing a `<tool_result>` itself; everything from here
    /// on is fabricated and is dropped.
    FabricatedResult,
}

/// Splits streamed text into visible text and `<tool_call>` bodies, holding
/// back any tail that could be the start of a marker split across deltas.
#[derive(Default)]
struct ToolCallSplitter {
    in_call: bool,
    carry: String,
    call: String,
    /// Set once a fabricated result was seen; all later input is ignored.
    cut: bool,
}

impl ToolCallSplitter {
    fn feed(&mut self, input: &str) -> Vec<Segment> {
        if self.cut {
            return Vec::new();
        }
        let mut work = std::mem::take(&mut self.carry);
        work.push_str(input);
        let mut out = Vec::new();
        loop {
            if !self.in_call
                && let Some(pos) = work.find(RESULT_OPEN)
                && work.find(TOOL_OPEN).is_none_or(|open| pos < open)
            {
                if pos > 0 {
                    out.push(Segment::Text(work[..pos].to_string()));
                }
                out.push(Segment::FabricatedResult);
                self.cut = true;
                return out;
            }
            let marker = if self.in_call { TOOL_CLOSE } else { TOOL_OPEN };
            if let Some(pos) = work.find(marker) {
                let before = &work[..pos];
                if self.in_call {
                    self.call.push_str(before);
                    out.push(Segment::ToolCall(std::mem::take(&mut self.call)));
                } else if !before.is_empty() {
                    out.push(Segment::Text(before.to_string()));
                }
                self.in_call = !self.in_call;
                work = work[pos + marker.len()..].to_string();
            } else {
                let keep = if self.in_call {
                    partial_marker_tail_len(&work, marker)
                } else {
                    partial_marker_tail_len(&work, marker)
                        .max(partial_marker_tail_len(&work, RESULT_OPEN))
                };
                let split = work.len() - keep;
                if self.in_call {
                    self.call.push_str(&work[..split]);
                } else if split > 0 {
                    out.push(Segment::Text(work[..split].to_string()));
                }
                self.carry = work[split..].to_string();
                break;
            }
        }
        out
    }

    /// End of stream: leftover visible text, or `Err(partial)` when a tool
    /// call was opened but never closed.
    fn finish(&mut self) -> Result<Option<String>, String> {
        let carry = std::mem::take(&mut self.carry);
        if self.cut {
            return Ok(None);
        }
        if self.in_call {
            let mut partial = std::mem::take(&mut self.call);
            partial.push_str(&carry);
            return Err(partial);
        }
        Ok((!carry.is_empty()).then_some(carry))
    }
}

/// Length of the longest suffix of `text` that is a proper prefix of `marker`.
fn partial_marker_tail_len(text: &str, marker: &str) -> usize {
    (1..marker.len())
        .rev()
        .find(|&n| {
            text.len() >= n
                && text.is_char_boundary(text.len() - n)
                && marker.starts_with(&text[text.len() - n..])
        })
        .unwrap_or(0)
}

/// Messages to send when `sent` is a prefix of the current conversation and
/// the only thing in between is Claude's own reply (which it already has).
/// `None` means history diverged and the process must be restarted.
fn new_messages_since<'a>(
    conversation: &'a [Message],
    hashes: &[u64],
    sent: &[u64],
) -> Option<&'a [Message]> {
    if sent.is_empty() || sent.len() > hashes.len() || hashes[..sent.len()] != *sent {
        return None;
    }
    let rest = &conversation[sent.len()..];
    // Skip Claude's own reply (recorded by the agent after the last turn).
    let skip = rest
        .iter()
        .take_while(|m| m.role == Role::Assistant)
        .count();
    let new = &rest[skip..];
    if new.is_empty() || new.iter().any(|m| m.role == Role::Assistant) {
        return None;
    }
    Some(new)
}

fn message_hash(message: &Message) -> u64 {
    let calls = message
        .tool_calls
        .as_ref()
        .map(|c| serde_json::to_string(c).unwrap_or_default());
    hash_of(&(
        format!("{:?}", message.role),
        message.content.to_summary_text(),
        &message.tool_call_id,
        calls,
    ))
}

fn hash_of<T: Hash>(value: &T) -> u64 {
    let mut h = DefaultHasher::new();
    value.hash(&mut h);
    h.finish()
}

fn build_system_prompt(base: &str, tools: &[ToolDefinition]) -> String {
    let mut out = String::new();
    if !base.trim().is_empty() {
        out.push_str(base.trim());
        out.push_str("\n\n");
    }
    if tools.is_empty() {
        return out;
    }
    out.push_str(
        "# Tool use\n\
You are running inside dcode-ai. You have no built-in tools. The only way to act is the protocol below; dcode-ai executes each call and returns the result.\n\n\
To call a tool, write a block exactly like this:\n\
<tool_call>\n{\"name\": \"<tool name>\", \"input\": { <arguments matching the tool's schema> }}\n</tool_call>\n\n\
Rules:\n\
- The block must contain one valid JSON object and nothing else. Use one block per call; several blocks in one reply run in order.\n\
- After your last </tool_call>, STOP your reply immediately. You cannot see results in the same reply: they only arrive in the NEXT user message, written by dcode-ai as <tool_result id=\"…\" name=\"…\">…</tool_result>.\n\
- Never write <tool_result> yourself and never guess what a tool returned. If you need a result before deciding the next step, make the call and stop.\n\
- When the task is complete, answer normally with no tool_call block.\n\n\
## Available tools\n",
    );
    for tool in tools {
        out.push_str(&format!(
            "\n### {}\n{}\nInput schema: {}\n",
            tool.name,
            tool.description.trim(),
            serde_json::to_string(&tool.parameters).unwrap_or_else(|_| "{}".into())
        ));
    }
    out
}

/// Format the messages Claude hasn't seen as one user turn.
fn render_new_messages(new: &[Message], conversation: &[Message]) -> String {
    let mut out = String::new();
    for message in new {
        render_message(message, conversation, &mut out, false);
    }
    out
}

/// Replay a whole conversation into a fresh process as one user turn.
fn render_replay(conversation: &[Message]) -> String {
    // Single fresh user message: send it as-is.
    if let [only] = conversation
        && only.role == Role::User
    {
        return message_text(&only.content);
    }
    let mut out = String::from(
        "The conversation so far is below (you are ASSISTANT). Continue it: respond to the latest USER message or tool results.\n\n<transcript>\n",
    );
    for message in conversation {
        render_message(message, conversation, &mut out, true);
    }
    out.push_str("</transcript>\n");
    out
}

fn render_message(message: &Message, conversation: &[Message], out: &mut String, labelled: bool) {
    match message.role {
        Role::User => {
            if labelled {
                out.push_str("USER:\n");
            }
            out.push_str(&message_text(&message.content));
            out.push_str("\n\n");
        }
        Role::Assistant => {
            if labelled {
                out.push_str("ASSISTANT:\n");
            }
            out.push_str(&message.content.to_summary_text());
            out.push('\n');
            for call in message.tool_calls.iter().flatten() {
                out.push_str(&format!(
                    "{TOOL_OPEN}\n{}\n{TOOL_CLOSE}\n",
                    json!({ "name": call.name, "input": call.arguments })
                ));
            }
            out.push('\n');
        }
        Role::Tool => {
            let id = message.tool_call_id.as_deref().unwrap_or("");
            let name = conversation
                .iter()
                .filter_map(|m| m.tool_calls.as_ref())
                .flatten()
                .find(|c| c.id == id)
                .map(|c| c.name.as_str())
                .unwrap_or("tool");
            out.push_str(&format!(
                "<tool_result id=\"{id}\" name=\"{name}\">\n{}\n</tool_result>\n\n",
                message.content.to_summary_text()
            ));
        }
        Role::System => {
            out.push_str(&format!(
                "[system note]\n{}\n\n",
                message.content.to_summary_text()
            ));
        }
    }
}

fn message_text(content: &MessageContent) -> String {
    content.to_summary_text()
}

fn stderr_suffix(tail: &Arc<std::sync::Mutex<String>>) -> String {
    let t = tail.lock().unwrap_or_else(|e| e.into_inner());
    let t = t.trim();
    if t.is_empty() {
        String::new()
    } else {
        format!(": {}", truncate(t, 600))
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dcode_ai_common::message::MessageToolCall;

    fn split_all(chunks: &[&str]) -> (Vec<Segment>, Result<Option<String>, String>) {
        let mut s = ToolCallSplitter::default();
        let mut out = Vec::new();
        for c in chunks {
            out.extend(s.feed(c));
        }
        (out, s.finish())
    }

    #[test]
    fn splitter_extracts_tool_calls_across_chunk_boundaries() {
        let (segments, rest) = split_all(&[
            "Reading it.\n<tool",
            "_call>\n{\"name\":\"read_file\",",
            "\"input\":{\"path\":\"a.rs\"}}\n</tool_c",
            "all>done",
        ]);
        assert_eq!(segments[0], Segment::Text("Reading it.\n".into()));
        let Segment::ToolCall(raw) = &segments[1] else {
            panic!("expected tool call, got {segments:?}");
        };
        assert_eq!(
            parse_tool_call(raw).unwrap(),
            ("read_file".to_string(), json!({"path": "a.rs"}))
        );
        assert_eq!(segments[2], Segment::Text("done".into()));
        assert_eq!(rest, Ok(None));
    }

    #[test]
    fn fabricated_result_cuts_the_stream() {
        let (segments, rest) = split_all(&[
            "<tool_call>{\"name\":\"read_file\",\"input\":{}}</tool_call>\n<tool_res",
            "ult id=\"1\">invented</tool_result>more invented text",
        ]);
        assert!(matches!(segments[0], Segment::ToolCall(_)));
        assert_eq!(segments.last(), Some(&Segment::FabricatedResult));
        assert!(
            !segments
                .iter()
                .any(|s| matches!(s, Segment::Text(t) if t.contains("invented")))
        );
        assert_eq!(rest, Ok(None));
    }

    #[test]
    fn splitter_reports_unterminated_call() {
        let (_, rest) = split_all(&["<tool_call>{\"name\":\"x\""]);
        assert!(rest.is_err());
    }

    #[test]
    fn parse_tool_call_accepts_fences_and_missing_input() {
        assert_eq!(
            parse_tool_call("```json\n{\"name\":\"git_status\"}\n```").unwrap(),
            ("git_status".to_string(), json!({}))
        );
        assert!(parse_tool_call("{\"input\":{}}").is_err());
        assert!(parse_tool_call("{\"name\":\"x\",\"input\":3}").is_err());
    }

    #[test]
    fn only_unseen_messages_are_sent_and_divergence_restarts() {
        let call = MessageToolCall {
            id: "cc_1".into(),
            name: "read_file".into(),
            arguments: json!({"path": "a"}),
        };
        let conv = vec![
            Message::user("fix a"),
            Message::assistant_with_tool_calls("", vec![call]),
            Message::tool("cc_1", "contents"),
        ];
        let hashes: Vec<u64> = conv.iter().map(message_hash).collect();
        let sent = hashes[..1].to_vec();

        let new = new_messages_since(&conv, &hashes, &sent).expect("reuse");
        assert_eq!(new.len(), 1);
        let rendered = render_new_messages(new, &conv);
        assert!(rendered.contains("<tool_result id=\"cc_1\" name=\"read_file\">"));

        // Edited first message → restart.
        let mut edited = conv.clone();
        edited[0] = Message::user("something else");
        let edited_hashes: Vec<u64> = edited.iter().map(message_hash).collect();
        assert!(new_messages_since(&edited, &edited_hashes, &sent).is_none());

        // Nothing new (e.g. an empty-completion retry) → restart.
        assert!(new_messages_since(&conv[..1], &hashes[..1], &sent).is_none());
    }

    #[test]
    fn system_prompt_lists_tools_and_protocol() {
        let prompt = build_system_prompt(
            "be terse",
            &[ToolDefinition {
                name: "read_file".into(),
                description: "Read a file".into(),
                parameters: json!({"type": "object"}),
            }],
        );
        assert!(prompt.starts_with("be terse"));
        assert!(prompt.contains("<tool_call>"));
        assert!(prompt.contains("### read_file"));
    }
}
