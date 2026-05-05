# TUI OAuth output render fix

## Goal

Prevent provider login flows from corrupting full-screen TUI layout.

## Root cause

`oauth_login` emits progress/instructions with direct `println!` (stdout). In TUI mode this bypasses transcript rendering and draws text over the bottom bar/composer.

## Scope

- `crates/cli/src/oauth_login.rs`
- `crates/cli/src/repl.rs`

## Implementation

1. Add output-aware auth APIs:
   - `login_with_output(provider, emit)`
   - `logout_with_output(target, emit)`
2. Keep existing public `login`/`logout` wrappers for CLI compatibility (stdout behavior retained there).
3. Refactor OAuth provider flows to report status via injected `emit` callback instead of direct `println!`.
4. In REPL/TUI `/login` and `/logout` handlers, call output-aware APIs and route messages through `ReplOutput`.

## Validation

- Build `dcode-ai-cli` and run targeted REPL tests.
- Manual TUI check: `/provider` -> select OAuth provider, verify instructions appear in transcript without breaking footer/composer rendering.

