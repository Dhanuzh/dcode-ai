//! Koda-style composer state object for TUI input editing.
//! UI rendering remains in `app.rs`; this module owns input text/cursor semantics.
//!
//! OpenCode-style keybinding model:
//!   Ctrl+← / Alt+B   move word backward
//!   Ctrl+→ / Alt+F   move word forward
//!   Ctrl+W / Alt+⌫   delete word backward
//!   Alt+D             delete word forward
//!   Ctrl+K            kill to end of line
//!   Ctrl+U            kill to start of line
//!   ↑ / ↓            per-line cursor movement in multiline; history at edges

#[derive(Debug, Clone, Default)]
pub struct TextArea {
    text: String,
    cursor_char_idx: usize,
    /// Yanked text from the last Ctrl+K / Ctrl+U kill (for future Ctrl+Y yank).
    pub kill_ring: Option<String>,
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

    #[allow(dead_code)]
    pub fn move_home(&mut self) {
        self.cursor_char_idx = 0;
    }

    #[allow(dead_code)]
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

    // ── Word movement ─────────────────────────────────────────────────────────

    /// Move cursor one word backward (Ctrl+← / Alt+B).
    /// Skips trailing whitespace then the preceding word token.
    pub fn move_word_backward(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut idx = self.cursor_char_idx;
        // skip whitespace to the left
        while idx > 0 && chars[idx - 1].is_whitespace() {
            idx -= 1;
        }
        // skip word chars to the left
        while idx > 0 && !chars[idx - 1].is_whitespace() {
            idx -= 1;
        }
        self.cursor_char_idx = idx;
    }

    /// Move cursor one word forward (Ctrl+→ / Alt+F).
    /// Skips the current word then any trailing whitespace, landing at the
    /// start of the next word (matches OpenCode / Emacs `forward-word`).
    pub fn move_word_forward(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let max = chars.len();
        let mut idx = self.cursor_char_idx;
        // skip any leading whitespace (if cursor is between words)
        while idx < max && chars[idx].is_whitespace() && chars[idx] != '\n' {
            idx += 1;
        }
        // skip word chars
        while idx < max && !chars[idx].is_whitespace() {
            idx += 1;
        }
        // skip trailing whitespace so cursor lands at next word start
        while idx < max && chars[idx].is_whitespace() && chars[idx] != '\n' {
            idx += 1;
        }
        self.cursor_char_idx = idx;
    }

    // ── Word deletion ─────────────────────────────────────────────────────────

    /// Delete the word before the cursor (Ctrl+W / Alt+Backspace).
    pub fn delete_word_backward(&mut self) {
        let prev = self.cursor_char_idx;
        self.move_word_backward();
        let next = self.cursor_char_idx;
        if next < prev {
            let start = cursor_byte_index(&self.text, next);
            let end = cursor_byte_index(&self.text, prev);
            self.text.replace_range(start..end, "");
        }
    }

    /// Delete the word after the cursor (Alt+D).
    pub fn delete_word_forward(&mut self) {
        let prev = self.cursor_char_idx;
        self.move_word_forward();
        let next = self.cursor_char_idx;
        if next > prev {
            let start = cursor_byte_index(&self.text, prev);
            let end = cursor_byte_index(&self.text, next);
            self.text.replace_range(start..end, "");
            self.cursor_char_idx = prev;
        }
    }

    // ── Kill ring ─────────────────────────────────────────────────────────────

    /// Kill from cursor to end of logical line (Ctrl+K).
    /// Saves killed text to kill_ring.
    pub fn kill_to_end_of_line(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let start = self.cursor_char_idx;
        // Find the next newline or end of text.
        let end = chars[start..]
            .iter()
            .position(|&c| c == '\n')
            .map(|p| start + p)
            .unwrap_or(chars.len());
        if end > start {
            let byte_start = cursor_byte_index(&self.text, start);
            let byte_end = cursor_byte_index(&self.text, end);
            self.kill_ring = Some(self.text[byte_start..byte_end].to_string());
            self.text.replace_range(byte_start..byte_end, "");
        } else if end < chars.len() && chars[end] == '\n' {
            // Cursor is at EOL — kill the newline itself.
            let byte_pos = cursor_byte_index(&self.text, end);
            let byte_next = cursor_byte_index(&self.text, end + 1);
            self.kill_ring = Some("\n".to_string());
            self.text.replace_range(byte_pos..byte_next, "");
        }
    }

    /// Kill from start of logical line to cursor (Ctrl+U).
    /// Saves killed text to kill_ring.
    #[allow(dead_code)]
    pub fn kill_to_start_of_line(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let end = self.cursor_char_idx;
        // Find the previous newline (or beginning of text).
        let start = chars[..end]
            .iter()
            .rposition(|&c| c == '\n')
            .map(|p| p + 1)
            .unwrap_or(0);
        if start < end {
            let byte_start = cursor_byte_index(&self.text, start);
            let byte_end = cursor_byte_index(&self.text, end);
            self.kill_ring = Some(self.text[byte_start..byte_end].to_string());
            self.text.replace_range(byte_start..byte_end, "");
            self.cursor_char_idx = start;
        }
    }

    /// Yank (paste) the last killed text at the cursor (Ctrl+Y).
    pub fn yank(&mut self) {
        if let Some(text) = self.kill_ring.clone() {
            self.insert_str(&text);
        }
    }

    // ── Multiline cursor navigation ───────────────────────────────────────────

    /// Move cursor up one visual line within multiline text.
    /// Returns `true` if the cursor moved; `false` if already on the first line
    /// (caller should switch to history navigation).
    pub fn move_up_in_multiline(&mut self, visual_width: usize) -> bool {
        let w = visual_width.max(1);
        let (line_idx, col) = self.cursor_line_col(w);
        if line_idx == 0 {
            return false;
        }
        // Move to same column on the line above.
        let target_line = line_idx - 1;
        let target_col = col;
        self.cursor_char_idx = self.char_idx_for_line_col(target_line, target_col, w);
        true
    }

    /// Move cursor down one visual line within multiline text.
    /// Returns `true` if the cursor moved; `false` if already on the last line
    /// (caller should switch to history navigation).
    pub fn move_down_in_multiline(&mut self, visual_width: usize) -> bool {
        let w = visual_width.max(1);
        let (line_idx, col) = self.cursor_line_col(w);
        let total_lines = self.visual_line_count(w);
        if line_idx + 1 >= total_lines {
            return false;
        }
        let target_line = line_idx + 1;
        let target_col = col;
        self.cursor_char_idx = self.char_idx_for_line_col(target_line, target_col, w);
        true
    }

    // ── Per-line Home / End ───────────────────────────────────────────────────

    /// Move to the start of the current logical line (Home key).
    pub fn move_home_line(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut idx = self.cursor_char_idx;
        while idx > 0 && chars[idx - 1] != '\n' {
            idx -= 1;
        }
        self.cursor_char_idx = idx;
    }

    /// Move to the end of the current logical line (End key).
    pub fn move_end_line(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let max = chars.len();
        let mut idx = self.cursor_char_idx;
        while idx < max && chars[idx] != '\n' {
            idx += 1;
        }
        self.cursor_char_idx = idx;
    }

    // ── Helpers ───────────────────────────────────────────────────────────────

    /// `(visual_line_index, col)` of the cursor given a terminal width.
    fn cursor_line_col(&self, w: usize) -> (usize, usize) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut visual_line = 0usize;
        let mut col = 0usize;
        for (i, &ch) in chars.iter().enumerate() {
            if i == self.cursor_char_idx {
                break;
            }
            if ch == '\n' {
                visual_line += 1;
                col = 0;
            } else {
                col += 1;
                if col >= w {
                    visual_line += 1;
                    col = 0;
                }
            }
        }
        (visual_line, col)
    }

    /// Total number of visual lines for `text` at terminal width `w`.
    fn visual_line_count(&self, w: usize) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut lines = 1usize;
        let mut col = 0usize;
        for &ch in &chars {
            if ch == '\n' {
                lines += 1;
                col = 0;
            } else {
                col += 1;
                if col >= w {
                    lines += 1;
                    col = 0;
                }
            }
        }
        lines
    }

    /// Convert a `(visual_line, col)` back to a char index, clamping to valid range.
    fn char_idx_for_line_col(&self, target_line: usize, target_col: usize, w: usize) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut visual_line = 0usize;
        let mut col = 0usize;

        for (i, &ch) in chars.iter().enumerate() {
            if visual_line == target_line {
                // Walk forward `target_col` cells on this line.
                let mut c = 0usize;
                let mut pos = i;
                while pos < chars.len() && c < target_col && chars[pos] != '\n' {
                    c += 1;
                    pos += 1;
                    if c < w && pos < chars.len() && chars[pos] == '\n' {
                        break;
                    }
                    if c >= w {
                        break;
                    }
                }
                return pos.min(chars.len());
            }
            if ch == '\n' {
                visual_line += 1;
                col = 0;
            } else {
                col += 1;
                if col >= w {
                    visual_line += 1;
                    col = 0;
                }
            }
        }
        // Target line is the last line (or beyond) — go to end.
        chars.len()
    }
}

pub fn cursor_byte_index(line: &str, cursor_char_idx: usize) -> usize {
    line.char_indices()
        .nth(cursor_char_idx)
        .map(|(i, _)| i)
        .unwrap_or(line.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ta_at(text: &str, cursor: usize) -> TextArea {
        let mut t = TextArea::default();
        t.set_text_with_cursor(text, cursor);
        t
    }

    #[test]
    fn move_word_backward_skips_whitespace_then_word() {
        let mut t = ta_at("hello world", 11); // cursor at end
        t.move_word_backward();
        assert_eq!(t.cursor_char_idx(), 6); // start of "world"
        t.move_word_backward();
        assert_eq!(t.cursor_char_idx(), 0); // start of "hello"
    }

    #[test]
    fn move_word_forward_skips_word_then_whitespace() {
        let mut t = ta_at("hello world", 0);
        t.move_word_forward();
        assert_eq!(t.cursor_char_idx(), 6); // after "hello "
        t.move_word_forward();
        assert_eq!(t.cursor_char_idx(), 11); // end
    }

    #[test]
    fn delete_word_backward_removes_preceding_word() {
        let mut t = ta_at("hello world", 11);
        t.delete_word_backward();
        assert_eq!(t.text(), "hello ");
        assert_eq!(t.cursor_char_idx(), 6);
    }

    #[test]
    fn delete_word_forward_removes_following_word_and_space() {
        // Alt+D from start of "hello world" deletes "hello " (word + space),
        // landing the cursor at the start of the next word.
        let mut t = ta_at("hello world", 0);
        t.delete_word_forward();
        assert_eq!(t.text(), "world");
        assert_eq!(t.cursor_char_idx(), 0);
    }

    #[test]
    fn kill_to_end_stores_in_kill_ring_and_removes() {
        let mut t = ta_at("hello world", 5);
        t.kill_to_end_of_line();
        assert_eq!(t.text(), "hello");
        assert_eq!(t.kill_ring.as_deref(), Some(" world"));
    }

    #[test]
    fn kill_to_start_stores_in_kill_ring_and_removes() {
        let mut t = ta_at("hello world", 6);
        t.kill_to_start_of_line();
        assert_eq!(t.text(), "world");
        assert_eq!(t.cursor_char_idx(), 0);
        assert_eq!(t.kill_ring.as_deref(), Some("hello "));
    }

    #[test]
    fn yank_reinserts_kill_ring() {
        let mut t = ta_at("hello world", 5);
        t.kill_to_end_of_line();
        t.yank();
        assert_eq!(t.text(), "hello world");
    }

    #[test]
    fn move_up_returns_false_on_first_line() {
        let mut t = ta_at("single line", 5);
        assert!(!t.move_up_in_multiline(80));
    }

    #[test]
    fn move_down_returns_false_on_last_line() {
        let mut t = ta_at("single line", 5);
        assert!(!t.move_down_in_multiline(80));
    }

    #[test]
    fn move_up_down_in_multiline_text() {
        let mut t = ta_at("first\nsecond", 9); // cursor on "eco" of "second"
        let moved = t.move_up_in_multiline(80);
        assert!(moved);
        // Should be on "first" at col 3 (same col as on "second")
        assert!(t.cursor_char_idx() <= 5);
    }

    #[test]
    fn home_end_on_multiline() {
        let mut t = ta_at("first\nsecond", 9);
        t.move_home_line();
        assert_eq!(t.cursor_char_idx(), 6); // start of "second"
        t.move_end_line();
        assert_eq!(t.cursor_char_idx(), 12); // end of "second"
    }

    #[test]
    fn kill_multiline_kills_only_current_line() {
        let mut t = ta_at("first\nsecond", 3); // mid "first"
        t.kill_to_end_of_line();
        assert_eq!(t.text(), "fir\nsecond");
        assert_eq!(t.kill_ring.as_deref(), Some("st"));
    }
}
