# TUI Rich Markdown Preview Plan

## Goal

Improve assistant transcript readability in `dcode-ai` TUI with richer markdown-aware styling.

## Implementation

- Extend per-line markdown parsing in `crates/cli/src/tui/app.rs`.
- Add inline styling support for:
  - `**bold**`
  - `*italic*` / `_italic_`
  - `` `inline code` ``
  - `~~strikethrough~~`
  - `[label](url)` links (label styled, URL shown muted)
- Add block-style handling for:
  - ATX headings (`#`..`######`)
  - blockquotes (`> `)
  - unordered lists (`-`, `*`, `+`)
  - ordered list markers (`1.`, `2.`, ...)
  - fenced code delimiters (```)
  - horizontal rule lines (`---`)
- Keep rendering Rust-native with ratatui `Line`/`Span` styles.
- Add unit tests for key markdown rendering behaviors.

## Validation

- `cargo fmt`
- `cargo test -p dcode-ai-cli parse_md_line_styles`
