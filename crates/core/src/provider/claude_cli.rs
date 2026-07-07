use std::path::Path;

use dcode_ai_common::config::{AnthropicConfig, DcodeAiConfig};
use dcode_ai_common::message::{Message, Role};
use dcode_ai_common::provider_runtime::has_claude_cli;
use dcode_ai_common::tool::ToolDefinition;

use super::{Provider, ProviderCapabilities, ProviderError, StreamChunk};

/// Anthropic-compatible provider backed by local `claude` CLI.
///
/// This bridge is intended for users with Claude Code subscription access
/// who do not want to rely on Anthropic API-key credits.
pub struct ClaudeCliProvider {
    config: AnthropicConfig,
}

impl ClaudeCliProvider {
    pub fn from_config(config: &DcodeAiConfig) -> Result<Self, ProviderError> {
        if !has_claude_cli() {
            return Err(ProviderError::Configuration(
                "Anthropic fallback requires local `claude` CLI in PATH. Install Claude Code CLI and run `claude auth login`.".to_string(),
            ));
        }
        Ok(Self {
            config: config.provider.anthropic.clone(),
        })
    }
}

#[async_trait::async_trait]
impl Provider for ClaudeCliProvider {
    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            // Bridge mode does not expose native image or tool-call streaming.
            supports_thinking_stream: false,
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
        let selected_model = if model.is_empty() {
            self.config.model.clone()
        } else {
            model.to_string()
        };
        let prompt = build_claude_bridge_prompt(messages, tools);
        let normalized_model = normalize_claude_cli_model_id(&selected_model);
        let model_arg = if should_pass_model_to_claude_cli(&normalized_model) {
            Some(normalized_model.as_str())
        } else {
            None
        };
        let first_attempt = run_claude_once(&prompt, workspace_root, model_arg).await;
        let (result_text, input_tokens, output_tokens) = match first_attempt {
            Ok(ok) => ok,
            Err(err)
                if model_arg.is_some() && should_retry_without_model(&err, &normalized_model) =>
            {
                run_claude_once(&prompt, workspace_root, None).await?
            }
            Err(err) => return Err(err),
        };

        let (tx, rx) = tokio::sync::mpsc::channel(8);
        tokio::spawn(async move {
            let _ = tx.send(StreamChunk::TextDelta(result_text)).await;
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

        Ok(rx)
    }
}

async fn run_claude_once(
    prompt: &str,
    workspace_root: &Path,
    model: Option<&str>,
) -> Result<(String, u64, u64), ProviderError> {
    // `claude` is an npm .cmd shim on Windows; route through the compat helper.
    let mut cmd = dcode_ai_common::provider_runtime::windows_compat_command_tokio("claude");
    cmd.arg("-p")
        .arg("--output-format")
        .arg("json")
        .arg("--add-dir")
        .arg(workspace_root)
        .arg("--no-session-persistence");
    if let Some(model) = model {
        cmd.arg("--model").arg(model);
    }
    // Feed the prompt via stdin, not argv: a long conversation easily exceeds
    // the OS argument-size limit, which fails as E2BIG / "Argument list too
    // long" (os error 7). `claude -p` reads the prompt from stdin when piped,
    // and stdin has no such size cap.
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|err| {
        ProviderError::Configuration(format!(
            "failed to execute `claude` CLI bridge: {err}. Ensure `claude` is installed and in PATH."
        ))
    })?;

    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        stdin.write_all(prompt.as_bytes()).await.map_err(|err| {
            ProviderError::Configuration(format!(
                "failed to send prompt to `claude` CLI bridge: {err}"
            ))
        })?;
        // Close stdin so `claude` sees EOF and starts processing.
        drop(stdin);
    }

    let output = child.wait_with_output().await.map_err(|err| {
        ProviderError::Configuration(format!(
            "failed to run `claude` CLI bridge: {err}. Ensure `claude` is installed and in PATH."
        ))
    })?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if let Some(parsed) = parse_claude_result_json(&stdout) {
        if parsed.is_error {
            let message = if parsed.text.is_empty() {
                if stderr.is_empty() {
                    "Claude CLI returned structured error with no details".to_string()
                } else {
                    stderr.clone()
                }
            } else {
                parsed.text.clone()
            };
            return Err(map_claude_cli_message(&message));
        }
        if !parsed.text.trim().is_empty() {
            return Ok((parsed.text, parsed.input_tokens, parsed.output_tokens));
        }
    }

    if output.status.success() {
        if !stdout.trim().is_empty() {
            return Ok((stdout, 0, 0));
        }
        return Err(ProviderError::RequestFailed(
            "Claude CLI bridge returned empty response".to_string(),
        ));
    }

    Err(map_claude_cli_failure(
        output.status.code(),
        &output.stderr,
        &output.stdout,
    ))
}

fn map_claude_cli_failure(code: Option<i32>, stderr: &[u8], stdout: &[u8]) -> ProviderError {
    let stderr = String::from_utf8_lossy(stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(stdout).trim().to_string();
    let combined = format!("{stderr}\n{stdout}").to_ascii_lowercase();

    if combined.contains("authentication")
        || combined.contains("not logged in")
        || combined.contains("auth login")
    {
        return ProviderError::AuthError(
            "Claude CLI authentication failed. Run `claude auth login` and retry.".to_string(),
        );
    }
    if combined.contains("rate limit")
        || combined.contains("billing")
        || combined.contains("quota")
        || combined.contains("credit")
    {
        return ProviderError::RateLimited {
            retry_after_ms: 1000,
        };
    }

    let details = if stderr.is_empty() && stdout.is_empty() {
        format!("exit code {}", code.unwrap_or(-1))
    } else {
        format!("exit code {}: {} {}", code.unwrap_or(-1), stderr, stdout)
    };
    ProviderError::RequestFailed(format!("Claude CLI bridge failed: {details}"))
}

fn map_claude_cli_message(message: &str) -> ProviderError {
    let lower = message.to_ascii_lowercase();
    if lower.contains("authentication")
        || lower.contains("not logged in")
        || lower.contains("auth login")
        || lower.contains("unauthorized")
    {
        return ProviderError::AuthError(
            "Claude CLI authentication failed. Run `claude auth login` and retry.".to_string(),
        );
    }
    if lower.contains("rate limit")
        || lower.contains("billing")
        || lower.contains("quota")
        || lower.contains("credit")
    {
        return ProviderError::RateLimited {
            retry_after_ms: 1000,
        };
    }
    if lower.contains("model")
        && (lower.contains("not found")
            || lower.contains("unsupported")
            || lower.contains("invalid"))
    {
        return ProviderError::ModelNotFound(message.to_string());
    }
    ProviderError::RequestFailed(format!("Claude CLI bridge error: {message}"))
}

#[derive(Debug, Clone)]
struct ClaudeParsedResult {
    text: String,
    input_tokens: u64,
    output_tokens: u64,
    is_error: bool,
}

fn parse_claude_result_json(stdout: &str) -> Option<ClaudeParsedResult> {
    let value = serde_json::from_str::<serde_json::Value>(stdout).ok()?;
    let text = value
        .get("result")
        .and_then(|v| v.as_str())
        .or_else(|| value.get("output").and_then(|v| v.as_str()))
        .unwrap_or_default()
        .to_string();

    let input_tokens = value
        .get("usage")
        .and_then(|u| u.get("input_tokens"))
        .and_then(|v| v.as_u64())
        .or_else(|| {
            value
                .get("modelUsage")
                .and_then(|u| u.get("inputTokens"))
                .and_then(|v| v.as_u64())
        })
        .unwrap_or(0);
    let output_tokens = value
        .get("usage")
        .and_then(|u| u.get("output_tokens"))
        .and_then(|v| v.as_u64())
        .or_else(|| {
            value
                .get("modelUsage")
                .and_then(|u| u.get("outputTokens"))
                .and_then(|v| v.as_u64())
        })
        .unwrap_or(0);
    let is_error = value
        .get("is_error")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Some(ClaudeParsedResult {
        text,
        input_tokens,
        output_tokens,
        is_error,
    })
}

fn should_pass_model_to_claude_cli(model: &str) -> bool {
    let m = model.trim().to_ascii_lowercase();
    matches!(m.as_str(), "sonnet" | "opus" | "haiku")
        || m.starts_with("claude-sonnet-")
        || m.starts_with("claude-opus-")
        || m.starts_with("claude-haiku-")
}

fn should_retry_without_model(err: &ProviderError, selected_model: &str) -> bool {
    if !should_pass_model_to_claude_cli(selected_model) {
        return false;
    }
    match err {
        ProviderError::ModelNotFound(message)
        | ProviderError::RequestFailed(message)
        | ProviderError::AuthError(message) => {
            let m = message.to_ascii_lowercase();
            m.contains("model")
                && (m.contains("not found")
                    || m.contains("unsupported")
                    || m.contains("invalid")
                    || m.contains("selected model")
                    || m.contains("may not exist")
                    || m.contains("pick a different model")
                    || m.contains("you may not have access"))
        }
        _ => false,
    }
}

fn normalize_claude_cli_model_id(model: &str) -> String {
    let m = model.trim();
    if m.is_empty() {
        return String::new();
    }
    // Claude CLI commonly expects hyphenated version segments (e.g. 4-6 instead of 4.6).
    if m.starts_with("claude-") && m.contains('.') {
        return m.replace('.', "-");
    }
    m.to_string()
}

fn build_claude_bridge_prompt(messages: &[Message], tools: &[ToolDefinition]) -> String {
    let mut out = String::new();
    out.push_str("You are running via dcode-ai Claude CLI bridge.\n");
    out.push_str("Reply with plain assistant text only.\n");
    if !tools.is_empty() {
        out.push_str(
            "Note: dcode-ai function/tool-calling is disabled in this bridge mode. Do not emit tool call JSON.\n",
        );
    }
    out.push_str("\nConversation:\n");

    for message in messages {
        let role = match message.role {
            Role::System => "system",
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };
        let content = message.content.to_summary_text();
        if !content.trim().is_empty() {
            out.push_str(&format!("[{role}] {content}\n"));
        }
        if let Some(tool_calls) = &message.tool_calls {
            for call in tool_calls {
                out.push_str(&format!(
                    "[assistant_tool_call] {} {} {}\n",
                    call.id, call.name, call.arguments
                ));
            }
        }
        if let Some(tool_call_id) = &message.tool_call_id {
            out.push_str(&format!("[tool_call_id] {tool_call_id}\n"));
        }
    }
    out.push_str("\nProvide the next assistant response for this conversation.");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_claude_result_json_extracts_text_and_usage() {
        let raw = r#"{"type":"result","subtype":"success","result":"hello","usage":{"input_tokens":12,"output_tokens":34}}"#;
        let parsed = parse_claude_result_json(raw).expect("json parsed");
        assert_eq!(parsed.text, "hello");
        assert_eq!(parsed.input_tokens, 12);
        assert_eq!(parsed.output_tokens, 34);
        assert!(!parsed.is_error);
    }

    #[test]
    fn parse_claude_result_json_falls_back_to_plain_text() {
        let raw = "plain text output";
        assert!(parse_claude_result_json(raw).is_none());
    }

    #[test]
    fn parse_claude_result_json_handles_structured_error() {
        let raw =
            r#"{"type":"result","subtype":"success","is_error":true,"result":"model not found"}"#;
        let parsed = parse_claude_result_json(raw).expect("json parsed");
        assert!(parsed.is_error);
        assert_eq!(parsed.text, "model not found");
    }

    #[test]
    fn should_pass_only_supported_claude_cli_model_ids() {
        assert!(should_pass_model_to_claude_cli("sonnet"));
        assert!(should_pass_model_to_claude_cli("claude-sonnet-4-6"));
        assert!(!should_pass_model_to_claude_cli("claude-3-7-sonnet-latest"));
    }

    #[test]
    fn normalizes_dotted_claude_cli_model_ids() {
        assert_eq!(
            normalize_claude_cli_model_id("claude-sonnet-4.6"),
            "claude-sonnet-4-6"
        );
        assert_eq!(
            normalize_claude_cli_model_id("claude-opus-4.1"),
            "claude-opus-4-1"
        );
    }
}
