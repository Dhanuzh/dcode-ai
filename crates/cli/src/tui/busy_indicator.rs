//! Advanced animated busy indicator (Claude-style).

use dcode_ai_common::event::BusyState;
use ratatui::style::Color;
use std::time::Instant;

/// Animation frames for different busy states.
const THINKING_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const STREAMING_FRAMES: &[&str] = &[
    "▁▂▃",
    "▂▃▄",
    "▃▄▅",
    "▄▅▆",
    "▅▆▇",
    "▆▇█",
    "▇█▇",
    "█▇▆",
    "▇▆▅",
    "▆▅▄",
    "▅▄▃",
    "▄▃▂",
];
const TOOL_FRAMES: &[&str] = &["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];
const APPROVAL_FRAMES: &[&str] = &["◇", "◈", "◆", "◈"];

/// Per-state frame interval (ms). Smaller = faster spin.
fn frame_interval_ms(state: BusyState) -> u128 {
    match state {
        BusyState::Thinking => 90,
        BusyState::Streaming => 70,
        BusyState::ToolRunning => 100,
        BusyState::ApprovalPending => 280,
        _ => 120,
    }
}

/// Get the color for a given busy state.
pub fn color_for_state(state: BusyState) -> Color {
    match state {
        BusyState::Idle => Color::Rgb(74, 222, 128),     // Green
        BusyState::Thinking => Color::Rgb(180, 120, 80), // Brown
        BusyState::Streaming => Color::Rgb(255, 165, 0), // Orange
        BusyState::ToolRunning => Color::Rgb(94, 234, 212), // Cyan/Teal
        BusyState::ApprovalPending => Color::Rgb(251, 191, 36), // Amber/Yellow
        BusyState::Error => Color::Rgb(248, 113, 113),   // Red
    }
}

/// Get the animated frame for a given state and elapsed time.
pub fn frame_for_state(state: BusyState, elapsed_ms: u128) -> &'static str {
    let frames = match state {
        BusyState::Thinking => THINKING_FRAMES,
        BusyState::Streaming => STREAMING_FRAMES,
        BusyState::ToolRunning => TOOL_FRAMES,
        BusyState::ApprovalPending => APPROVAL_FRAMES,
        _ => return "●",
    };

    let interval = frame_interval_ms(state);
    let frame_idx = (elapsed_ms / interval) as usize % frames.len();
    frames[frame_idx]
}

/// Animated trailing dots for any state. Cycles "", ".", "..", "..." every 350ms.
pub fn trailing_dots(elapsed_ms: u128) -> &'static str {
    const D: &[&str] = &["", ".", "..", "..."];
    D[(elapsed_ms / 350) as usize % D.len()]
}

/// Build the busy indicator span with animation.
pub fn render_indicator(state: BusyState, state_since: Instant) -> String {
    let elapsed_ms = state_since.elapsed().as_millis();
    let frame = frame_for_state(state, elapsed_ms);
    let label = state.label();

    match state {
        BusyState::Idle => format!(" ○ {label} "),
        BusyState::Error => format!(" ✕ {label} "),
        _ => {
            let dots = trailing_dots(elapsed_ms);
            format!(" {frame} {label}{dots} ")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_for_idle_is_green() {
        let c = color_for_state(BusyState::Idle);
        assert_eq!(c, Color::Rgb(74, 222, 128));
    }

    #[test]
    fn color_for_thinking_is_brown() {
        let c = color_for_state(BusyState::Thinking);
        assert_eq!(c, Color::Rgb(180, 120, 80));
    }

    #[test]
    fn color_for_streaming_is_orange() {
        let c = color_for_state(BusyState::Streaming);
        assert_eq!(c, Color::Rgb(255, 165, 0));
    }

    #[test]
    fn frame_cycles_through_thinking_frames() {
        let interval = frame_interval_ms(BusyState::Thinking);
        let f0 = frame_for_state(BusyState::Thinking, 0);
        let f1 = frame_for_state(BusyState::Thinking, interval);
        let f2 = frame_for_state(BusyState::Thinking, interval * 2);
        let wrap = frame_for_state(
            BusyState::Thinking,
            interval * THINKING_FRAMES.len() as u128,
        );
        assert_eq!(f0, THINKING_FRAMES[0]);
        assert_eq!(f1, THINKING_FRAMES[1]);
        assert_eq!(f2, THINKING_FRAMES[2]);
        assert_eq!(wrap, THINKING_FRAMES[0]);
    }

    #[test]
    fn streaming_frames_advance() {
        let interval = frame_interval_ms(BusyState::Streaming);
        let a = frame_for_state(BusyState::Streaming, 0);
        let b = frame_for_state(BusyState::Streaming, interval);
        assert_ne!(a, b);
    }

    #[test]
    fn render_indicator_idle() {
        let ind = render_indicator(BusyState::Idle, Instant::now());
        assert!(ind.contains("idle"));
        assert!(ind.contains("○"));
    }

    #[test]
    fn render_indicator_thinking_contains_braille_frame() {
        let ind = render_indicator(BusyState::Thinking, Instant::now());
        assert!(ind.contains("thinking"));
        assert!(THINKING_FRAMES.iter().any(|f| ind.contains(f)));
    }

    #[test]
    fn trailing_dots_cycle() {
        assert_eq!(trailing_dots(0), "");
        assert_eq!(trailing_dots(350), ".");
        assert_eq!(trailing_dots(700), "..");
        assert_eq!(trailing_dots(1050), "...");
        assert_eq!(trailing_dots(1400), "");
    }
}
