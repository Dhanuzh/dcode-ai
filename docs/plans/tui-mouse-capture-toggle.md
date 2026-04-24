# TUI Mouse Capture Toggle Plan

## Goal

Allow users to select text with their terminal mouse by making TUI mouse capture configurable.

## Implementation

- Add a new UI config option: `[ui].mouse_capture` (bool).
- Default `mouse_capture` to `true` so in-app mouse interactions work out of the box.
- Update terminal setup to accept a `mouse_capture` flag and only send `EnableMouseCapture` when enabled.
- Track whether mouse capture was enabled so restore logic only disables it when needed.
- Wire the option into both full TUI and onboarding TUI paths.
- Document the new config key in `docs/documentation/configuration.md`.

## Validation

- `cargo test -p dcode-ai-common`
- `cargo test -p dcode-ai-cli`
- `cargo build --release`
