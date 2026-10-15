//! Geometry helpers for the TUI: splitting the frame into transcript/status/
//! slash/input regions, sidebar fitting, and centering popups. Extracted from
//! `tui::app`.

use ratatui::layout::Rect;

pub(crate) fn layout_chunks(
    area: Rect,
    slash_h: u16,
    input_h: u16,
    queue_total: usize,
    activity_total: usize,
) -> (Rect, Rect, Option<Rect>, Rect) {
    let vp = crate::tui::tui_viewport::layout(area, slash_h, input_h, queue_total, activity_total);
    // Return transcript rect only (callers don't need header separately from here).
    (vp.transcript, vp.status, vp.slash, vp.input)
}

pub(crate) fn layout_with_sidebar(area: Rect, _sidebar_open: bool) -> (Rect, Option<Rect>) {
    // Fullscreen-only layout: right sidebar removed.
    // Context/session details are command-driven (/status, /config, etc.).
    (area, None)
}

pub(crate) fn centered_rect(area: Rect, width: u16, height: u16) -> Rect {
    const POPUP_W_PAD: u16 = 10;
    const POPUP_H_PAD: u16 = 3;
    let target_w = width.saturating_add(POPUP_W_PAD);
    let target_h = height.saturating_add(POPUP_H_PAD);
    let popup_w = target_w
        .min(area.width.saturating_sub(2).max(20))
        .min(area.width);
    let popup_h = target_h
        .min(area.height.saturating_sub(2).max(6))
        .min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(popup_w) / 2,
        area.y + area.height.saturating_sub(popup_h) / 2,
        popup_w,
        popup_h,
    )
}
