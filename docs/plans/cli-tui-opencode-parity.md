# CLI TUI (OpenCode-style DX)

## Goal

Full-screen ratatui session driven by `AgentEvent` (streaming, tools, cost), with fixed composer and status bar—default on TTY when using human stream mode.

## Behavior

- **`--no-tui`**: legacy Reedline + line printer (`StreamMode::Human` still prints events).
- **Default (TTY)**: TUI consumes the event channel; no duplicate stdout streaming.
- **Approvals**: `cli-prompts` may fight alternate screen; use `--no-tui` if prompts break.

## Wiring

1. `spawn_tui_bridge`: disk log + IPC broadcast + `TuiSessionState::apply_event`.
2. `run_blocking`: crossterm loop, `UnboundedSender` for submit / Tab / Ctrl+C / Esc.
3. `Repl::run_with_tui`: bridges UI commands to `run_turn`, slash commands, `!`, `@`.

## Implemented UX

- Mouse wheel on transcript scrolls (with `EnableMouseCapture`).
- Typing `/` (no space yet) opens **commands** panel; filter as you type; ↑↓ select; **Tab** complete; click to pick.

## Future

- In-TUI approval (y/n) without leaving alternate screen.
- Dynamic skills in slash menu (same as Reedline).
- Session sidebar / file tree (Conductor-style).
