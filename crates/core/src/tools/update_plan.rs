use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};

use super::ToolExecutor;

/// Agent-driven task plan (Codex `update_plan`). The model calls this to create
/// and maintain a step-by-step plan; the formatted checklist is returned as the
/// tool output and rendered in the transcript.
pub struct UpdatePlanTool;

impl UpdatePlanTool {
    pub fn new() -> Self {
        Self
    }
}

impl Default for UpdatePlanTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ToolExecutor for UpdatePlanTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "update_plan".into(),
            description: "Create or update a concise step-by-step plan for the current task. \
Call this at the start of any multi-step task and again whenever a step's status changes \
(e.g. when you finish a step). Keep steps short and outcome-focused. Each step has a \
`step` description and a `status` of `pending`, `in_progress`, or `completed`. Exactly one \
step should be `in_progress` at a time."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "steps": {
                        "type": "array",
                        "description": "The full ordered list of plan steps (send the whole list each update).",
                        "items": {
                            "type": "object",
                            "properties": {
                                "step": { "type": "string" },
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"]
                                }
                            },
                            "required": ["step", "status"]
                        }
                    }
                },
                "required": ["steps"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let Some(steps) = call.input.get("steps").and_then(|v| v.as_array()) else {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some("update_plan requires a 'steps' array".into()),
            };
        };

        let total = steps.len();
        let mut done = 0usize;
        let mut out = String::new();
        for s in steps {
            let step = s.get("step").and_then(|v| v.as_str()).unwrap_or("").trim();
            let status = s
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("pending");
            let marker = match status {
                "completed" | "done" => {
                    done += 1;
                    "[x]"
                }
                "in_progress" | "active" => "[~]",
                _ => "[ ]",
            };
            out.push_str(marker);
            out.push(' ');
            out.push_str(step);
            out.push('\n');
        }
        out.push_str(&format!("\n{done}/{total} complete"));

        ToolResult {
            call_id: call.id.clone(),
            success: true,
            output: out,
            error: None,
        }
    }
}
