# Best dcode-ai UI + Performance Pass

## Goal

Ship a high-impact CLI/TUI upgrade for `dcode-ai` by adopting proven interaction patterns from top agent CLIs (`codex`, `pi-mono`, `gemini-cli`) while keeping the existing Rust architecture and MiniMax-first provider flow.

## External CLI Findings Applied

- `pi-mono` emphasizes dense but legible terminal surfaces: clear startup/status bands, explicit keyboard hints, and queue-aware interaction.
- `codex` and `gemini-cli` both treat structured event streaming and script-safe outputs as first-class surfaces.
- Strong agent CLIs avoid expensive full redraw work when transcript content has not changed.

## Scope

- Add transcript render caching in the TUI to avoid repeated markdown parsing/highlighting on unchanged frames.
- Reuse cached transcript/hit maps for mouse scroll/click handling to reduce duplicate work.
- Improve assistant response affordances in transcript rows (copy and quick response-size signal) so AI output is easier to consume.
- Keep JSON/NDJSON and IPC event contracts unchanged.

## Out of Scope

- Replacing Ratatui layout architecture.
- Rewriting provider transport layer.
- Changing session persistence format.

## Implementation Steps

- [x] Add transcript revision tracking in TUI state and mark transcript changes at mutation points.
- [x] Add a local run-loop transcript cache keyed by `(width, transcript_revision)`.
- [x] Route draw + mouse handlers through the cache instead of recomputing transcript lines.
- [x] Add richer assistant header row (copy affordance + compact size metric).
- [x] Run focused CLI tests for markdown rendering and TUI interaction behavior.

## Validation

- `cargo test -p dcode-ai-cli tui::app::tests::markdown_event_renderer_supports_code_line_numbers_and_copy_hits`
- `cargo test -p dcode-ai-cli tui::app::tests::request_turn_cancel_is_idempotent`
- `cargo test -p dcode-ai-cli tui::state::tests`
