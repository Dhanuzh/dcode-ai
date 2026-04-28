# Tool UI Refresh Plan

## Goal

Improve how every tool call appears in the CLI by making tool rows easier to scan, giving each tool family a clear label/icon, and showing better argument/result previews in both the full TUI and human stream mode.

## Scope

- Add shared Rust-native tool display metadata for built-in, MCP, shell, web, file, git, validation, sub-agent, and question tools.
- Use that metadata in the TUI transcript for running, approval, completed, and system/activity tool rows.
- Use the same metadata in `--no-tui` human stream rendering.
- Keep JSON/NDJSON event payloads unchanged.

## Steps

- [x] Introduce shared tool UI helpers in the CLI crate.
- [x] Update TUI transcript rendering to use richer tool rows.
- [x] Update human stream rendering to match the improved labels.
- [x] Run focused formatting and tests.
