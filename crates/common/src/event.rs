use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::tool::ToolResult;

/// Stable event envelope schema version used across disk logs, IPC, and stdout NDJSON.
pub const EVENT_ENVELOPE_SCHEMA_VERSION: u32 = 1;

/// Real-time busy state indicator for CLI rendering.
/// Reflects the current processing state of the agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BusyState {
    /// Agent is idle and ready for input.
    Idle,
    /// Agent is thinking/planning (heavy computation, model reasoning).
    Thinking,
    /// Agent is streaming tokens from LLM.
    Streaming,
    /// Agent is executing a tool.
    ToolRunning,
    /// Agent is waiting for user approval on a tool call.
    ApprovalPending,
    /// Error or blocked state.
    Error,
}

impl BusyState {
    /// Human-readable label for this state.
    pub fn label(&self) -> &'static str {
        match self {
            BusyState::Idle => "idle",
            BusyState::Thinking => "thinking",
            BusyState::Streaming => "streaming",
            BusyState::ToolRunning => "tool",
            BusyState::ApprovalPending => "your approval needed",
            BusyState::Error => "error",
        }
    }

    /// Whether the agent is actively working (drives the "working" status and
    /// spinner). Both `Idle` and terminal `Error` are inactive: after an error
    /// the turn is over, so the UI must read idle — not stay stuck on "working".
    pub fn is_active(&self) -> bool {
        !matches!(self, BusyState::Idle | BusyState::Error)
    }
}

/// Envelope for events written to disk, with stable id and timestamp for ordering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    #[serde(default = "default_event_envelope_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub id: u64,
    #[serde(default)]
    pub ts: Option<DateTime<Utc>>,
    pub event: AgentEvent,
}

impl EventEnvelope {
    pub fn new(id: u64, event: AgentEvent) -> Self {
        Self {
            schema_version: EVENT_ENVELOPE_SCHEMA_VERSION,
            id,
            ts: Some(Utc::now()),
            event,
        }
    }
}

fn default_event_envelope_schema_version() -> u32 {
    EVENT_ENVELOPE_SCHEMA_VERSION
}

/// Lifecycle of a background shell job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackgroundJobStatus {
    Running,
    Completed,
    Failed,
    Stopped,
}

impl BackgroundJobStatus {
    pub fn is_running(self) -> bool {
        self == Self::Running
    }
}

/// Events emitted by the agent runtime, broadcast over IPC to the CLI.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentEvent {
    SessionStarted {
        session_id: String,
        workspace: PathBuf,
        model: String,
    },
    MessageReceived {
        role: String,
        content: String,
    },
    TokensStreamed {
        delta: String,
    },
    ThinkingDelta {
        delta: String,
    },
    ToolCallStarted {
        call_id: String,
        tool: String,
        input: serde_json::Value,
    },
    ToolCallCompleted {
        call_id: String,
        output: ToolResult,
    },
    /// Incremental output from a long-running tool (bash, etc.).
    /// The TUI appends this to the ToolRunning block for live feedback.
    ToolOutputDelta {
        call_id: String,
        delta: String,
    },
    /// A `run_background` job started or ended. On end, `output_tail` holds
    /// the last lines of its output.
    BackgroundJob {
        job_id: u32,
        command: String,
        status: BackgroundJobStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
        #[serde(default, skip_serializing_if = "String::is_empty")]
        output_tail: String,
    },
    ApprovalRequested {
        call_id: String,
        tool: String,
        description: String,
    },
    ApprovalResolved {
        call_id: String,
        approved: bool,
    },
    CostUpdated {
        input_tokens: u64,
        output_tokens: u64,
        estimated_cost_usd: f64,
        /// Estimated size of the live conversation sent on the most recent
        /// request (current context-window occupancy), for the ctx gauge.
        context_tokens: u64,
    },
    Checkpoint {
        phase: String,
        detail: String,
        turn: u32,
    },
    SessionEnded {
        reason: EndReason,
    },
    Error {
        message: String,
    },
    /// Something worth a line in the transcript that is not an error, e.g.
    /// auto mode approving a command after its safety check.
    Notice {
        message: String,
    },
    /// Someone is with this session: on `dcode-ai join` (`role: "join"`),
    /// on its `/watch` page (`"watch"`), or the one running it (`"owner"`).
    PersonJoined {
        person: String,
        name: String,
        role: String,
    },
    /// Still there (heartbeat; not kept in the session log).
    PersonSeen {
        person: String,
    },
    PersonLeft {
        person: String,
        #[serde(default)]
        name: String,
    },
    /// A joined person sent a message.
    PersonSaid {
        person: String,
        name: String,
        text: String,
    },
    Response {
        response: AgentResponse,
    },
    ChildSessionSpawned {
        parent_session_id: String,
        child_session_id: String,
        task: String,
        workspace: PathBuf,
        branch: Option<String>,
    },
    ChildSessionCompleted {
        parent_session_id: String,
        child_session_id: String,
        status: String,
    },
    /// A sub-agent left work on its own worktree branch; the UI asks
    /// whether to merge it, keep the branch or discard it.
    SubagentChanges {
        child_session_id: String,
        task: String,
        branch: String,
        base_branch: String,
        worktree_path: PathBuf,
        /// `M path` / `A path` lines.
        files: Vec<String>,
        insertions: usize,
        deletions: usize,
    },
    /// A sub-agent started in the background has settled; `output` is its
    /// final report (the parent is notified with it).
    BackgroundAgentFinished {
        agent_id: String,
        task: String,
        status: String,
        output: String,
    },
    /// Live activity from a child session (tools, checkpoints, nested spawns), for parent UI.
    ChildSessionActivity {
        child_session_id: String,
        /// Short label, e.g. tool name or checkpoint phase.
        phase: String,
        /// One-line detail for the sidebar/transcript.
        detail: String,
    },
    /// User must pick an option, use the suggested answer, or enter custom text.
    /// Emitted when the `ask_question` tool runs; answer via `AgentCommand::AnswerQuestion` or local CLI.
    QuestionRequested {
        question: InteractiveQuestionPayload,
    },
    /// Logged after the user (or orchestrator) answers a question.
    QuestionResolved {
        question_id: String,
        selection: QuestionSelection,
    },
    /// Warning that context is approaching limit.
    ContextWarning {
        message: String,
    },
    /// Context compaction/summarization event.
    /// `/rewind` dropped the last `user_messages_dropped` user messages and
    /// everything after them. Logged so a resumed transcript matches the
    /// rewound conversation.
    ConversationRewound {
        user_messages_dropped: usize,
    },
    ContextCompaction {
        phase: String,
        message: String,
        /// Current estimated context tokens at this point, so the UI gauge can
        /// reflect the post-compaction size immediately (not just on next turn).
        #[serde(default)]
        context_tokens: u64,
    },
    /// MCP server startup progress.
    McpStartupUpdate {
        server: String,
        status: McpStartupStatus,
    },
    /// Busy state transition (for animated indicator rendering).
    BusyStateChanged {
        state: BusyState,
    },
    /// A `run_preview` job (dev server / static preview) was started.
    PreviewStarted {
        job_id: u32,
        command: String,
    },
    /// A line of output from a running preview job.
    PreviewLog {
        job_id: u32,
        delta: String,
    },
    /// The preview job's listening URL was detected from its output.
    PreviewUrlDetected {
        job_id: u32,
        url: String,
    },
    /// A preview job exited or was stopped.
    PreviewStopped {
        job_id: u32,
        reason: String,
    },
}

/// One selectable row shown for an interactive question.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuestionOption {
    pub id: String,
    pub label: String,
    /// One short line under the label explaining the choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Full question payload broadcast on the event bus and shown in the CLI.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct InteractiveQuestionPayload {
    pub question_id: String,
    pub call_id: String,
    pub prompt: String,
    pub options: Vec<QuestionOption>,
    #[serde(default = "default_allow_custom")]
    pub allow_custom: bool,
    /// Model-provided default; user can accept via suggested / `/auto-answer`.
    pub suggested_answer: String,
    /// Short chip shown as the prompt's title (e.g. "Database").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    /// The user may pick several options (answered with `Options`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub multi_select: bool,
}

fn default_allow_custom() -> bool {
    true
}

/// How the user answered an interactive question.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QuestionSelection {
    Option {
        option_id: String,
    },
    /// Several options picked in a multi-select question.
    Options {
        option_ids: Vec<String>,
    },
    Custom {
        text: String,
    },
    Suggested,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum McpStartupStatus {
    Starting,
    Initializing,
    Ready,
    Failed { message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EndReason {
    UserExit,
    Completed,
    Error,
    Cancelled,
}

/// Commands sent over IPC to the agent runtime.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentCommand {
    SendMessage {
        content: String,
    },
    /// `dcode-ai join` introduces itself; `Heartbeat` every few seconds,
    /// `Bye` when it leaves.
    Hello {
        person: String,
        name: String,
    },
    Heartbeat {
        person: String,
    },
    Bye {
        person: String,
    },
    /// A message from a joined person (shown with their name).
    Say {
        person: String,
        content: String,
    },
    ApproveToolCall {
        call_id: String,
    },
    DenyToolCall {
        call_id: String,
    },
    /// Submit an answer for the pending `ask_question` with matching `question_id`.
    AnswerQuestion {
        question_id: String,
        selection: QuestionSelection,
    },
    Cancel,
    Shutdown,
}

/// Responses to query-style IPC messages, sent back as `AgentEvent::Response`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentResponse {
    SessionState {
        session: Box<crate::session::SessionState>,
    },
    SessionList {
        sessions: Vec<crate::session::SessionMeta>,
    },
    Error {
        message: String,
    },
    Ok,
}

#[cfg(test)]
mod interactive_question_serde_tests {
    use super::*;

    #[test]
    fn question_requested_roundtrip() {
        let q = InteractiveQuestionPayload {
            question_id: "q-1".into(),
            call_id: "call_1".into(),
            prompt: "Pick one".into(),
            options: vec![
                QuestionOption {
                    id: "a".into(),
                    label: "Alpha".into(),
                    description: None,
                },
                QuestionOption {
                    id: "b".into(),
                    label: "Beta".into(),
                    description: None,
                },
            ],
            allow_custom: true,
            suggested_answer: "Alpha".into(),
            ..Default::default()
        };
        let ev = AgentEvent::QuestionRequested {
            question: q.clone(),
        };
        let json = serde_json::to_string(&ev).expect("serialize");
        let back: AgentEvent = serde_json::from_str(&json).expect("deserialize");
        match back {
            AgentEvent::QuestionRequested { question } => assert_eq!(question, q),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn answer_question_command_roundtrip() {
        let cmd = AgentCommand::AnswerQuestion {
            question_id: "q-1".into(),
            selection: QuestionSelection::Suggested,
        };
        let json = serde_json::to_string(&cmd).expect("serialize");
        let back: AgentCommand = serde_json::from_str(&json).expect("deserialize");
        match back {
            AgentCommand::AnswerQuestion {
                question_id,
                selection,
            } => {
                assert_eq!(question_id, "q-1");
                assert!(matches!(selection, QuestionSelection::Suggested));
            }
            _ => panic!("wrong variant"),
        }
    }
}
