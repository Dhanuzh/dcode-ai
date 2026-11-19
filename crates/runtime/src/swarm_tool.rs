//! `swarm_post` / `swarm_read`: the lead agent and its sub-agents share one
//! message board (`dcode_ai_common::swarm`). A post also reaches every other
//! running sub-agent at its next step, so workers see claims and findings
//! without polling.

use std::path::PathBuf;

use dcode_ai_common::swarm;
use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};
use dcode_ai_core::tools::ToolExecutor;
use dcode_ai_core::tools::spawn_subagent::BackgroundAgents;
use serde_json::json;

fn ok(call: &ToolCall, output: String) -> ToolResult {
    ToolResult {
        call_id: call.id.clone(),
        success: true,
        output,
        error: None,
    }
}

fn fail(call: &ToolCall, error: impl Into<String>) -> ToolResult {
    ToolResult {
        call_id: call.id.clone(),
        success: false,
        output: String::new(),
        error: Some(error.into()),
    }
}

/// Puts `post` in the inbox of every running agent in `peers` except `me`.
/// Returns how many got it.
pub fn deliver_to_peers(peers: &BackgroundAgents, me: &str, post: &swarm::Post) -> usize {
    let Ok(map) = peers.lock() else {
        return 0;
    };
    let line = format!(
        "Swarm board — {} {} {}: {}",
        post.who,
        swarm::icon(&post.kind),
        post.kind,
        post.text
    );
    let mut n = 0;
    for (id, r) in map.iter() {
        if id == me || r.status != "running" {
            continue;
        }
        if let Some(inbox) = &r.steer
            && let Ok(mut q) = inbox.lock()
        {
            q.push(line.clone());
            n += 1;
        }
    }
    n
}

pub struct SwarmPostTool {
    path: PathBuf,
    who: String,
    /// The lead's sub-agents (to tell the running ones), and this agent's
    /// own id among them.
    peers: Option<(BackgroundAgents, String)>,
}

impl SwarmPostTool {
    pub fn new(
        path: PathBuf,
        who: impl Into<String>,
        peers: Option<(BackgroundAgents, String)>,
    ) -> Self {
        Self {
            path,
            who: who.into(),
            peers,
        }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for SwarmPostTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "swarm_post".into(),
            description:
                "Post to the swarm board shared by the lead agent and all its sub-agents. \
Running sub-agents read new posts at their next step. Use it to coordinate parallel work: \
`claim` before you start a piece (\"taking: auth tests\") so nobody duplicates it, `finding` for \
something others need to know, `question` / `answer` between agents, `done` when your piece is \
finished (closes your claim). Keep posts to one or two sentences."
                    .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "kind": {"type": "string", "enum": swarm::KINDS, "description": "claim, finding, question, answer, done or note"},
                    "text": {"type": "string", "description": "The post (one or two sentences)"}
                },
                "required": ["kind", "text"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let text = call.input["text"].as_str().unwrap_or("").trim();
        if text.is_empty() {
            return fail(call, "text is required");
        }
        let kind = call.input["kind"].as_str().unwrap_or("note");
        let post = match swarm::append(&self.path, &self.who, kind, text) {
            Ok(p) => p,
            Err(e) => return fail(call, format!("could not write the swarm board: {e}")),
        };
        let told = self
            .peers
            .as_ref()
            .map(|(reg, me)| deliver_to_peers(reg, me, &post))
            .unwrap_or(0);
        ok(
            call,
            format!(
                "Posted ({}). {told} running agent(s) will see it at their next step.",
                post.kind
            ),
        )
    }
}

pub struct SwarmReadTool {
    path: PathBuf,
}

impl SwarmReadTool {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }
}

/// The board as the model reads it: open claims first, then the posts after
/// `since` (the last `limit` of them).
pub fn render_board(posts: &[swarm::Post], since: usize, limit: usize) -> String {
    if posts.is_empty() {
        return "The swarm board is empty.".into();
    }
    let mut out = String::new();
    let open = swarm::open_claims(posts);
    if !open.is_empty() {
        out.push_str("Open claims:\n");
        for p in open {
            out.push_str(&format!("- {}: {}\n", p.who, p.text));
        }
        out.push('\n');
    }
    let new = &posts[since.min(posts.len())..];
    let skip = new.len().saturating_sub(limit);
    out.push_str(&format!(
        "Posts {}–{} of {}:\n",
        since.min(posts.len()) + skip + 1,
        posts.len(),
        posts.len()
    ));
    for p in &new[skip..] {
        out.push_str(&format!("{}\n", swarm::one_line(p)));
    }
    out
}

#[async_trait::async_trait]
impl ToolExecutor for SwarmReadTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "swarm_read".into(),
            description: "Read the swarm board shared by the lead agent and its sub-agents: open \
claims (who is working on what) and recent posts. Read it before claiming work. `since` = the \
number of posts you've already seen, to get only newer ones."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "since": {"type": "integer", "description": "Skip this many (already seen) posts"},
                    "limit": {"type": "integer", "description": "At most this many posts (default 40)"}
                }
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let since = call.input["since"].as_u64().unwrap_or(0) as usize;
        let limit = call.input["limit"].as_u64().unwrap_or(40).clamp(1, 200) as usize;
        let posts = swarm::load(&self.path);
        ok(call, render_board(&posts, since, limit))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dcode_ai_core::tools::spawn_subagent::AgentRecord;

    fn call(input: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "c".into(),
            name: "swarm_post".into(),
            input,
        }
    }

    #[tokio::test]
    async fn a_claim_reaches_the_other_running_agents_and_the_board() {
        let dir = std::env::temp_dir().join(format!("dcode-swarm-tool-{}", std::process::id()));
        let path = dir.join("lead.jsonl");
        let _ = std::fs::remove_file(&path);
        let reg: BackgroundAgents = Default::default();
        let (ia, ib): (
            dcode_ai_core::agent::SteerInbox,
            dcode_ai_core::agent::SteerInbox,
        ) = (Default::default(), Default::default());
        {
            let mut m = reg.lock().unwrap();
            let mut a = AgentRecord::new("a");
            a.steer = Some(ia.clone());
            let mut b = AgentRecord::new("b");
            b.steer = Some(ib.clone());
            let mut done = AgentRecord::new("c");
            done.status = "completed".into();
            done.steer = Some(Default::default());
            m.insert("a".into(), a);
            m.insert("b".into(), b);
            m.insert("c".into(), done);
        }
        let tool = SwarmPostTool::new(path.clone(), "a", Some((reg.clone(), "a".into())));
        let r = tool
            .execute(&call(
                json!({"kind": "claim", "text": "taking: auth tests"}),
            ))
            .await;
        assert!(r.success, "{:?}", r.error);
        assert!(r.output.contains("1 running agent"), "{}", r.output);
        assert!(ia.lock().unwrap().is_empty(), "not to itself");
        assert!(ib.lock().unwrap()[0].contains("a ⚑ claim: taking: auth tests"));

        let read = SwarmReadTool::new(path.clone())
            .execute(&call(json!({})))
            .await;
        assert!(
            read.output
                .contains("Open claims:\n- a: taking: auth tests"),
            "{}",
            read.output
        );
        assert!(!tool.execute(&call(json!({"kind": "note"}))).await.success);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reading_since_skips_what_was_seen() {
        let posts: Vec<swarm::Post> = (0..5)
            .map(|i| swarm::Post {
                at: String::new(),
                who: "w".into(),
                kind: "note".into(),
                text: format!("n{i}"),
            })
            .collect();
        let out = render_board(&posts, 3, 40);
        assert!(
            out.contains("n3") && out.contains("n4") && !out.contains("n2"),
            "{out}"
        );
        let out = render_board(&posts, 0, 2);
        assert!(
            out.contains("Posts 4–5 of 5") && !out.contains("n2"),
            "{out}"
        );
    }
}
