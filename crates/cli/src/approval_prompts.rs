//! Interactive CLI approval prompts using cli-prompts library.
//!
//! Provides enhanced approval dialogs with:
//! - Rich tool descriptions
//! - Formatted JSON input display
//! - Confirmation prompts with help text
//! - Multi-select for batch approvals

use cli_prompts::{
    DisplayPrompt,
    prompts::{AbortReason, Confirmation},
};
use dcode_ai_common::tool::ToolCall;
use dcode_ai_core::approval::{ApprovalHandler, ApprovalVerdict};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex as AsyncMutex;

/// Pretty-print JSON with indentation for readability
pub fn format_json_pretty(value: &Value) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(value.as_str().unwrap_or("")) {
        serde_json::to_string_pretty(&v).unwrap_or_else(|_| value.to_string())
    } else {
        serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
    }
}

/// Truncate long strings with ellipsis
pub fn truncate(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len.saturating_sub(3)])
    }
}

/// Interactive approval handler using cli-prompts library.
/// Provides rich TUI prompts for tool approval.
#[allow(dead_code)]
pub struct InteractiveApprovalHandler {
    prompt_lock: AsyncMutex<()>,
    show_full_json: bool,
}

impl InteractiveApprovalHandler {
    #[allow(dead_code)]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            prompt_lock: AsyncMutex::new(()),
            show_full_json: false,
        })
    }

    #[allow(dead_code)]
    pub fn with_full_json(mut self, show: bool) -> Self {
        self.show_full_json = show;
        self
    }

    #[allow(dead_code)]
    fn prompt_approval(&self, call: &ToolCall, description: &str) -> Option<bool> {
        let tool_name = &call.name;
        let input_preview = truncate(&format_json_pretty(&call.input), 150);

        let prompt_msg = if description.is_empty() {
            format!(
                "Tool '{}' wants to execute:\n\nInput preview:\n{}",
                tool_name, input_preview
            )
        } else {
            format!(
                "{}\n\nTool: {}\nInput preview:\n{}",
                description, tool_name, input_preview
            )
        };

        let confirmed = Confirmation::new(&prompt_msg)
            .default_positive(false)
            .display();

        match confirmed {
            Ok(true) => Some(true),
            Ok(false) => Some(false),
            Err(AbortReason::Interrupt) | Err(AbortReason::Error(_)) => None,
        }
    }

    #[allow(dead_code)]
    fn show_tool_details(&self, call: &ToolCall) {
        println!("\n╭─────────────────────────────────────────────────────────────╮");
        println!(
            "│ Tool: {}                                                   ",
            call.name
        );
        println!("├─────────────────────────────────────────────────────────────┤");

        let json_str = format_json_pretty(&call.input);
        for line in json_str.lines() {
            println!("│ {} │", truncate(line, 59));
        }

        println!("╰─────────────────────────────────────────────────────────────╯\n");
    }
}

impl Default for InteractiveApprovalHandler {
    fn default() -> Self {
        Self {
            prompt_lock: AsyncMutex::new(()),
            show_full_json: false,
        }
    }
}

#[async_trait::async_trait]
impl ApprovalHandler for InteractiveApprovalHandler {
    async fn resolve(&self, call: &ToolCall, description: &str) -> ApprovalVerdict {
        let _guard = self.prompt_lock.lock().await;
        match self.prompt_approval(call, description) {
            Some(true) => ApprovalVerdict::Approved,
            _ => ApprovalVerdict::Denied,
        }
    }
}

/// Interactive IPC approval handler that uses rich prompts
/// with fallback to legacy stdio if IPC times out.
pub struct InteractiveIpcApprovalHandler {
    pending: tokio::sync::Mutex<HashMap<String, tokio::sync::oneshot::Sender<bool>>>,
    prompt_lock: AsyncMutex<()>,
}

impl InteractiveIpcApprovalHandler {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            pending: tokio::sync::Mutex::new(HashMap::new()),
            prompt_lock: AsyncMutex::new(()),
        })
    }

    fn prompt_approval(&self, call: &ToolCall, description: &str) -> Option<bool> {
        let tool_name = &call.name;
        let input_preview = truncate(&format_json_pretty(&call.input), 150);

        let prompt_msg = if description.is_empty() {
            format!(
                "Tool '{}' wants to execute:\n\nInput preview:\n{}",
                tool_name, input_preview
            )
        } else {
            format!(
                "{}\n\nTool: {}\nInput preview:\n{}",
                description, tool_name, input_preview
            )
        };

        let confirmed = Confirmation::new(&prompt_msg)
            .default_positive(false)
            .display();

        match confirmed {
            Ok(true) => Some(true),
            Ok(false) => Some(false),
            Err(AbortReason::Interrupt) | Err(AbortReason::Error(_)) => None,
        }
    }
}

impl Default for InteractiveIpcApprovalHandler {
    fn default() -> Self {
        Self {
            pending: tokio::sync::Mutex::new(HashMap::new()),
            prompt_lock: AsyncMutex::new(()),
        }
    }
}

#[async_trait::async_trait]
impl ApprovalHandler for InteractiveIpcApprovalHandler {
    async fn resolve(&self, call: &ToolCall, description: &str) -> ApprovalVerdict {
        let (tx, rx) = tokio::sync::oneshot::channel();
        {
            let mut m = self.pending.lock().await;
            m.insert(call.id.clone(), tx);
        }

        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(approved)) => {
                let mut m = self.pending.lock().await;
                m.remove(&call.id);
                if approved {
                    ApprovalVerdict::Approved
                } else {
                    ApprovalVerdict::Denied
                }
            }
            _ => {
                let mut m = self.pending.lock().await;
                m.remove(&call.id);
                drop(m);

                let _guard = self.prompt_lock.lock().await;
                match self.prompt_approval(call, description) {
                    Some(true) => ApprovalVerdict::Approved,
                    _ => ApprovalVerdict::Denied,
                }
            }
        }
    }
}

/// Legacy stdio handler for backward compatibility
pub mod legacy {
    use super::*;
    use tokio::io::{self, AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[allow(dead_code)]
    pub struct StdioApprovalHandler {
        prompt_lock: AsyncMutex<()>,
    }

    impl StdioApprovalHandler {
        #[allow(dead_code)]
        pub fn new() -> Arc<Self> {
            Arc::new(Self {
                prompt_lock: AsyncMutex::new(()),
            })
        }
    }

    #[async_trait::async_trait]
    impl ApprovalHandler for StdioApprovalHandler {
        async fn resolve(&self, call: &ToolCall, description: &str) -> ApprovalVerdict {
            let _guard = self.prompt_lock.lock().await;
            let mut stderr = io::stderr();
            let stdin = io::stdin();
            let mut reader = BufReader::new(stdin);

            let prompt = format!(
                "\n[approval] {description}\nTool: {}\nInput preview:\n{}\nApprove? [y/N]: ",
                call.name,
                truncate(&format_json_pretty(&call.input), 200)
            );
            if stderr.write_all(prompt.as_bytes()).await.is_err() {
                return ApprovalVerdict::Denied;
            }
            if stderr.flush().await.is_err() {
                return ApprovalVerdict::Denied;
            }

            let mut answer = String::new();
            if reader.read_line(&mut answer).await.is_err() {
                return ApprovalVerdict::Denied;
            }

            if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                ApprovalVerdict::Approved
            } else {
                ApprovalVerdict::Denied
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_truncate_short() {
        assert_eq!(truncate("hello", 10), "hello");
    }

    #[test]
    fn test_truncate_long() {
        assert_eq!(truncate("hello world", 8), "hello...");
    }

    #[test]
    fn test_truncate_exact() {
        assert_eq!(truncate("hello", 5), "hello");
    }

    #[test]
    fn test_format_json_pretty_valid() {
        let json = serde_json::json!({"key": "value"});
        let formatted = format_json_pretty(&json);
        assert!(formatted.contains("key"));
        assert!(formatted.contains("value"));
    }
}
