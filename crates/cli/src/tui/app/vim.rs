//! Vim keys in the prompt (`/vim`), like Claude Code's vim mode: Esc enters
//! NORMAL mode; `i a A I o O` return to INSERT. NORMAL supports the common
//! motions and edits; Enter, Ctrl-keys and arrows keep their usual meaning.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::tui::state::TuiSessionState;

/// Handle `key` for vim mode. `true` = consumed; `false` = let the normal
/// key handling run (INSERT mode typing, Enter, Ctrl-keys, …).
pub(super) fn handle_vim_key(key: KeyEvent, g: &mut TuiSessionState) -> bool {
    if !g.vim_enabled {
        return false;
    }
    if !g.vim_normal {
        if key.code == KeyCode::Esc && key.modifiers.is_empty() {
            g.vim_normal = true;
            g.vim_pending = None;
            // Like vim, leaving INSERT steps back onto the last character.
            g.move_input_left();
            return true;
        }
        return false;
    }
    // NORMAL mode: only plain (or shifted) characters are vim commands.
    let KeyCode::Char(c) = key.code else {
        g.vim_pending = None;
        return false;
    };
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return false;
    }
    if let Some(op) = g.vim_pending.take() {
        apply_operator(g, op, c);
        return true;
    }
    match c {
        'h' => g.move_input_left(),
        'l' => g.move_input_right(),
        '0' | '^' => g.move_input_home_line(),
        // `$` rests on the last character (not past it, as INSERT's End does).
        '$' => {
            g.move_input_end_line();
            let at_line_start = g.cursor_char_idx == 0
                || g.input_buffer.chars().nth(g.cursor_char_idx - 1) == Some('\n');
            if !at_line_start {
                g.move_input_left();
            }
        }
        'w' | 'e' | 'W' | 'E' => g.move_input_word_forward(),
        'b' | 'B' => g.move_input_word_backward(),
        'j' => {
            g.move_input_down(usize::MAX);
        }
        'k' => {
            g.move_input_up(usize::MAX);
        }
        'x' => g.delete_input(),
        'X' => g.backspace_input(),
        'D' => g.kill_input_to_end(),
        'C' => {
            g.kill_input_to_end();
            insert(g);
        }
        'p' => g.yank_input(),
        'u' => {
            g.undo_input();
        }
        'i' => insert(g),
        'a' => {
            g.move_input_right();
            insert(g);
        }
        'A' => {
            g.move_input_end_line();
            insert(g);
        }
        'I' => {
            g.move_input_home_line();
            insert(g);
        }
        'o' => {
            g.move_input_end_line();
            g.insert_input_char('\n');
            insert(g);
        }
        'O' => {
            g.move_input_home_line();
            g.insert_input_char('\n');
            g.move_input_left();
            insert(g);
        }
        'd' | 'c' => g.vim_pending = Some(c),
        // Unknown keys do nothing in NORMAL mode (never typed into the prompt).
        _ => {}
    }
    true
}

fn insert(g: &mut TuiSessionState) {
    g.vim_normal = false;
    g.vim_pending = None;
}

/// `d`/`c` followed by a motion: `dd`/`cc` (line), `dw`/`cw`, `d$`/`c$`, `d0`.
fn apply_operator(g: &mut TuiSessionState, op: char, motion: char) {
    match motion {
        m if m == op => {
            g.move_input_home_line();
            g.kill_input_to_end();
        }
        'w' | 'e' => g.delete_input_word_forward(),
        'b' => g.delete_input_word_backward(),
        '$' => g.kill_input_to_end(),
        '0' => g.kill_input_to_start(),
        _ => return,
    }
    if op == 'c' {
        insert(g);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(text: &str) -> TuiSessionState {
        let mut g = TuiSessionState::new(
            "s".into(),
            "m".into(),
            "@build".into(),
            "default".into(),
            std::path::PathBuf::from("."),
            false,
        );
        g.vim_enabled = true;
        g.set_input_text(text);
        g
    }

    fn press(g: &mut TuiSessionState, keys: &str) {
        for c in keys.chars() {
            let code = if c == '\u{1b}' {
                KeyCode::Esc
            } else {
                KeyCode::Char(c)
            };
            if !handle_vim_key(KeyEvent::new(code, KeyModifiers::NONE), g) {
                g.insert_input_char(c);
            }
        }
    }

    #[test]
    fn normal_mode_edits_like_vim() {
        let mut g = st("hello big world");
        press(&mut g, "\u{1b}0wdw");
        assert_eq!(g.input_buffer, "hello world");
        press(&mut g, "$x");
        assert_eq!(g.input_buffer, "hello worl");
        press(&mut g, "Ad!");
        assert_eq!(g.input_buffer, "hello world!", "A appends in INSERT");
        press(&mut g, "\u{1b}ccnew");
        assert_eq!(g.input_buffer, "new");
        press(&mut g, "\u{1b}u");
        assert!(g.vim_normal);
        // Unknown keys never type into the prompt in NORMAL mode.
        let mut g = st("abc");
        press(&mut g, "\u{1b}zq");
        assert_eq!(g.input_buffer, "abc");
    }
}
