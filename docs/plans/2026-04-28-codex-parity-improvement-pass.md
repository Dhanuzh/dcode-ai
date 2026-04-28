# Codex Parity Improvement Pass

## Goal

Improve `dcode-ai` as a Rust-native CLI agent by adopting high-value patterns from the local `codex/` reference without changing the product shape or replacing the existing runtime.

## Reference Findings

- Codex keeps terminal interaction conservative: it avoids forced mouse capture and leans on terminal scrollback where possible.
- Codex presents tool execution as structured activity, not raw JSON-heavy rows.
- Codex treats compaction as a first-class lifecycle with visible state and reliable fallback paths.
- Codex keeps tool approval and execution metadata consistent across UI surfaces.

## dcode-ai Fixes In Scope

- Honor configured mouse capture so terminal text selection works by default.
- Keep context compaction proactive enough to avoid provider-side context errors.
- Render tool activity with shared metadata, concise previews, and consistent completed-state messages.
- Keep JSON and NDJSON event contracts unchanged for scripts and IPC consumers.

## Out Of Scope

- Full Codex inline scrollback architecture port.
- Replacing the provider stack.
- Rewriting session storage or IPC protocol.

## Validation

- `cargo test -p dcode-ai-cli tool_ui`
- `cargo test -p dcode-ai-cli activity`
- `cargo test -p dcode-ai-cli tui_state_defaults_mouse_capture_off`
- `cargo test -p dcode-ai-runtime context_manager`
