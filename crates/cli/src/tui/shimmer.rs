//! Animated shimmer / shine effect for loading states.
//!
//! A bright band sweeps across the text over time while the rest stays at the
//! base color — the classic "loading shine". Used on the status-bar `working`
//! label and other transient loading indicators.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

/// Build shimmering spans for `text`: a 2-3 char bright band sweeps left→right
/// on a ~900ms cycle; other characters use `base`. One span per character.
pub fn shimmer_spans(
    text: &str,
    elapsed_ms: u128,
    base: Color,
    shine: Color,
    bg: Color,
) -> Vec<Span<'static>> {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len().max(1) as f64;
    const PERIOD_MS: u128 = 900;
    // Sweep head position in [-2, n+2) so the band enters and exits cleanly.
    let phase = (elapsed_ms % PERIOD_MS) as f64 / PERIOD_MS as f64;
    let head = phase * (n + 4.0) - 2.0;

    chars
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            let dist = (i as f64 - head).abs();
            let style = if dist <= 0.6 {
                Style::default()
                    .fg(shine)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD)
            } else if dist <= 1.6 {
                Style::default().fg(shine).bg(bg)
            } else {
                Style::default().fg(base).bg(bg)
            };
            Span::styled(c.to_string(), style)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn produces_one_span_per_char() {
        let spans = shimmer_spans("working", 0, Color::Gray, Color::White, Color::Reset);
        assert_eq!(spans.len(), "working".chars().count());
    }
}
