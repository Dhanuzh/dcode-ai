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
    let input_h = input_h.min(area.height);
    let queue_h = QueuePreview::height_for(queue_total);
    let activity_h = ChildActivityOverlay::height_for(activity_total);

    if slash_h > 0 {
        let c = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(activity_h),
            Constraint::Length(queue_h),
            Constraint::Length(slash_h),
            Constraint::Length(input_h),
            Constraint::Length(1),
            Constraint::Length(2),
        ])
        .split(area);
        ViewportLayout {
            transcript: c[0],
            activity_overlay: c[1],
            queue_preview: c[2],
            slash: Some(c[3]),
            input: c[4],
            status: c[5],
        }
    } else {
        let c = Layout::vertical([
            Constraint::Min(4),
            Constraint::Length(activity_h),
            Constraint::Length(queue_h),
            Constraint::Length(input_h),
            Constraint::Length(1),
            Constraint::Length(2),
        ])
        .split(area);
        ViewportLayout {
            transcript: c[0],
            activity_overlay: c[1],
            queue_preview: c[2],
            input: c[3],
            status: c[4],
            slash: None,
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
