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

/// Thumbnail size of a picture in the chat (cells).
pub const THUMB_COLS: u16 = 28;
pub const THUMB_ROWS: u16 = 8;

#[derive(Debug, Clone)]
pub struct ChatLine {
    pub name: String,
    pub text: String,
    pub at: chrono::DateTime<chrono::Local>,
    /// A picture sent with it, and its thumbnail (drawn once).
    pub image: Option<dcode_ai_common::event::ChatImage>,
    pub thumb: Vec<ratatui::text::Line<'static>>,
}

#[derive(Debug, Default)]
pub struct Team {
    pub todos: Vec<TeamTodo>,
    pub chat: VecDeque<ChatLine>,
}

impl Team {
    pub fn apply(&mut self, ev: &AgentEvent) {
        match ev {
            AgentEvent::PersonChat {
                name, text, image, ..
            } => {
                let thumb = image
                    .as_ref()
                    .and_then(crate::chat_image::bytes)
                    .and_then(|b| crate::tui::thumbnail::from_bytes(&b, THUMB_COLS, THUMB_ROWS))
                    .unwrap_or_default();
                self.chat.push_back(ChatLine {
                    name: name.clone(),
                    text: text.clone(),
                    at: chrono::Local::now(),
                    image: image.clone(),
                    thumb,
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

    /// The pictures in the chat, oldest first (`/chat open <n>`).
    pub fn images(&self) -> Vec<&dcode_ai_common::event::ChatImage> {
        self.chat.iter().filter_map(|c| c.image.as_ref()).collect()
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

/// `/chat open [n]` and `/chat image <path>`, for the host and guests:
/// `Some(note)` when `rest` was one of them.
pub fn chat_picture_command(
    rest: &str,
    g: &mut crate::tui::state::TuiSessionState,
    workspace: &std::path::Path,
) -> Option<String> {
    let (word, arg) = rest.split_once(' ').unwrap_or((rest, ""));
    let arg = arg.trim();
    match word {
        "open" => {
            let images = g.team.images();
            if images.is_empty() {
                return Some("[chat] no pictures in the chat yet".into());
            }
            let n = arg.parse::<usize>().unwrap_or(images.len());
            let Some(img) = n.checked_sub(1).and_then(|i| images.get(i)) else {
                return Some(format!(
                    "[chat] there are {} pictures: /chat open 1…{}",
                    images.len(),
                    images.len()
                ));
            };
            let img = (*img).clone();
            Some(
                match crate::chat_image::to_temp_file(&img).and_then(|p| {
                    crate::tui::app::open_link_in_system(&p.to_string_lossy()).map(|_| p)
                }) {
                    Ok(p) => format!("[chat] opened {} ({})", img.name, p.display()),
                    Err(e) => format!("[chat] couldn't open it: {e}"),
                },
            )
        }
        "image" | "img" | "picture" if !arg.is_empty() => {
            let path = std::path::Path::new(arg.trim_matches(['"', '\'']));
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                workspace.join(path)
            };
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let prepared = std::fs::read(&path)
                .map_err(|e| e.to_string())
                .and_then(|b| crate::chat_image::prepare(&b, &name));
            Some(match (prepared, g.chat_image_sink.clone()) {
                (Ok(img), Some(sink)) => {
                    sink(img);
                    String::new()
                }
                (Ok(_), None) => "[chat] pictures need the terminal UI".into(),
                (Err(e), _) => format!("[chat] couldn't send {}: {e}", path.display()),
            })
        }
        _ => None,
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
            image: None,
        });
        assert_eq!(t.chat.back().unwrap().text, "lunch?");
        // A picture gets a thumbnail.
        let mut png = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(64, 32, image::Rgb([9, 9, 9])))
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        let pic = crate::chat_image::prepare(png.get_ref(), "bug.png").unwrap();
        t.apply(&AgentEvent::PersonChat {
            person: "p".into(),
            name: "Priya".into(),
            text: String::new(),
            image: Some(pic),
        });
        let last = t.chat.back().unwrap();
        assert!(!last.thumb.is_empty() && last.thumb.len() <= THUMB_ROWS as usize);
        assert_eq!(t.images().len(), 1);
    }
}
