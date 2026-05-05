# TUI popup size increase

## Goal

Increase popup/modal sizes across TUI so content is easier to read and less cramped.

## Scope

- `crates/cli/src/tui/app.rs`
- `crates/cli/src/tui/onboarding.rs`

## Approach

- Increase popup dimensions centrally in `centered_rect(...)` helpers by applying width/height padding before clamping to terminal size.
- Keep safe bounds so very small terminals still render correctly.

## Validation

- `cargo fmt`
- `cargo build --release -p dcode-ai-cli`

