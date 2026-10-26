//! `/btw <question>`: a side question about the current work, answered by
//! one tool-less model call over the conversation so far. The answer is
//! shown in a panel and never added to the conversation, and it works while
//! the agent is busy (the context is a snapshot taken at turn boundaries).

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dcode_ai_common::config::DcodeAiConfig;
use dcode_ai_common::message::{Message, Role};
use dcode_ai_core::provider::StreamChunk;
use tokio::sync::mpsc::UnboundedSender;

use crate::tui::state::TuiSessionState;

/// What a side question is answered from.
#[derive(Clone)]
pub struct BtwContext {
    pub config: DcodeAiConfig,
    pub model: String,
    pub workspace: PathBuf,
    pub messages: Vec<Message>,
}

/// Shared slot the session runtime keeps up to date.
pub type BtwHandle = Arc<Mutex<Option<BtwContext>>>;

/// The `/btw` panel shown in place of the composer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BtwView {
    pub question: String,
    pub answer: String,
    pub done: bool,
    pub error: Option<String>,
    pub scroll: usize,
}

/// Budget for the conversation text sent with a side question.
const CONTEXT_CHARS: usize = 40_000;

fn clip(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let head: String = text.chars().take(max).collect();
    format!("{head} …")
}

/// The conversation as plain text, newest last, within the budget. Tool
/// calls and results are shortened; the system prompt is left out.
pub fn conversation_text(messages: &[Message]) -> String {
    let mut parts: Vec<String> = Vec::new();
    let mut used = 0;
    for m in messages.iter().rev() {
        let text = m.content.to_summary_text();
        let entry = match m.role {
            Role::System => continue,
            Role::User => format!("User: {}", clip(text.trim(), 4_000)),
            Role::Assistant => {
                let mut e = String::new();
                if !text.trim().is_empty() {
                    e.push_str(&format!("Assistant: {}", clip(text.trim(), 4_000)));
                }
                for call in m.tool_calls.iter().flatten() {
                    if !e.is_empty() {
                        e.push('\n');
                    }
                    e.push_str(&format!(
                        "Assistant ran {}: {}",
                        call.name,
                        clip(&call.arguments.to_string(), 200)
                    ));
                }
                e
            }
            Role::Tool => format!("Tool result: {}", clip(text.trim(), 400)),
        };
        if entry.is_empty() {
            continue;
        }
        used += entry.len();
        if used > CONTEXT_CHARS && !parts.is_empty() {
            break;
        }
        parts.push(entry);
    }
    parts.reverse();
    parts.join("\n\n")
}

/// The one-shot prompt for a side question.
pub fn side_question_prompt(
    workspace: &std::path::Path,
    messages: &[Message],
    question: &str,
) -> String {
    let convo = conversation_text(messages);
    let convo = if convo.is_empty() {
        "(no conversation yet)".to_string()
    } else {
        convo
    };
    format!(
        "You are helping a developer who is working with a coding agent in {}. \
         They have a quick side question. Answer it briefly and directly from the \
         conversation below and what you know. You cannot run tools or change anything; \
         do not start new work or continue the agent's task.\n\n\
         <conversation>\n{convo}\n</conversation>\n\nSide question: {question}",
        workspace.display()
    )
}

fn update(state: &Arc<Mutex<TuiSessionState>>, question: &str, f: impl FnOnce(&mut BtwView)) {
    if let Ok(mut g) = state.lock() {
        // Only the panel still showing this question gets the update.
        if let Some(v) = g.btw.as_mut().filter(|v| v.question == question) {
            f(v);
            g.touch_transcript();
        }
    }
}

async fn answer(ctx: BtwContext, question: String, state: Arc<Mutex<TuiSessionState>>) {
    let provider = match dcode_ai_core::provider::factory::build_provider(&ctx.config) {
        Ok(p) => p,
        Err(e) => {
            return update(&state, &question, |v| {
                v.error = Some(e.to_string());
                v.done = true;
            });
        }
    };
    let messages = vec![Message::user(side_question_prompt(
        &ctx.workspace,
        &ctx.messages,
        &question,
    ))];
    let mut stream = match provider
        .chat(&messages, &[], &ctx.model, &ctx.workspace)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            return update(&state, &question, |v| {
                v.error = Some(e.to_string());
                v.done = true;
            });
        }
    };
    while let Some(chunk) = stream.recv().await {
        match chunk {
            StreamChunk::TextDelta(t) => update(&state, &question, |v| v.answer.push_str(&t)),
            StreamChunk::Error(e) | StreamChunk::FatalError(e) => {
                return update(&state, &question, |v| {
                    v.error = Some(e);
                    v.done = true;
                });
            }
            StreamChunk::Done => break,
            _ => {}
        }
    }
    update(&state, &question, |v| {
        if v.answer.trim().is_empty() && v.error.is_none() {
            v.error = Some("the model returned an empty answer".into());
        }
        v.done = true;
    });
}

/// Start the worker that answers side questions sent on the returned channel.
pub fn spawn_btw_worker(
    handle: BtwHandle,
    state: Arc<Mutex<TuiSessionState>>,
) -> UnboundedSender<String> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::spawn(async move {
        while let Some(question) = rx.recv().await {
            let ctx = handle.lock().ok().and_then(|c| c.clone());
            let Some(ctx) = ctx else {
                update(&state, &question, |v| {
                    v.error = Some("the session is still starting; try again".into());
                    v.done = true;
                });
                continue;
            };
            tokio::spawn(answer(ctx, question, state.clone()));
        }
    });
    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversation_text_skips_system_and_keeps_order() {
        let msgs = vec![
            Message::system("secret system prompt"),
            Message::user("fix the login bug"),
            Message::assistant("Looking at auth.rs now."),
            Message::tool("c1", "fn login() {}"),
        ];
        let t = conversation_text(&msgs);
        assert!(!t.contains("secret system prompt"));
        let user = t.find("User: fix the login bug").expect("user");
        let asst = t.find("Assistant: Looking at auth.rs").expect("assistant");
        assert!(user < asst);
        assert!(t.contains("Tool result: fn login() {}"));
    }

    #[test]
    fn conversation_text_keeps_the_newest_within_budget() {
        let big = "x".repeat(3_900);
        let msgs: Vec<Message> = (0..40)
            .map(|i| Message::user(format!("msg{i} {big}")))
            .collect();
        let t = conversation_text(&msgs);
        assert!(t.contains("msg39"));
        assert!(!t.contains("msg0 "));
        assert!(t.len() < CONTEXT_CHARS + 5_000);
    }
}
