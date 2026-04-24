# TUI Rich Markdown Preview Phase 2 Plan

## Goal

Move assistant transcript rendering from line-heuristic markdown styling to parser-driven markdown rendering with syntax-highlighted code blocks.

## Implementation

- Add a pulldown-cmark based renderer in `crates/cli/src/tui/app.rs` that converts markdown events into `ratatui::text::Line`/`Span`.
- Track inline style state (bold, italic, strike, links) and block context (headings, blockquotes, lists).
- Add fenced code block rendering using `syntect`:
  - detect language token from fence info
  - highlight with default syntect theme
  - convert syntect RGB colors into ratatui colors
- Use the new renderer for:
  - committed assistant messages
  - streaming assistant text
- Keep existing simpler parser as fallback helper/tests while primary TUI path uses parser-driven output.

## Validation

- `cargo fmt`
- `cargo test -p dcode-ai-cli parse_md_line_styles`
- `cargo test -p dcode-ai-cli markdown_event_renderer`
