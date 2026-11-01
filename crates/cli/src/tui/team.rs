//! The people on a session working together: their chat (not sent to the
//! agent) and a shared to-do list the agent sees every turn.

use dcode_ai_common::event::AgentEvent;
use std::collections::VecDeque;

/// Wraps the open to-dos after a prompt; hidden in the transcript.
pub const TODO_BLOCK_START: &str = "<team-todos>";
pub const TODO_BLOCK_END: &str = "</team-todos>";
/// Chat lines kept.
const CHAT_KEEP: usize = 300;

#[derive(Debug, Clone)]
pub struct TeamTodo {
    pub id: String,
    pub text: String,
    pub by: String,
    pub done_by: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ChatLine {
    pub name: String,
    pub text: String,
    pub at: chrono::DateTime<chrono::Local>,
}

#[derive(Debug, Default)]
pub struct Team {
    pub todos: Vec<TeamTodo>,
    pub chat: VecDeque<ChatLine>,
}

impl Team {
    pub fn apply(&mut self, ev: &AgentEvent) {
        match ev {
            AgentEvent::PersonChat { name, text, .. } => {
                self.chat.push_back(ChatLine {
                    name: name.clone(),
                    text: text.clone(),
                    at: chrono::Local::now(),
                });
                while self.chat.len() > CHAT_KEEP {
                    self.chat.pop_front();
                }
            }
            AgentEvent::TeamTodoAdded { id, text, by } => {
                if !self.todos.iter().any(|t| t.id == *id) {
                    self.todos.push(TeamTodo {
                        id: id.clone(),
                        text: text.clone(),
                        by: by.clone(),
                        done_by: None,
                    });
                }
            }
            AgentEvent::TeamTodoDone { id, by } => {
                if let Some(t) = self.todos.iter_mut().find(|t| t.id == *id) {
                    t.done_by = Some(by.clone());
                }
            }
            _ => {}
        }
    }

    pub fn open(&self) -> impl Iterator<Item = &TeamTodo> {
        self.todos.iter().filter(|t| t.done_by.is_none())
    }

    /// The item `/todo done <n>` means: its number in the list, or its id.
    pub fn find(&self, which: &str) -> Option<&TeamTodo> {
        let which = which.trim().trim_start_matches('#');
        match which.parse::<usize>() {
            Ok(n) if n >= 1 => self.todos.get(n - 1),
            _ => self.todos.iter().find(|t| t.id == which),
        }
    }

    /// The list as text lines (`/todo`).
    pub fn lines(&self) -> Vec<String> {
        if self.todos.is_empty() {
            return vec!["no shared to-dos yet — /todo <text> adds one".into()];
        }
        self.todos
            .iter()
            .enumerate()
            .map(|(i, t)| match &t.done_by {
                Some(who) => format!("{:>2}. ☑ {}  (done · {who})", i + 1, t.text),
                None => format!("{:>2}. ☐ {}  ({})", i + 1, t.text, t.by),
            })
            .collect()
    }

    /// What the agent is told with each prompt while items are open.
    pub fn prompt_block(&self) -> Option<String> {
        let open: Vec<String> = self
            .todos
            .iter()
            .enumerate()
            .filter(|(_, t)| t.done_by.is_none())
            .map(|(i, t)| format!("{}. {} (added by {})", i + 1, t.text, t.by))
            .collect();
        if open.is_empty() {
            return None;
        }
        Some(format!(
            "{TODO_BLOCK_START}\nThe people on this session keep a shared to-do list. Open items:\n{}\nWork on them when they fit what you're asked; when you finish one, say so (\"done: #n\") so someone can tick it off.\n{TODO_BLOCK_END}",
            open.join("\n")
        ))
    }
}

/// A prompt without the to-do block (for showing it).
pub fn strip_todo_block(text: &str) -> String {
    match (text.find(TODO_BLOCK_START), text.find(TODO_BLOCK_END)) {
        (Some(a), Some(b)) if b > a => {
            let mut out = String::with_capacity(text.len());
            out.push_str(text[..a].trim_end());
            out.push_str(&text[b + TODO_BLOCK_END.len()..]);
            out.trim_end().to_string()
        }
        _ => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn todos_and_chat_follow_events() {
        let mut t = Team::default();
        t.apply(&AgentEvent::TeamTodoAdded {
            id: "a1".into(),
            text: "fix login".into(),
            by: "Priya".into(),
        });
        t.apply(&AgentEvent::TeamTodoAdded {
            id: "b2".into(),
            text: "write tests".into(),
            by: "Sam".into(),
        });
        t.apply(&AgentEvent::TeamTodoDone {
            id: "a1".into(),
            by: "dhanush".into(),
        });
        assert_eq!(t.open().count(), 1);
        assert_eq!(t.find("2").unwrap().id, "b2");
        assert_eq!(t.find("a1").unwrap().text, "fix login");
        let block = t.prompt_block().unwrap();
        assert!(block.contains("2. write tests (added by Sam)"));
        assert!(!block.contains("fix login"));
        let prompt = format!("hello\n\n{block}");
        assert_eq!(strip_todo_block(&prompt), "hello");
        assert!(t.lines()[0].contains("☑ fix login"));

        t.apply(&AgentEvent::PersonChat {
            person: "p".into(),
            name: "Priya".into(),
            text: "lunch?".into(),
        });
        assert_eq!(t.chat.back().unwrap().text, "lunch?");
    }
}
