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
/// Claude's native function-call markup, which it sometimes writes instead of
/// `<tool_call>` — run those calls too rather than printing them.
const INVOKE_OPENS: [&str; 2] = ["<invoke name=\"", concat!("<", "antml", ":invoke name=\"")];
const INVOKE_CLOSES: [&str; 2] = ["</invoke>", concat!("</", "antml", ":invoke>")];
/// Wrapper tags around native calls; dropped from the visible text.
const CALL_WRAPPERS: [&str; 4] = [
    "<function_calls>",
    "</function_calls>",
    concat!("<", "antml", ":function_calls>"),
    concat!("</", "antml", ":function_calls>"),
];
/// The same call this many times in one reply means the model is looping.
const MAX_REPEATED_CALLS: usize = 3;

/// Longest silence tolerated between two stdout lines. Thinking on hard tasks
/// can be quiet for a while, so this is generous.
const LINE_TIMEOUT: Duration = Duration::from_secs(600);

pub struct ClaudeCodeProvider {
    config: AnthropicConfig,
    session: Arc<tokio::sync::Mutex<Option<CcSession>>>,
}

/// `claude` processes started for conversations in this run (the first one
/// and every restart; each restart re-sends the whole conversation).
pub static SESSION_STARTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Short-lived `claude` processes for one-off calls (command checks,
/// summaries, titles).
pub static ONE_SHOTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Why conversation processes were restarted (each restart re-sends the
/// conversation without the cache): system prompt / model / effort changed,
/// a tool's schema changed, history rewritten (compaction, rewind), the
/// process ended.
pub static RESTART_PROMPT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static RESTART_TOOLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static RESTART_HISTORY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static RESTART_ENDED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn count(counter: &std::sync::atomic::AtomicU64) {
    counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

struct CcSession {
    child: Child,
    stdin: ChildStdin,
    stdout: Lines<BufReader<ChildStdout>>,
    stderr_tail: Arc<std::sync::Mutex<String>>,
    /// Model + system prompt (without tool schemas) + effort this process
    /// was started with.
    key: u64,
    /// Tool schemas Claude has: from the system prompt, plus ones announced
    /// later in a message (tools loaded by `tool_search`, late MCP servers).
    tools_sent: std::collections::HashMap<String, u64>,
    /// The last reply was interrupted (Esc): Claude saw it finish, dcode-ai
    /// did not use it. Said so in the next message.
    interrupted: bool,
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

        // One-off calls without tools (auto mode's command check, summaries,
        // titles) get a throwaway process: reusing the session's would kill
        // it, and the next real step would re-send the whole conversation.
        if tools.is_empty() {
            return one_shot(messages, &model, workspace_root).await;
        }

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
        // Tools are not part of the key: a changed tool list must not restart
        // the process, which would re-send (and re-cache) the whole history.
        let key = hash_of(&(&model, &base_system, &self.config.effort));
        let tool_hashes: std::collections::HashMap<String, u64> = tools
            .iter()
            .map(|t| (t.name.clone(), tool_hash(t)))
            .collect();

        let mut slot = self.session.lock().await;
        let reusable = slot.as_mut().and_then(|s| {
            if s.key != key {
                tracing::info!(
                    tools = tools.len(),
                    "claude-code: system prompt or tools changed; starting a new process"
                );
                count(&RESTART_PROMPT);
                return None;
            }
            if !matches!(s.child.try_wait(), Ok(None)) {
                tracing::info!("claude-code: process ended; starting a new one");
                count(&RESTART_ENDED);
                return None;
            }
            // A tool whose schema changed can't be announced as "new".
            if tool_hashes
                .iter()
                .any(|(n, h)| s.tools_sent.get(n).is_some_and(|old| old != h))
            {
                tracing::info!("claude-code: a tool's schema changed; starting a new process");
                count(&RESTART_TOOLS);
                return None;
            }
            let new = new_messages_since(conversation, &hashes, &s.sent);
            if new.is_none() {
                let same = s
                    .sent
                    .iter()
                    .zip(&hashes)
                    .take_while(|(a, b)| a == b)
                    .count();
                tracing::info!(
                    sent = s.sent.len(),
                    now = hashes.len(),
                    same_prefix = same,
                    "claude-code: history changed; replaying it in a new process"
                );
                count(&RESTART_HISTORY);
            }
            new
        });

        let (mut session, payload) = match reusable {
            Some(new_messages) => {
                let mut session = slot.take().expect("checked above");
                let added: Vec<&ToolDefinition> = tools
                    .iter()
                    .filter(|t| !session.tools_sent.contains_key(&t.name))
                    .collect();
                let mut payload = if session.interrupted {
                    session.interrupted = false;
                    "(The user interrupted your previous reply; any tool calls in it were not run.)\n\n"
                        .to_string()
                } else {
                    String::new()
                };
                payload.push_str(&render_added_tools(&added));
                payload.push_str(&render_new_messages(new_messages, conversation));
                for t in added {
                    session.tools_sent.insert(t.name.clone(), tool_hash(t));
                }
                (session, payload)
            }
            None => {
                if let Some(mut old) = slot.take() {
                    let _ = old.child.start_kill();
                }
                let mut session = spawn_session(
                    &model,
                    self.config.effort.as_deref(),
                    &system_prompt,
                    key,
                    workspace_root,
                )
                .await?;
                session.tools_sent = tool_hashes.clone();
                SESSION_STARTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
            let mut finished = pump_turn(&mut session, &tx).await;
            // Interrupted (the receiver went away): let Claude finish the
            // reply unseen and keep the process, instead of re-sending the
            // whole conversation to a new one.
            if !finished && tx.is_closed() && drain_turn(&mut session).await {
                session.interrupted = true;
                finished = true;
            }
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

/// Run a tool-less conversation in its own short-lived `claude` process.
async fn one_shot(
    messages: &[Message],
    model: &str,
    workspace_root: &Path,
) -> Result<tokio::sync::mpsc::Receiver<StreamChunk>, ProviderError> {
    let system_len = messages
        .iter()
        .take_while(|m| m.role == Role::System)
        .count();
    let system = messages[..system_len]
        .iter()
        .map(|m| m.content.to_summary_text())
        .collect::<Vec<_>>()
        .join("\n\n");
    let payload = render_replay(&messages[system_len..]);
    if payload.trim().is_empty() {
        return Err(ProviderError::Other(
            "nothing to send to Claude Code".into(),
        ));
    }
    ONE_SHOTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut session = spawn_session(model, None, &system, 0, workspace_root).await?;
    let line = json!({
        "type": "user",
        "message": { "role": "user", "content": payload },
    });
    let mut bytes = serde_json::to_vec(&line).map_err(|e| ProviderError::Other(e.to_string()))?;
    bytes.push(b'\n');
    if let Err(err) = session.stdin.write_all(&bytes).await {
        let _ = session.child.start_kill();
        return Err(ProviderError::Transient(format!(
            "Claude Code process closed its input: {err}"
        )));
    }
    let _ = session.stdin.flush().await;
    let (tx, rx) = tokio::sync::mpsc::channel(64);
    tokio::spawn(async move {
        pump_turn(&mut session, &tx).await;
        let _ = session.child.start_kill();
    });
    Ok(rx)
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
        tools_sent: Default::default(),
        interrupted: false,
        sent: Vec::new(),
        _prompt_file: prompt_file,
    })
}

/// Read and drop the rest of an interrupted turn. `true` when it ended with
/// a `result` event, so the process can be reused.
async fn drain_turn(session: &mut CcSession) -> bool {
    loop {
        match tokio::time::timeout(LINE_TIMEOUT, session.stdout.next_line()).await {
            Ok(Ok(Some(line))) => {
                if serde_json::from_str::<Value>(&line)
                    .is_ok_and(|e| e["type"].as_str() == Some("result"))
                {
                    return true;
                }
            }
            _ => return false,
        }
    }
}

/// Stream one turn's events to `tx`. Returns `true` when the turn ended with a
/// `result` event and the process can be reused.
async fn pump_turn(session: &mut CcSession, tx: &tokio::sync::mpsc::Sender<StreamChunk>) -> bool {
    let mut splitter = ToolCallSplitter::default();
    let mut tool_calls = 0usize;
    let mut seen = SeenCalls::default();
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
                                match emit_segments(
                                    tx,
                                    splitter.feed(text),
                                    &mut tool_calls,
                                    &mut seen,
                                )
                                .await
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
                        tool_calls += 1;
                        let _ = tx
                            .send(StreamChunk::ToolUse(invalid_call(
                                "the call was never closed with </tool_call>",
                                &partial,
                            )))
                            .await;
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

/// Calls already sent in this reply, to drop repeats and spot a loop.
#[derive(Default)]
struct SeenCalls {
    calls: std::collections::HashSet<String>,
    repeats: usize,
}

/// Forward splitter output to the agent.
async fn emit_segments(
    tx: &tokio::sync::mpsc::Sender<StreamChunk>,
    segments: Vec<Segment>,
    tool_calls: &mut usize,
    seen: &mut SeenCalls,
) -> Flow {
    for segment in segments {
        let parsed = match &segment {
            Segment::ToolCall(raw) => Some(parse_tool_call(raw)),
            Segment::Invoke(raw) => Some(parse_invoke(raw)),
            _ => None,
        };
        let chunk = match (segment, parsed) {
            (Segment::FabricatedResult, _) => return Flow::Cut,
            (Segment::Text(text), _) => {
                let text = CALL_WRAPPERS.iter().fold(text, |t, w| t.replace(w, ""));
                if text.is_empty() {
                    continue;
                }
                StreamChunk::TextDelta(text)
            }
            (_, Some(Ok((name, input)))) => {
                // A call repeated word for word in the same reply is a loop:
                // run it once, and end the reply if it keeps coming.
                if !seen.calls.insert(format!("{name} {input}")) {
                    seen.repeats += 1;
                    if seen.repeats >= MAX_REPEATED_CALLS {
                        return Flow::Cut;
                    }
                    continue;
                }
                *tool_calls += 1;
                StreamChunk::ToolUse(ToolCall {
                    id: format!("cc_{}_{}", std::process::id(), next_call_seq()),
                    name,
                    input,
                })
            }
            // Like Claude Code: hand the parse error back to the model as a
            // failed tool result so it can fix the call, instead of failing
            // the whole turn.
            (Segment::ToolCall(raw) | Segment::Invoke(raw), Some(Err(err))) => {
                *tool_calls += 1;
                StreamChunk::ToolUse(invalid_call(&err, &raw))
            }
            (Segment::ToolCall(_) | Segment::Invoke(_), None) => continue,
        };
        if tx.send(chunk).await.is_err() {
            return Flow::Abort;
        }
    }
    Flow::Continue
}

/// A tool call that could not be parsed, sent through as the
/// `invalid_tool_call` tool so the model gets the error back.
fn invalid_call(err: &str, raw: &str) -> ToolCall {
    ToolCall {
        id: format!("cc_{}_{}", std::process::id(), next_call_seq()),
        name: dcode_ai_common::tool::INVALID_TOOL_CALL.into(),
        input: json!({ "error": err, "raw": truncate(raw, 400) }),
    }
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

/// A native `<invoke name="…">` call: the tool name and its
/// `<parameter name="…">value</parameter>` children (JSON values when they
/// parse as JSON, strings otherwise).
fn parse_invoke(raw: &str) -> Result<(String, Value), String> {
    let raw = raw.replace(concat!("antml", ":"), "");
    let rest = raw
        .trim_start()
        .strip_prefix("<invoke name=\"")
        .ok_or("expected <invoke name=\"…\">")?;
    let end = rest.find('"').ok_or("unterminated tool name")?;
    let name = rest[..end].trim().to_string();
    if name.is_empty() {
        return Err("missing tool name".into());
    }
    let mut input = serde_json::Map::new();
    let mut body = &rest[end..];
    while let Some(p) = body.find("<parameter name=\"") {
        let after = &body[p + "<parameter name=\"".len()..];
        let q = after.find('"').ok_or("unterminated parameter name")?;
        let key = after[..q].to_string();
        let open_end = after[q..].find('>').ok_or("unterminated <parameter>")? + q + 1;
        let value_and_rest = &after[open_end..];
        let close = value_and_rest
            .find("</parameter>")
            .ok_or_else(|| format!("parameter `{key}` was never closed"))?;
        let text = &value_and_rest[..close];
        let value = match serde_json::from_str::<Value>(text.trim()) {
            Ok(v) if !v.is_string() => v,
            _ => Value::String(text.to_string()),
        };
        input.insert(key, value);
        body = &value_and_rest[close + "</parameter>".len()..];
    }
    Ok((name, Value::Object(input)))
}

#[derive(Debug, PartialEq)]
enum Segment {
    Text(String),
    ToolCall(String),
    /// A native `<invoke name="…">…` call (without the closing tag).
    Invoke(String),
    /// The model began writing a `<tool_result>` itself; everything from here
    /// on is fabricated and is dropped.
    FabricatedResult,
}

/// Splits streamed text into visible text and `<tool_call>` bodies, holding
/// back any tail that could be the start of a marker split across deltas.
#[derive(Default)]
struct ToolCallSplitter {
    in_call: bool,
    /// The open call is a native `<invoke>` (closed by `</invoke>`).
    invoke: bool,
    carry: String,
    call: String,
    /// Set once a fabricated result was seen; all later input is ignored.
    cut: bool,
    /// Inside a ``` code block in the visible text: markers there are
    /// examples, not protocol.
    fence_open: bool,
    /// Last character of text already emitted, so a backtick that arrived
    /// in an earlier chunk still quotes a marker at the start of this one.
    last_char: Option<char>,
}

/// Flip `fence` for every ``` in `s` and remember its last character.
fn track_fences(fence: &mut bool, last: &mut Option<char>, s: &str) {
    if s.matches("```").count() % 2 == 1 {
        *fence = !*fence;
    }
    if let Some(c) = s.chars().next_back() {
        *last = Some(c);
    }
}

impl ToolCallSplitter {
    fn feed(&mut self, input: &str) -> Vec<Segment> {
        if self.cut {
            return Vec::new();
        }
        let mut work = std::mem::take(&mut self.carry);
        work.push_str(input);
        let mut out = Vec::new();
        let mut text = String::new();
        loop {
            if self.in_call {
                let closes: &[&str] = if self.invoke {
                    &INVOKE_CLOSES
                } else {
                    &[TOOL_CLOSE]
                };
                let found = closes
                    .iter()
                    .filter_map(|c| work.find(c).map(|p| (p, c.len())))
                    .min();
                if let Some((pos, len)) = found {
                    self.call.push_str(&work[..pos]);
                    let raw = std::mem::take(&mut self.call);
                    out.push(if self.invoke {
                        Segment::Invoke(raw)
                    } else {
                        Segment::ToolCall(raw)
                    });
                    self.in_call = false;
                    self.invoke = false;
                    work = work[pos + len..].to_string();
                    continue;
                }
                let tail = closes
                    .iter()
                    .map(|c| partial_marker_tail_len(&work, c))
                    .max()
                    .unwrap_or(0);
                let split = work.len() - tail;
                self.call.push_str(&work[..split]);
                self.carry = work[split..].to_string();
                break;
            }
            match next_marker(&work, self.fence_open, self.last_char) {
                Marker::Result(pos) => {
                    text.push_str(&work[..pos]);
                    track_fences(&mut self.fence_open, &mut self.last_char, &work[..pos]);
                    if !text.is_empty() {
                        out.push(Segment::Text(text));
                    }
                    out.push(Segment::FabricatedResult);
                    self.cut = true;
                    return out;
                }
                Marker::Call { start, body } => {
                    text.push_str(&work[..start]);
                    track_fences(&mut self.fence_open, &mut self.last_char, &work[..start]);
                    if !text.is_empty() {
                        out.push(Segment::Text(std::mem::take(&mut text)));
                    }
                    self.in_call = true;
                    work = work[body..].to_string();
                }
                Marker::Invoke(start) => {
                    text.push_str(&work[..start]);
                    track_fences(&mut self.fence_open, &mut self.last_char, &work[..start]);
                    if !text.is_empty() {
                        out.push(Segment::Text(std::mem::take(&mut text)));
                    }
                    // Keep the opening tag: it carries the tool name.
                    self.in_call = true;
                    self.invoke = true;
                    work = work[start..].to_string();
                }
                Marker::Undecided(pos) => {
                    text.push_str(&work[..pos]);
                    track_fences(&mut self.fence_open, &mut self.last_char, &work[..pos]);
                    self.carry = work[pos..].to_string();
                    break;
                }
                Marker::None => {
                    let keep = INVOKE_OPENS
                        .iter()
                        .map(|m| partial_marker_tail_len(&work, m))
                        .fold(
                            partial_marker_tail_len(&work, TOOL_OPEN)
                                .max(partial_marker_tail_len(&work, RESULT_OPEN)),
                            usize::max,
                        );
                    // Hold back a possible half-written ``` too, so fence
                    // tracking never splits one.
                    let keep = keep.max(work.len() - work.trim_end_matches('`').len());
                    let split = work.len() - keep;
                    text.push_str(&work[..split]);
                    track_fences(&mut self.fence_open, &mut self.last_char, &work[..split]);
                    self.carry = work[split..].to_string();
                    break;
                }
            }
        }
        if !text.is_empty() {
            out.push(Segment::Text(text));
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

/// The first protocol marker in visible text.
#[derive(Debug, PartialEq)]
enum Marker {
    /// No marker (a partial one may still be forming at the end).
    None,
    /// A real `<tool_call>` at `start`; its body begins at `body`.
    Call { start: usize, body: usize },
    /// A fabricated `<tool_result>` at this offset.
    Result(usize),
    /// A `<tool_call>` whose following text has not arrived yet.
    Undecided(usize),
    /// A native `<invoke name="…">` call at this offset.
    Invoke(usize),
}

/// Find the first marker that is protocol, not prose. A marker written in
/// backticks (`` `<tool_call>` ``) is a mention, and so is a `<tool_call>`
/// not followed by its JSON body (`{` or a fenced block) — the model
/// talking *about* the protocol must not open a call.
fn next_marker(work: &str, fence_open: bool, before: Option<char>) -> Marker {
    let mut from = 0;
    loop {
        let call = work[from..].find(TOOL_OPEN).map(|p| p + from);
        let result = work[from..].find(RESULT_OPEN).map(|p| p + from);
        let invoke = INVOKE_OPENS
            .iter()
            .filter_map(|m| work[from..].find(m).map(|p| (p + from, m.len())))
            .min();
        let (pos, is_call) = match (call, result) {
            (None, None) => match invoke {
                None => return Marker::None,
                Some((p, _)) => (p, false),
            },
            (Some(c), Some(r)) if r < c => (r, false),
            (Some(c), _) => (c, true),
            (None, Some(r)) => (r, false),
        };
        // A native call that comes first.
        if let Some((ipos, ilen)) = invoke
            && ipos <= pos
        {
            let quoted = if ipos == 0 {
                before == Some('`')
            } else {
                work[..ipos].ends_with('`')
            };
            let in_fence = fence_open ^ (work[..ipos].matches("```").count() % 2 == 1);
            if quoted || in_fence {
                from = ipos + ilen;
                continue;
            }
            return Marker::Invoke(ipos);
        }
        let len = if is_call {
            TOOL_OPEN.len()
        } else {
            RESULT_OPEN.len()
        };
        let quoted = if pos == 0 {
            before == Some('`')
        } else {
            work[..pos].ends_with('`')
        };
        let in_fence = fence_open ^ (work[..pos].matches("```").count() % 2 == 1);
        if quoted || in_fence {
            from = pos + len;
            continue;
        }
        if !is_call {
            return Marker::Result(pos);
        }
        let after = &work[pos + len..];
        let rest = after.trim_start();
        if rest.is_empty() || (rest.starts_with('`') && rest.len() < 3) {
            return Marker::Undecided(pos);
        }
        if rest.starts_with("```") {
            return Marker::Call {
                start: pos,
                body: pos + len,
            };
        }
        // A JSON body: `{` then a key or `}` — not `{json}` in prose.
        if let Some(obj) = rest.strip_prefix('{') {
            match obj.trim_start().chars().next() {
                None => return Marker::Undecided(pos),
                Some('"') | Some('}') => {
                    return Marker::Call {
                        start: pos,
                        body: pos + len,
                    };
                }
                Some(_) => {}
            }
        }
        from = pos + len;
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

fn tool_hash(tool: &ToolDefinition) -> u64 {
    hash_of(&(
        &tool.name,
        &tool.description,
        serde_json::to_string(&tool.parameters).unwrap_or_default(),
    ))
}

fn render_tool(out: &mut String, tool: &ToolDefinition) {
    out.push_str(&format!(
        "\n### {}\n{}\nInput schema: {}\n",
        tool.name,
        tool.description.trim(),
        serde_json::to_string(&tool.parameters).unwrap_or_else(|_| "{}".into())
    ));
}

/// Tools that became available after the process started, announced at the
/// top of the next message so the cached prefix stays the same.
fn render_added_tools(added: &[&ToolDefinition]) -> String {
    if added.is_empty() {
        return String::new();
    }
    let mut out = String::from(
        "<new_tools>\nThese tools are now available too; call them with <tool_call> like the others.\n",
    );
    for tool in added {
        render_tool(&mut out, tool);
    }
    out.push_str("</new_tools>\n\n");
    out
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
- Write real calls as plain text, never inside a ``` code block. To mention or show the protocol in prose, wrap it in backticks (`<tool_call>`); mentions, examples and code blocks are never executed.\n\
- Use only <tool_call> blocks: no function_calls/invoke markup, and never repeat a call you already made in this reply.\n\
- When the task is complete, answer normally with no tool_call block.\n\n\
## Available tools\n",
    );
    for tool in tools {
        render_tool(&mut out, tool);
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
    #[test]
    fn tools_added_later_are_announced_in_the_message() {
        let tool = ToolDefinition {
            name: "zoho_list".into(),
            description: "List records".into(),
            parameters: serde_json::json!({"type": "object"}),
        };
        assert_eq!(render_added_tools(&[]), "");
        let text = render_added_tools(&[&tool]);
        assert!(text.starts_with("<new_tools>"), "{text}");
        assert!(text.contains("### zoho_list\nList records\nInput schema: {\"type\":\"object\"}"));
        let mut changed = tool.clone();
        changed.description = "List records (v2)".into();
        assert_ne!(tool_hash(&tool), tool_hash(&changed));
    }

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
    fn protocol_mentions_in_prose_stay_text() {
        // The model describing the protocol (the bug that ended a turn with
        // "unterminated tool call").
        let (segments, rest) = split_all(&[
            "Uses the `<tool_call>` text protocol** (`provider/claude_code.rs`).\n",
            "Results come back as `<tool_result>` blocks; a bare <tool_call> mention too.",
        ]);
        assert!(
            segments.iter().all(|s| matches!(s, Segment::Text(_))),
            "{segments:?}"
        );
        let text: String = segments
            .iter()
            .map(|s| match s {
                Segment::Text(t) => t.as_str(),
                _ => "",
            })
            .collect::<String>()
            + rest.unwrap().as_deref().unwrap_or("");
        assert!(text.contains("`<tool_call>` text protocol"), "{text}");
        assert!(text.contains("`<tool_result>` blocks"), "{text}");
        assert!(text.ends_with("mention too."), "{text}");
    }

    #[test]
    fn real_call_after_a_mention_and_split_before_body() {
        let (segments, rest) = split_all(&[
            "See `<tool_call>`. Now:\n<tool_call>",
            "\n```json\n{\"name\":\"git_status\"}\n```\n</tool_call>",
        ]);
        assert_eq!(
            segments[0],
            Segment::Text("See `<tool_call>`. Now:\n".into())
        );
        let Segment::ToolCall(raw) = &segments[1] else {
            panic!("expected call: {segments:?}");
        };
        assert_eq!(parse_tool_call(raw).unwrap().0, "git_status");
        assert_eq!(rest, Ok(None));
    }

    #[test]
    fn examples_in_code_blocks_and_placeholder_bodies_are_text() {
        // From a real session: Claude describing the protocol.
        let (segments, rest) = split_all(&[
            "The backend uses <tool_call>{json}</tool_call> blocks.\n```\n",
            "<tool_call>\n{\"name\": \"read_file\", \"input\": {}}\n</tool_call>\n``",
            "`\nDone.",
        ]);
        assert!(
            segments.iter().all(|s| matches!(s, Segment::Text(_))),
            "{segments:?}"
        );
        assert_eq!(rest, Ok(None));
        // After the fence closes, a real call works again.
        let mut sp = ToolCallSplitter::default();
        sp.feed("```\n<tool_call>{}</tool_call>\n```\n");
        let segs = sp.feed("<tool_call>\n{\"name\":\"git_status\"}\n</tool_call>");
        assert!(
            matches!(segs.last(), Some(Segment::ToolCall(_))),
            "{segs:?}"
        );
    }

    #[test]
    fn backtick_from_an_earlier_chunk_still_quotes() {
        // The bug: "as a `" then "<tool_result>`." cut the reply short.
        let (segments, rest) = split_all(&["results come back as a `", "<tool_result>` block."]);
        assert!(
            !segments.contains(&Segment::FabricatedResult),
            "{segments:?}"
        );
        let text: String = segments
            .iter()
            .map(|s| match s {
                Segment::Text(t) => t.as_str(),
                _ => "",
            })
            .collect::<String>()
            + rest.unwrap().as_deref().unwrap_or("");
        assert_eq!(text, "results come back as a `<tool_result>` block.");
    }

    #[tokio::test]
    async fn malformed_calls_go_back_to_the_model() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(8);
        let mut calls = 0;
        let flow = emit_segments(
            &tx,
            vec![Segment::ToolCall("{\"name\": read_file}".into())],
            &mut calls,
            &mut SeenCalls::default(),
        )
        .await;
        assert!(matches!(flow, Flow::Continue));
        assert_eq!(calls, 1);
        let Some(StreamChunk::ToolUse(call)) = rx.recv().await else {
            panic!("expected a tool use");
        };
        assert_eq!(call.name, dcode_ai_common::tool::INVALID_TOOL_CALL);
        let res = crate::tools::ToolRegistry::new().execute(&call).await;
        assert!(!res.success);
        assert!(res.error.unwrap().contains("could not be read"));
    }

    #[test]
    fn native_invoke_calls_run_and_wrappers_vanish() {
        // From a real session: Claude wrote its own function-call markup.
        let (segments, rest) = split_all(&[
            "Checking.\n<function_calls>\n<invoke name=\"read_f",
            "ile\"><parameter name=\"path\">src/a.ts</parameter></invoke>\n<invoke name=\"execute_bash\">",
            "<parameter name=\"command\">cd src && sed -n 1,5p a.ts</parameter><parameter name=\"timeout\">30</parameter></inv",
            "oke>\n</function_calls>",
        ]);
        let calls: Vec<_> = segments
            .iter()
            .filter_map(|s| match s {
                Segment::Invoke(raw) => Some(parse_invoke(raw).unwrap()),
                _ => None,
            })
            .collect();
        assert_eq!(calls.len(), 2, "{segments:?}");
        assert_eq!(
            calls[0],
            ("read_file".to_string(), json!({"path": "src/a.ts"}))
        );
        assert_eq!(
            calls[1],
            (
                "execute_bash".to_string(),
                json!({"command": "cd src && sed -n 1,5p a.ts", "timeout": 30})
            )
        );
        assert_eq!(rest, Ok(None));
        // The namespaced form works the same.
        let ns = |t: &str| {
            t.replace("<invoke", concat!("<", "antml", ":invoke"))
                .replace("</invoke", concat!("</", "antml", ":invoke"))
                .replace("<parameter", concat!("<", "antml", ":parameter"))
                .replace("</parameter", concat!("</", "antml", ":parameter"))
        };
        let (segments, _) = split_all(&[&ns(
            "<invoke name=\"git_diff\"><parameter name=\"staged\">true</parameter></invoke>",
        )]);
        let Some(Segment::Invoke(raw)) = segments.first() else {
            panic!("{segments:?}");
        };
        assert_eq!(
            parse_invoke(raw).unwrap(),
            ("git_diff".to_string(), json!({"staged": true}))
        );
        // A mention in backticks stays text.
        let (segments, _) = split_all(&["Claude's `<invoke name=\"x\">` markup."]);
        assert!(segments.iter().all(|s| matches!(s, Segment::Text(_))));
    }

    #[tokio::test]
    async fn repeated_calls_run_once_and_a_loop_ends_the_reply() {
        let (tx, mut rx) = tokio::sync::mpsc::channel(64);
        let call = || Segment::Invoke("<invoke name=\"git_status\">".into());
        let mut calls = 0;
        let mut seen = SeenCalls::default();
        let flow = emit_segments(
            &tx,
            vec![
                Segment::Text("<function_calls>\n".into()),
                call(),
                call(),
                call(),
                call(),
            ],
            &mut calls,
            &mut seen,
        )
        .await;
        assert!(matches!(flow, Flow::Cut));
        assert_eq!(calls, 1);
        drop(tx);
        let mut uses = 0;
        while let Some(c) = rx.recv().await {
            match c {
                StreamChunk::ToolUse(_) => uses += 1,
                StreamChunk::TextDelta(t) => assert!(!t.contains("function_calls"), "{t}"),
                _ => {}
            }
        }
        assert_eq!(uses, 1);
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
