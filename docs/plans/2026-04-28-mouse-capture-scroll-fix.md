# Mouse Capture + Scroll Stability Plan

## Goal

Fix TUI mouse issues where text selection and wheel scrolling interfere with normal usage.

## Findings

- `setup_terminal(mouse_capture)` currently ignores its argument and always enables mouse capture sequences.
- `TuiSessionState::new()` initializes `mouse_capture_on = true`, which can disagree with config (`ui.mouse_capture`).
- REPL/TUI startup does not sync runtime config into state before first render.
- Local Codex reference (`codex/codex-rs`) does not rely on direct mouse capture in its TUI loop, reducing selection conflicts.

## Implementation

- Honor `mouse_capture` flag in terminal setup.
- Keep restore path always disabling capture (safe cleanup).
- Initialize `mouse_capture_on` to config value at TUI startup.
- Default `mouse_capture_on` in state to `false` to match config defaults and avoid accidental capture in tests/new callsites.

## Validation

- `cargo test -p dcode-ai-cli mouse_capture`
- `cargo test -p dcode-ai-cli run_with_tui`
