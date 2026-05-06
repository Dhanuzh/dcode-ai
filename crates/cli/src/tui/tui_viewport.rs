use crate::tui::widgets::{
    child_activity_overlay::ChildActivityOverlay, queue_preview::QueuePreview,
    status_bar::StatusBar,
};
use ratatui::layout::{Constraint, Layout, Rect};

#[derive(Debug, Clone, Copy)]
pub struct ViewportLayout {
    pub transcript: Rect,
    pub status: Rect,
    pub slash: Option<Rect>,
    pub input: Rect,
    pub queue_preview: Rect,
    pub activity_overlay: Rect,
}

pub fn layout(
    area: Rect,
    slash_h: u16,
    input_h: u16,
    queue_total: usize,
    activity_total: usize,
) -> ViewportLayout {
    let input_h = input_h.max(3);
    let queue_h = QueuePreview::height_for(queue_total);
    let activity_h = ChildActivityOverlay::height_for(activity_total);

    if slash_h > 0 {
        let c = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(1),
            Constraint::Length(activity_h),
            Constraint::Length(queue_h),
            Constraint::Length(slash_h),
            Constraint::Length(input_h),
        ])
        .split(area);
        ViewportLayout {
            transcript: c[0],
            status: c[1],
            activity_overlay: c[2],
            queue_preview: c[3],
            slash: Some(c[4]),
            input: c[5],
        }
    } else {
        let c = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(1),
            Constraint::Length(activity_h),
            Constraint::Length(queue_h),
            Constraint::Length(input_h),
        ])
        .split(area);
        ViewportLayout {
            transcript: c[0],
            status: c[1],
            activity_overlay: c[2],
            queue_preview: c[3],
            slash: None,
            input: c[4],
        }
    }
}

pub fn render_queue_preview(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    items: &[String],
    total: usize,
) {
    if area.height == 0 || total == 0 {
        return;
    }
    frame.render_widget(QueuePreview::new(items, total), area);
}

pub fn render_activity_overlay(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    rows: &[crate::tui::widgets::child_activity_overlay::ActivityRow],
    total: usize,
) {
    if area.height == 0 || total == 0 {
        return;
    }
    frame.render_widget(ChildActivityOverlay::new(rows, total), area);
}

pub fn render_status_bar(frame: &mut ratatui::Frame<'_>, area: Rect, status: StatusBar<'_>) {
    if area.height == 0 {
        return;
    }
    frame.render_widget(status, area);
}
