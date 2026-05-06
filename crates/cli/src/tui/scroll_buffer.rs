use ratatui::text::Line;
use std::collections::VecDeque;

const MAX_CACHE_LINES: usize = 3000;

pub struct ScrollBuffer {
    lines: VecDeque<Line<'static>>,
    scroll_offset: usize,
    sticky_bottom: bool,
    cached_width: usize,
}

impl Default for ScrollBuffer {
    fn default() -> Self {
        Self::new(MAX_CACHE_LINES)
    }
}

impl ScrollBuffer {
    pub fn new(capacity: usize) -> Self {
        let cap = capacity.clamp(1, MAX_CACHE_LINES);
        Self {
            lines: VecDeque::with_capacity(cap),
            scroll_offset: 0,
            sticky_bottom: true,
            cached_width: 80,
        }
    }

    pub fn replace_lines(&mut self, lines: impl IntoIterator<Item = Line<'static>>) {
        self.lines.clear();
        for line in lines {
            self.lines.push_back(line);
        }
        while self.lines.len() > MAX_CACHE_LINES {
            self.lines.pop_front();
        }
        if self.sticky_bottom {
            self.scroll_offset = 0;
        } else {
            let max_offset = self.max_offset(1);
            self.scroll_offset = self.scroll_offset.min(max_offset);
        }
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_sticky(&self) -> bool {
        self.sticky_bottom
    }

    pub fn scroll_up(&mut self, n: usize, width: usize, viewport_height: usize) {
        self.cached_width = width.max(1);
        let max_offset = self.max_offset(viewport_height);
        self.scroll_offset = (self.scroll_offset + n).min(max_offset);
        self.sticky_bottom = false;
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(n);
        if self.scroll_offset == 0 {
            self.sticky_bottom = true;
        }
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
        self.sticky_bottom = true;
    }

    pub fn total_visual_lines(&self, width: usize) -> usize {
        let _ = width;
        self.lines.len()
    }

    pub fn scroll_position_from_top(&self, viewport_height: usize, width: usize) -> (u16, u16) {
        let total = self.total_visual_lines(width.max(1));
        let from_top = total
            .saturating_sub(viewport_height)
            .saturating_sub(self.scroll_offset);
        (from_top.min(u16::MAX as usize) as u16, 0)
    }

    pub fn set_from_top(&mut self, from_top: usize, viewport_height: usize, width: usize) {
        let total = self.total_visual_lines(width.max(1));
        let max_from_top = total.saturating_sub(viewport_height);
        let clamped = from_top.min(max_from_top);
        self.scroll_offset = max_from_top.saturating_sub(clamped);
        self.sticky_bottom = self.scroll_offset == 0;
    }

    fn max_offset(&self, viewport_height: usize) -> usize {
        self.lines.len().saturating_sub(viewport_height)
    }
}
