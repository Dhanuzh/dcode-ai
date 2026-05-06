//! Koda-style composer state object for TUI input editing.
//! UI rendering remains in `app.rs`; this module owns input text/cursor semantics.

#[derive(Debug, Clone, Default)]
pub struct TextArea {
    text: String,
    cursor_char_idx: usize,
}

impl TextArea {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn cursor_char_idx(&self) -> usize {
        self.cursor_char_idx
    }

    pub fn set_text(&mut self, text: impl Into<String>) {
        self.text = text.into();
        self.cursor_char_idx = self.text.chars().count();
    }

    pub fn set_text_with_cursor(&mut self, text: impl Into<String>, cursor_char_idx: usize) {
        self.text = text.into();
        let max = self.text.chars().count();
        self.cursor_char_idx = cursor_char_idx.min(max);
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor_char_idx = 0;
    }

    pub fn take_text(&mut self) -> String {
        let out = std::mem::take(&mut self.text);
        self.cursor_char_idx = 0;
        out
    }

    pub fn insert_char(&mut self, ch: char) {
        let idx = cursor_byte_index(&self.text, self.cursor_char_idx);
        self.text.insert(idx, ch);
        self.cursor_char_idx += 1;
    }

    pub fn insert_str(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        let idx = cursor_byte_index(&self.text, self.cursor_char_idx);
        self.text.insert_str(idx, s);
        self.cursor_char_idx += s.chars().count();
    }

    pub fn move_left(&mut self) {
        self.cursor_char_idx = self.cursor_char_idx.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        let max = self.text.chars().count();
        self.cursor_char_idx = (self.cursor_char_idx + 1).min(max);
    }

    pub fn move_home(&mut self) {
        self.cursor_char_idx = 0;
    }

    pub fn move_end(&mut self) {
        self.cursor_char_idx = self.text.chars().count();
    }

    pub fn backspace(&mut self) {
        if self.cursor_char_idx == 0 {
            return;
        }
        let start = cursor_byte_index(&self.text, self.cursor_char_idx - 1);
        let end = cursor_byte_index(&self.text, self.cursor_char_idx);
        self.text.replace_range(start..end, "");
        self.cursor_char_idx -= 1;
    }

    pub fn delete(&mut self) {
        let max = self.text.chars().count();
        if self.cursor_char_idx >= max {
            return;
        }
        let start = cursor_byte_index(&self.text, self.cursor_char_idx);
        let end = cursor_byte_index(&self.text, self.cursor_char_idx + 1);
        self.text.replace_range(start..end, "");
    }
}

fn cursor_byte_index(line: &str, cursor_char_idx: usize) -> usize {
    line.char_indices()
        .nth(cursor_char_idx)
        .map(|(i, _)| i)
        .unwrap_or(line.len())
}
