//! Interactive multiple-choice / custom / suggested-answer questions for the user.

use dcode_ai_common::event::{
    AgentEvent, InteractiveQuestionPayload, QuestionOption, QuestionSelection,
};
use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tokio::sync::oneshot;

use super::ToolExecutor;

const WAIT_SECS: u64 = 300;

/// The answer as the tool result: readable first (it is also what the
/// transcript shows under the question), with option ids for the model.
fn selection_summary(sel: &QuestionSelection, payload: &InteractiveQuestionPayload) -> String {
    let label = |id: &str| {
        payload
            .options
            .iter()
            .find(|o| o.id == id || o.label == id)
            .map(|o| format!("{} (`{}`)", o.label, o.id))
            .unwrap_or_else(|| id.to_string())
    };
    match sel {
        QuestionSelection::Suggested => format!("User chose {}", label(&payload.suggested_answer)),
        QuestionSelection::Option { option_id } => format!("User chose {}", label(option_id)),
        QuestionSelection::Options { option_ids } if option_ids.is_empty() => {
            "User chose none of the options".to_string()
        }
        QuestionSelection::Options { option_ids } => {
            let labels: Vec<String> = option_ids.iter().map(|id| label(id)).collect();
            format!("User chose {}", labels.join(", "))
        }
        QuestionSelection::Custom { text } => format!("User answered: {text}"),
    }
}

/// Tool that blocks until the user answers via CLI or `AgentCommand::AnswerQuestion`.
pub struct AskQuestionTool {
    event_tx: mpsc::Sender<AgentEvent>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<QuestionSelection>>>>,
}

impl AskQuestionTool {
    pub fn new(
        event_tx: mpsc::Sender<AgentEvent>,
        pending: Arc<Mutex<HashMap<String, oneshot::Sender<QuestionSelection>>>>,
    ) -> Self {
        Self { event_tx, pending }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for AskQuestionTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "ask_question".into(),
            description: "Ask the user a structured question with 2-4 choices, an optional \
                custom text answer, and a suggested default (always provide `suggested_answer`, \
                the id of the option you recommend). Give each option a short `description` of \
                what it means, and a 1-2 word `header` for the question. Set `multi_select` when \
                several options can apply. The user can pick, type their own answer, or accept the \
                suggestion. Use this instead of long numbered lists in plain assistant text when \
                you need a fast, reliable answer."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "The question text shown to the user."
                    },
                    "header": {
                        "type": "string",
                        "description": "Very short label for the question (max ~12 chars), e.g. \"Database\"."
                    },
                    "options": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string", "description": "Stable id for this option (e.g. static_site)." },
                                "label": { "type": "string", "description": "Human-readable label (1-5 words)." },
                                "description": { "type": "string", "description": "One line explaining this choice or its trade-off." }
                            },
                            "required": ["id", "label"]
                        },
                        "description": "List of choices; each needs a unique id."
                    },
                    "multi_select": {
                        "type": "boolean",
                        "description": "If true, the user may pick several options. Default false."
                    },
                    "allow_custom": {
                        "type": "boolean",
                        "description": "If true, user can submit freeform text. Default true."
                    },
                    "suggested_answer": {
                        "type": "string",
                        "description": "Your best guess / recommendation; always set this so the user can accept quickly."
                    }
                },
                "required": ["prompt", "options", "suggested_answer"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let prompt = call.input["prompt"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        if prompt.is_empty() {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some("prompt is required".into()),
            };
        }

        let suggested = call.input["suggested_answer"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();
        if suggested.is_empty() {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(
                    "suggested_answer is required (provide your recommended choice)".into(),
                ),
            };
        }

        let allow_custom = call
            .input
            .get("allow_custom")
            .and_then(|v| v.as_bool())
            .unwrap_or(true);

        let options: Vec<QuestionOption> = match call.input.get("options") {
            Some(serde_json::Value::Array(arr)) => {
                let mut out = Vec::new();
                for v in arr {
                    let id = v
                        .get("id")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    let label = v
                        .get("label")
                        .and_then(|x| x.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    if id.is_empty() || label.is_empty() {
                        return ToolResult {
                            call_id: call.id.clone(),
                            success: false,
                            output: String::new(),
                            error: Some("each option needs non-empty id and label".into()),
                        };
                    }
                    let description = v
                        .get("description")
                        .and_then(|x| x.as_str())
                        .map(str::trim)
                        .filter(|d| !d.is_empty())
                        .map(str::to_string);
                    out.push(QuestionOption {
                        id,
                        label,
                        description,
                    });
                }
                out
            }
            _ => {
                return ToolResult {
                    call_id: call.id.clone(),
                    success: false,
                    output: String::new(),
                    error: Some("options must be a non-empty array of {id, label}".into()),
                };
            }
        };

        if options.is_empty() {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some("at least one option is required".into()),
            };
        }

        let question_id = format!("q-{}", call.id);
        let payload = InteractiveQuestionPayload {
            question_id: question_id.clone(),
            call_id: call.id.clone(),
            prompt,
            options,
            allow_custom,
            suggested_answer: suggested,
            header: call.input["header"]
                .as_str()
                .map(str::trim)
                .filter(|h| !h.is_empty())
                .map(|h| h.chars().take(24).collect()),
            multi_select: call.input["multi_select"].as_bool().unwrap_or(false),
        };

        let (tx, rx) = oneshot::channel();
        {
            let mut m = self.pending.lock().unwrap();
            m.insert(question_id.clone(), tx);
        }

        if self
            .event_tx
            .send(AgentEvent::QuestionRequested {
                question: payload.clone(),
            })
            .await
            .is_err()
        {
            let mut m = self.pending.lock().unwrap();
            m.remove(&question_id);
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some("failed to emit QuestionRequested (session ended?)".into()),
            };
        }

        let (selection, timed_out) =
            match tokio::time::timeout(std::time::Duration::from_secs(WAIT_SECS), rx).await {
                Ok(Ok(sel)) => (sel, false),
                Ok(Err(_)) => {
                    let mut m = self.pending.lock().unwrap();
                    m.remove(&question_id);
                    return ToolResult {
                        call_id: call.id.clone(),
                        success: false,
                        output: String::new(),
                        error: Some(
                            "question answer channel closed unexpectedly; use IPC AnswerQuestion \
                         or an interactive terminal"
                                .into(),
                        ),
                    };
                }
                Err(_) => {
                    let mut m = self.pending.lock().unwrap();
                    m.remove(&question_id);
                    (QuestionSelection::Suggested, true)
                }
            };

        let summary = if timed_out {
            format!(
                "No answer received within {WAIT_SECS}s; auto-selected suggested answer: {}",
                payload.suggested_answer
            )
        } else {
            selection_summary(&selection, &payload)
        };
        let _ = self
            .event_tx
            .send(AgentEvent::QuestionResolved {
                question_id: question_id.clone(),
                selection: selection.clone(),
            })
            .await;

        ToolResult {
            call_id: call.id.clone(),
            success: true,
            output: summary,
            error: None,
        }
    }
}
