//! `run_workflow`: fan a list of tasks out to sub-agents, a few at a time,
//! and gather their reports — for jobs too big for one agent (migrate 40
//! files, review every module, fix every failing test). Progress goes to the
//! swarm board (the dashboard's swarm tab, `/fleet board`); cancelling the
//! turn stops the agents it started.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};
use futures_util::StreamExt;
use tokio::sync::{mpsc, oneshot};

use super::ToolExecutor;
use super::spawn_subagent::{BackgroundAgents, SpawnRequest, SpawnResponse, stop_agent};
use crate::approval::{ApprovalHandler, ModeCell};

/// At most this many tasks in one workflow.
pub const MAX_TASKS: usize = 100;
/// At most this many agents at once.
pub const MAX_PARALLEL: usize = 8;
/// One task may take this long.
const TASK_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
/// Each report is cut to this many characters in the combined result.
const REPORT_CHARS: usize = 1500;

pub struct RunWorkflowTool {
    spawn_tx: mpsc::Sender<SpawnRequest>,
    registry: BackgroundAgents,
    parent_mode: Option<ModeCell>,
    approval: Option<Arc<dyn ApprovalHandler>>,
    /// The swarm board progress is posted to.
    board: Option<PathBuf>,
}

impl RunWorkflowTool {
    pub fn new(
        spawn_tx: mpsc::Sender<SpawnRequest>,
        registry: BackgroundAgents,
        parent_mode: Option<ModeCell>,
        approval: Option<Arc<dyn ApprovalHandler>>,
        board: Option<PathBuf>,
    ) -> Self {
        Self {
            spawn_tx,
            registry,
            parent_mode,
            approval,
            board,
        }
    }
}

/// The tasks from the call: `tasks`, or `template` with `{item}` filled from
/// each of `items`. Empty ones are dropped; at most [`MAX_TASKS`].
pub fn expand_tasks(input: &serde_json::Value) -> Result<Vec<String>, String> {
    let strings = |k: &str| -> Vec<String> {
        input[k]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| match v {
                        serde_json::Value::String(s) => Some(s.trim().to_string()),
                        serde_json::Value::Null => None,
                        other => Some(other.to_string()),
                    })
                    .filter(|s| !s.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut tasks = strings("tasks");
    let items = strings("items");
    if !items.is_empty() {
        let template = input["template"].as_str().unwrap_or("").trim();
        if !template.contains("{item}") {
            return Err("`items` needs a `template` containing {item}".into());
        }
        tasks.extend(items.iter().map(|i| template.replace("{item}", i)));
    }
    if tasks.is_empty() {
        return Err("give `tasks` (a list of tasks) or `items` + `template`".into());
    }
    if tasks.len() > MAX_TASKS {
        return Err(format!(
            "{} tasks is too many for one workflow (at most {MAX_TASKS}) — split it",
            tasks.len()
        ));
    }
    Ok(tasks)
}

/// "12/40 done · 2 failed · ~6 min left"
pub fn progress_line(done: usize, failed: usize, total: usize, elapsed: Duration) -> String {
    let finished = done + failed;
    let mut s = format!("{done}/{total} done");
    if failed > 0 {
        s.push_str(&format!(" · {failed} failed"));
    }
    if finished > 0 && finished < total {
        let per = elapsed.as_secs_f64() / finished as f64;
        let left = (per * (total - finished) as f64 / 60.0).ceil() as u64;
        s.push_str(&format!(" · ~{left} min left"));
    }
    s
}

fn cut(s: &str, n: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

/// Stops the agents a workflow started if it ends early (the turn was
/// cancelled and the tool's future dropped).
struct StopOnDrop {
    registry: BackgroundAgents,
    before: HashSet<String>,
    tasks: HashSet<String>,
    armed: bool,
}

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let ids: Vec<String> = self
            .registry
            .lock()
            .map(|m| {
                m.iter()
                    .filter(|(id, r)| {
                        r.status == "running"
                            && !self.before.contains(*id)
                            && self.tasks.contains(&r.task)
                    })
                    .map(|(id, _)| id.clone())
                    .collect()
            })
            .unwrap_or_default();
        for id in ids {
            stop_agent(&self.registry, &id, "The workflow was cancelled.");
        }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for RunWorkflowTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "run_workflow".into(),
            description: format!(
                "Run many independent tasks with sub-agents, `parallel` at a time, and get every \
report back in one result — for jobs too big for one agent: migrate each of 40 files, review \
every module, fix each failing test. Give `tasks` (each complete and self-contained), or `items` \
plus a `template` with {{item}}. Agents share the swarm board (claims, findings) and progress is \
posted there for the user. Up to {MAX_TASKS} tasks, {MAX_PARALLEL} at once; each may run up to 2 \
hours. Edits happen in worktrees (use_worktree, default true) — merge the branches you keep. Use \
spawn_subagent for one or two tasks."
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "tasks": {"type": "array", "items": {"type": "string"}, "description": "Each a complete task for one agent"},
                    "items": {"type": "array", "items": {"type": "string"}, "description": "Values to fill {item} in `template` with"},
                    "template": {"type": "string", "description": "A task with {item} in it, run once per item"},
                    "parallel": {"type": "integer", "description": "Agents at once (default 3, max 8)"},
                    "use_worktree": {"type": "boolean", "description": "Each agent in its own git worktree (default true)"},
                    "explore": {"type": "boolean", "description": "Read-only research on a fast, cheap model (no edits, no worktrees)"}
                }
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let fail = |e: String| ToolResult {
            call_id: call.id.clone(),
            success: false,
            output: String::new(),
            error: Some(e),
        };
        let tasks = match expand_tasks(&call.input) {
            Ok(t) => t,
            Err(e) => return fail(e),
        };
        let parallel = call.input["parallel"]
            .as_u64()
            .unwrap_or(3)
            .clamp(1, MAX_PARALLEL as u64) as usize;
        let explore = call.input["explore"].as_bool().unwrap_or(false);
        let use_worktree = !explore && call.input["use_worktree"].as_bool().unwrap_or(true);
        let total = tasks.len();
        let started = Instant::now();
        let post = |text: String| {
            if let Some(b) = &self.board {
                let _ = dcode_ai_common::swarm::append(b, "workflow", "note", &text);
            }
        };
        post(format!("started: {total} task(s), {parallel} at a time"));

        let mut guard = StopOnDrop {
            registry: self.registry.clone(),
            before: self
                .registry
                .lock()
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default(),
            tasks: tasks.iter().cloned().collect(),
            armed: true,
        };

        let mode = self
            .parent_mode
            .as_ref()
            .and_then(|c| c.read().ok().map(|m| *m));
        let runs = futures_util::stream::iter(tasks.iter().cloned().enumerate())
            .map(|(i, task)| {
                let spawn_tx = self.spawn_tx.clone();
                let registry = self.registry.clone();
                let approval = self.approval.clone();
                async move {
                    let (reply, rx) = oneshot::channel();
                    let req = SpawnRequest {
                        task,
                        agent: None,
                        focus_files: Vec::new(),
                        use_worktree,
                        explore,
                        parent_mode: mode,
                        background: false,
                        browser_task: false,
                        resume: None,
                        fork: false,
                        parent_messages: None,
                        agents: registry,
                        approval,
                        reply,
                    };
                    let r: Result<SpawnResponse, String> = if spawn_tx.send(req).await.is_err() {
                        Err("the sub-agent spawner is not available".into())
                    } else {
                        match tokio::time::timeout(TASK_TIMEOUT, rx).await {
                            Ok(Ok(r)) => Ok(r),
                            Ok(Err(_)) => Err("ended without a report".into()),
                            Err(_) => Err("took more than 2 hours".into()),
                        }
                    };
                    (i, r)
                }
            })
            .buffer_unordered(parallel);
        futures_util::pin_mut!(runs);

        let mut results: Vec<Option<Result<SpawnResponse, String>>> = vec![None; total];
        let (mut done, mut failed) = (0, 0);
        while let Some((i, r)) = runs.next().await {
            match &r {
                Ok(resp) if resp.status == "completed" => done += 1,
                _ => failed += 1,
            }
            results[i] = Some(r);
            post(progress_line(done, failed, total, started.elapsed()));
        }
        guard.armed = false;

        let mins = started.elapsed().as_secs() / 60;
        let mut out = format!(
            "Workflow finished in {}: {}\n",
            if mins > 0 {
                format!("{mins} min")
            } else {
                format!("{} s", started.elapsed().as_secs())
            },
            progress_line(done, failed, total, started.elapsed())
        );
        for (i, (task, r)) in tasks.iter().zip(results).enumerate() {
            out.push_str(&format!("\n### {}. {}\n", i + 1, cut(task, 120)));
            match r {
                Some(Ok(resp)) => {
                    out.push_str(&format!("status: {}", resp.status));
                    if let Some(b) = &resp.branch {
                        out.push_str(&format!(" · branch {b}"));
                    }
                    out.push('\n');
                    out.push_str(&cut(&resp.output, REPORT_CHARS));
                    out.push('\n');
                }
                Some(Err(e)) => out.push_str(&format!("status: failed — {e}\n")),
                None => out.push_str("status: not run\n"),
            }
        }
        ToolResult {
            call_id: call.id.clone(),
            success: done > 0 || total == 0,
            output: out,
            error: (done == 0).then(|| "every task failed".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tasks_come_from_a_list_or_a_template() {
        let t = expand_tasks(&serde_json::json!({"tasks": ["a", " ", "b"]})).unwrap();
        assert_eq!(t, vec!["a", "b"]);
        let t = expand_tasks(&serde_json::json!({
            "items": ["x.rs", "y.rs"], "template": "port {item} to the new API"
        }))
        .unwrap();
        assert_eq!(
            t,
            vec!["port x.rs to the new API", "port y.rs to the new API"]
        );
        assert!(expand_tasks(&serde_json::json!({"items": ["a"], "template": "no slot"})).is_err());
        assert!(expand_tasks(&serde_json::json!({})).is_err());
        let many: Vec<String> = (0..101).map(|i| i.to_string()).collect();
        assert!(expand_tasks(&serde_json::json!({ "tasks": many })).is_err());
    }

    #[test]
    fn progress_counts_and_estimates() {
        assert_eq!(progress_line(0, 0, 4, Duration::ZERO), "0/4 done");
        assert_eq!(
            progress_line(1, 1, 4, Duration::from_secs(120)),
            "1/4 done · 1 failed · ~2 min left"
        );
        assert_eq!(progress_line(4, 0, 4, Duration::from_secs(9)), "4/4 done");
    }

    /// A fake spawner: answers each request after a moment, records how many
    /// ran at once.
    #[tokio::test]
    async fn runs_every_task_within_the_parallel_limit() {
        let (tx, mut rx) = mpsc::channel::<SpawnRequest>(16);
        let live = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (l2, p2) = (live.clone(), peak.clone());
        tokio::spawn(async move {
            while let Some(req) = rx.recv().await {
                let (l, p) = (l2.clone(), p2.clone());
                tokio::spawn(async move {
                    let now = l.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    p.fetch_max(now, std::sync::atomic::Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    l.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                    let fail = req.task.contains("bad");
                    let _ = req.reply.send(SpawnResponse {
                        child_session_id: "c".into(),
                        status: if fail { "error" } else { "completed" }.into(),
                        output: format!("did {}", req.task),
                        workspace: String::new(),
                        branch: None,
                        worktree_path: None,
                    });
                });
            }
        });
        let dir = std::env::temp_dir().join(format!("dcode-wf-{}", std::process::id()));
        let board = dir.join("lead.jsonl");
        let tool = RunWorkflowTool::new(tx, Default::default(), None, None, Some(board.clone()));
        let tasks: Vec<String> = (0..7)
            .map(|i| format!("t{i}"))
            .chain(["bad".into()])
            .collect();
        let r = tool
            .execute(&ToolCall {
                id: "w".into(),
                name: "run_workflow".into(),
                input: serde_json::json!({"tasks": tasks, "parallel": 3}),
            })
            .await;
        assert!(r.success);
        assert!(r.output.contains("7/8 done · 1 failed"), "{}", r.output);
        assert!(r.output.contains("did t6") && r.output.contains("status: error"));
        assert!(peak.load(std::sync::atomic::Ordering::SeqCst) <= 3);
        let posts = dcode_ai_common::swarm::load(&board);
        assert_eq!(posts.len(), 9, "start + one per task");
        assert!(posts.last().unwrap().text.starts_with("7/8 done"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
