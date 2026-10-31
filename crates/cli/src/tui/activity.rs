//! What the agent did, for the eye: the files it touched (dashboard
//! `files` tab, `/files`) and the session as steps (`/timeline`). Both are
//! built from the event stream, so a resumed session gets them back from
//! its log.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use dcode_ai_common::event::AgentEvent;
use serde_json::Value;

/// How the agent used a file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileTouch {
    pub reads: u32,
    pub edits: u32,
    /// Last tool that touched it, and when (for the pulse and "2m ago").
    pub last_tool: String,
    pub last_at: Option<Instant>,
}

/// Every file touched this session, by workspace-relative path.
#[derive(Debug, Default)]
pub struct FileMap {
    pub files: BTreeMap<String, FileTouch>,
    /// The file of the call that is running now.
    pub current: Option<String>,
}

const EDIT_TOOLS: [&str; 8] = [
    "write_file",
    "edit_file",
    "apply_patch",
    "replace_match",
    "notebook_edit",
    "delete_path",
    "move_path",
    "rename_path",
];
const READ_TOOLS: [&str; 5] = [
    "read_file",
    "list_directory",
    "inspect_binary",
    "query_symbols",
    "copy_path",
];

/// The file a tool call is about, if any.
pub fn call_path(tool: &str, input: &Value) -> Option<String> {
    if !EDIT_TOOLS.contains(&tool) && !READ_TOOLS.contains(&tool) {
        return None;
    }
    [
        "path",
        "file_path",
        "notebook_path",
        "from",
        "source",
        "file",
    ]
    .iter()
    .find_map(|k| input[*k].as_str())
    .map(|p| p.trim_start_matches("./").to_string())
    .filter(|p| !p.is_empty())
}

impl FileMap {
    pub fn touch(&mut self, tool: &str, input: &Value) {
        let Some(path) = call_path(tool, input) else {
            return;
        };
        let entry = self.files.entry(path.clone()).or_default();
        if EDIT_TOOLS.contains(&tool) {
            entry.edits += 1;
        } else {
            entry.reads += 1;
        }
        entry.last_tool = tool.to_string();
        entry.last_at = Some(Instant::now());
        self.current = Some(path);
    }

    pub fn edited(&self) -> usize {
        self.files.values().filter(|f| f.edits > 0).count()
    }

    /// Lines for a tree: `(depth, name, touch)`; folders have `None`.
    pub fn tree(&self) -> Vec<(usize, String, Option<&FileTouch>)> {
        let mut out: Vec<(usize, String, Option<&FileTouch>)> = Vec::new();
        let mut open: Vec<String> = Vec::new();
        for (path, touch) in &self.files {
            let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
            let (dirs, name) = parts.split_at(parts.len().saturating_sub(1));
            // Close folders that this path is not in.
            let common = open
                .iter()
                .zip(dirs.iter())
                .take_while(|(a, b)| a.as_str() == **b)
                .count();
            open.truncate(common);
            for d in &dirs[common..] {
                out.push((open.len(), format!("{d}/"), None));
                open.push((*d).to_string());
            }
            if let Some(n) = name.first() {
                out.push((open.len(), (*n).to_string(), Some(touch)));
            }
        }
        out
    }
}

/// "just now", "45s", "3m", "2h".
pub fn ago(at: Option<Instant>) -> String {
    let Some(at) = at else {
        return String::new();
    };
    let s = at.elapsed().as_secs();
    match s {
        0..=4 => "now".into(),
        5..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        _ => format!("{}h", s / 3600),
    }
}

/// Whether a file was touched within the last few seconds (it pulses).
pub fn fresh(touch: &FileTouch) -> bool {
    touch
        .last_at
        .is_some_and(|t| t.elapsed() < Duration::from_secs(4))
}

/// One step of the session, for `/timeline`.
#[derive(Debug, Clone, PartialEq)]
pub struct Step {
    pub kind: StepKind,
    /// One line: the message, `tool target`, the reply's first line.
    pub label: String,
    /// More: full message, tool input, output excerpt.
    pub detail: String,
    pub at: chrono::DateTime<chrono::Local>,
    /// For your messages: how many of your messages come after it (what
    /// rewind needs).
    pub user_index: Option<usize>,
    pub ok: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    User,
    Tool,
    Reply,
    Compaction,
    Agent,
    Error,
}

impl StepKind {
    pub fn icon(self) -> &'static str {
        match self {
            StepKind::User => "›",
            StepKind::Tool => "⚙",
            StepKind::Reply => "●",
            StepKind::Compaction => "⟳",
            StepKind::Agent => "◇",
            StepKind::Error => "✗",
        }
    }
}

fn first_line(s: &str, max: usize) -> String {
    let line = s
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if line.chars().count() > max {
        format!("{}…", line.chars().take(max).collect::<String>())
    } else {
        line.to_string()
    }
}

/// The session's steps, built from events.
#[derive(Debug, Default)]
pub struct Timeline {
    pub steps: Vec<Step>,
    /// call id → step index, to finish tool steps.
    open_calls: std::collections::HashMap<String, usize>,
}

const MAX_STEPS: usize = 5_000;

impl Timeline {
    pub fn apply(&mut self, ev: &AgentEvent) {
        let now = chrono::Local::now();
        let step = |kind, label: String, detail: String| Step {
            kind,
            label,
            detail,
            at: now,
            user_index: None,
            ok: None,
        };
        match ev {
            AgentEvent::MessageReceived { role, content } if role == "user" => {
                let mut s = step(StepKind::User, first_line(content, 90), content.clone());
                s.user_index = Some(0);
                self.push(s);
            }
            AgentEvent::MessageReceived { role, content } if role == "assistant" => {
                if !content.trim().is_empty() {
                    self.push(step(
                        StepKind::Reply,
                        first_line(content, 90),
                        content.clone(),
                    ));
                }
            }
            AgentEvent::ToolCallStarted {
                call_id,
                tool,
                input,
            } => {
                let target = call_path(tool, input)
                    .or_else(|| input["command"].as_str().map(|c| first_line(c, 60)))
                    .or_else(|| input["action"].as_str().map(str::to_string))
                    .or_else(|| input["query"].as_str().map(|q| first_line(q, 60)))
                    .unwrap_or_default();
                let detail = serde_json::to_string_pretty(input).unwrap_or_default();
                self.push(step(StepKind::Tool, format!("{tool} {target}"), detail));
                self.open_calls
                    .insert(call_id.clone(), self.steps.len().saturating_sub(1));
            }
            AgentEvent::ToolCallCompleted { call_id, output } => {
                if let Some(i) = self.open_calls.remove(call_id)
                    && let Some(s) = self.steps.get_mut(i)
                {
                    s.ok = Some(output.success);
                    let text = if output.success {
                        output.output.clone()
                    } else {
                        output.error.clone().unwrap_or_default()
                    };
                    let excerpt: String = text.chars().take(1_500).collect();
                    s.detail = format!("{}\n\n── result ──\n{excerpt}", s.detail);
                }
            }
            AgentEvent::ContextCompaction { phase, message, .. } if phase == "completed" => {
                self.push(step(
                    StepKind::Compaction,
                    first_line(message, 90),
                    message.clone(),
                ));
            }
            AgentEvent::ChildSessionSpawned { task, .. } => {
                self.push(step(StepKind::Agent, first_line(task, 90), task.clone()));
            }
            AgentEvent::Error { message } => {
                self.push(step(
                    StepKind::Error,
                    first_line(message, 90),
                    message.clone(),
                ));
            }
            _ => {}
        }
    }

    fn push(&mut self, s: Step) {
        let is_user = s.kind == StepKind::User;
        self.steps.push(s);
        if is_user {
            // Count, for every user step, the user steps after it.
            let mut after = 0;
            for st in self.steps.iter_mut().rev() {
                if st.kind == StepKind::User {
                    st.user_index = Some(after);
                    after += 1;
                }
            }
        }
        if self.steps.len() > MAX_STEPS {
            let drop = self.steps.len() - MAX_STEPS;
            self.steps.drain(..drop);
            self.open_calls.retain(|_, i| *i >= drop);
            for i in self.open_calls.values_mut() {
                *i -= drop;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn file_map_counts_reads_and_edits_as_a_tree() {
        let mut m = FileMap::default();
        m.touch("read_file", &json!({"path": "src/a.rs"}));
        m.touch("edit_file", &json!({"path": "src/a.rs"}));
        m.touch("read_file", &json!({"path": "./README.md"}));
        m.touch("execute_bash", &json!({"command": "ls"}));
        assert_eq!(m.files["src/a.rs"].reads, 1);
        assert_eq!(m.files["src/a.rs"].edits, 1);
        assert_eq!(m.edited(), 1);
        assert_eq!(m.current.as_deref(), Some("README.md"));
        let tree: Vec<(usize, String)> = m.tree().into_iter().map(|(d, n, _)| (d, n)).collect();
        assert_eq!(
            tree,
            [
                (0, "README.md".into()),
                (0, "src/".into()),
                (1, "a.rs".into())
            ]
        );
    }

    #[test]
    fn timeline_records_steps_and_tool_results() {
        let mut t = Timeline::default();
        t.apply(&AgentEvent::MessageReceived {
            role: "user".into(),
            content: "fix the bug".into(),
        });
        t.apply(&AgentEvent::ToolCallStarted {
            call_id: "c1".into(),
            tool: "edit_file".into(),
            input: json!({"path": "src/a.rs"}),
        });
        t.apply(&AgentEvent::ToolCallCompleted {
            call_id: "c1".into(),
            output: dcode_ai_common::tool::ToolResult {
                call_id: "c1".into(),
                success: true,
                output: "done".into(),
                error: None,
            },
        });
        t.apply(&AgentEvent::MessageReceived {
            role: "user".into(),
            content: "thanks".into(),
        });
        assert_eq!(t.steps.len(), 3);
        assert_eq!(t.steps[1].label, "edit_file src/a.rs");
        assert_eq!(t.steps[1].ok, Some(true));
        assert!(t.steps[1].detail.contains("done"));
        assert_eq!(
            t.steps[0].user_index,
            Some(1),
            "one of your messages after it"
        );
        assert_eq!(t.steps[2].user_index, Some(0));
    }
}
