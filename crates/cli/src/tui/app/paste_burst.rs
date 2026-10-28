//! Paste detection for terminals without bracketed paste (the Windows
//! console, some SSH clients). There a paste arrives as ordinary key
//! presses, and the first newline would submit the prompt half-pasted.
//! Keys that are already queued together form a burst; a burst with a line
//! break in the middle (or a long one with a line break anywhere) is turned
//! into one paste, like a bracketed paste would be.

use std::collections::VecDeque;
use std::time::Duration;

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, poll, read};

/// Bursts at least this long count as a paste even when the only line break
/// is the last key (a pasted paragraph that ends with a newline).
const LONG_BURST: usize = 16;

/// A plain typing key: a character, Enter or Tab, without Ctrl/Alt.
fn burst_key(key: &KeyEvent) -> bool {
    let plain = !key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT);
    plain && matches!(key.code, KeyCode::Char(_) | KeyCode::Enter | KeyCode::Tab)
}

/// The text a burst of key events types, or `None` when it is not a paste.
pub(super) fn burst_text(events: &[Event]) -> Option<String> {
    let mut text = String::new();
    let mut chars = 0usize;
    for ev in events {
        let Event::Key(key) = ev else { return None };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        if !burst_key(key) {
            return None;
        }
        match key.code {
            KeyCode::Enter | KeyCode::Char('\r') | KeyCode::Char('\n') => text.push('\n'),
            KeyCode::Tab => text.push('\t'),
            KeyCode::Char(c) => text.push(c),
            _ => return None,
        }
        chars += 1;
    }
    let newline = text.find('\n')?;
    let inner_break = newline + 1 < text.len();
    (chars >= 2 && (inner_break || chars >= LONG_BURST)).then_some(text)
}

/// After `first` was read: when more typing keys are already waiting, read
/// them all. Returns the event to handle now (a `Paste` when the burst was
/// a paste) and leaves the rest in `pending`.
pub(super) fn coalesce(first: Event, pending: &mut VecDeque<Event>) -> Event {
    let starts_burst = matches!(&first, Event::Key(k) if burst_key(k))
        && pending.is_empty()
        && poll(Duration::ZERO).unwrap_or(false);
    if !starts_burst {
        return first;
    }
    let mut burst = vec![first];
    let mut leftover = None;
    // A console paste lands in the input buffer at once; the short wait only
    // bridges the gaps between its chunks.
    while poll(Duration::from_millis(3)).unwrap_or(false) {
        let Ok(ev) = read() else { break };
        match &ev {
            Event::Key(k) if k.kind == KeyEventKind::Release || burst_key(k) => burst.push(ev),
            _ => {
                leftover = Some(ev);
                break;
            }
        }
    }
    let ev = match burst_text(&burst) {
        Some(text) => Event::Paste(text),
        None => {
            let mut rest = burst.into_iter();
            let first = rest.next().expect("burst starts with an event");
            pending.extend(rest);
            first
        }
    };
    pending.extend(leftover);
    ev
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn typed(s: &str) -> Vec<Event> {
        s.chars()
            .map(|c| match c {
                '\n' => key(KeyCode::Enter),
                c => key(KeyCode::Char(c)),
            })
            .collect()
    }

    #[test]
    fn pasted_lines_become_one_paste() {
        assert_eq!(
            burst_text(&typed("first line\nsecond")).as_deref(),
            Some("first line\nsecond")
        );
    }

    #[test]
    fn quick_typing_then_enter_still_submits() {
        assert_eq!(burst_text(&typed("ok\n")), None);
        assert_eq!(burst_text(&typed("abc")), None);
    }

    #[test]
    fn long_paste_ending_in_newline_is_a_paste() {
        assert!(burst_text(&typed("a whole pasted sentence.\n")).is_some());
    }

    #[test]
    fn ctrl_keys_break_a_burst() {
        let mut events = typed("a\nb");
        events.push(Event::Key(KeyEvent::new(
            KeyCode::Char('c'),
            KeyModifiers::CONTROL,
        )));
        assert_eq!(burst_text(&events), None);
    }
}
