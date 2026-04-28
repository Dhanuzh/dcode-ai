# Codex-Style TUI Neatness + Responsiveness Pass

## Goal

Bring `dcode-ai` TUI closer to Codex-grade presentation quality: cleaner message/process layout, clearer tool lifecycle visibility, tighter spacing, and smoother responsiveness under streaming and tool-heavy runs.

## Codex UI Patterns Applied

- Dedicated live process row near composer (what is happening now, not buried in transcript).
- Clear separation between user/assistant/thinking/tool states with restrained spacing.
- Controlled verbosity for noisy blocks (thinking, tool raw input) to avoid transcript flooding.
- Keep interactive responsiveness stable by limiting unnecessary reflow/re-render work.

## Scope

- Add explicit process status fields in TUI state and update them from runtime events.
- Render a second status row showing current process + detail (thinking/tool/approval/question/error).
- Improve transcript neatness:
  - concise assistant/user headers
  - cap overly long thinking/tool raw-input sections with overflow indicators
- Keep queue indicators + busy controls visible but less noisy.
- Keep protocol/event contracts unchanged.

## Out of Scope

- Replacing ratatui architecture.
- Rewriting runtime event model.
- Changing session storage or IPC schema.

## Implementation

- [x] Extend `TuiSessionState` with live process title/detail fields.
- [x] Update `apply_event` transitions to maintain process title/detail.
- [x] Render process row in `tui/app.rs` status region.
- [x] Add line caps + overflow hints for thinking/tool raw input sections.
- [x] Tighten transcript labels/spacing for user + assistant blocks.
- [x] Run format + full CLI tests.

## Validation

- `cargo test -p dcode-ai-cli`
