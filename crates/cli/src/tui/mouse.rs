//! Small mouse/pointer helpers: hit-testing rects, detecting left-click
//! release, filtering click jitter, and computing scroll step from modifiers.
//! Extracted from `tui::app`.

use crossterm::event::{KeyModifiers, MouseButton, MouseEventKind};
use ratatui::layout::Rect;

pub(crate) fn rect_contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x
        && col < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
}

pub(crate) fn mouse_left_activated(kind: MouseEventKind) -> bool {
    matches!(kind, MouseEventKind::Up(MouseButton::Left))
}

pub(crate) fn is_click_jitter(selection: &crate::tui::mouse_select::Selection) -> bool {
    selection.anchor.row == selection.cursor.row
        && selection.anchor.col.abs_diff(selection.cursor.col) <= 1
}

pub(crate) fn mouse_scroll_step(
    modifiers: KeyModifiers,
    viewport_lines: usize,
    base_step: usize,
) -> usize {
    let base = base_step.max(1);
    if modifiers.contains(KeyModifiers::CONTROL) {
        viewport_lines.max(1).saturating_mul(3).max(base)
    } else if modifiers.contains(KeyModifiers::SHIFT) {
        viewport_lines.max(1).max(base)
    } else {
        base
    }
}
