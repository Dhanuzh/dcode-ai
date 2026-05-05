# Provider connect OAuth + logout command fix

## Goal

Fix TUI provider-connect behavior so selecting an OAuth provider reliably launches OAuth flow, and add missing `/logout` command support in interactive mode.

## Problems observed

1. `OpenCode Zen` appears in connect modal with OAuth-backed status (`logged in`/`not logged in`) but row action is wired as non-OAuth (`oauth_login_slug: None`), so selection does not start OAuth login.
2. Interactive slash-command surface lacks `/logout` (and no easy `/auth` status alias), so users cannot manage login state from TUI/REPL.

## Scope

- `crates/cli/src/tui/connect_modal.rs`
- `crates/cli/src/slash_commands.rs`
- `crates/cli/src/repl.rs`

## Implementation

1. Set `OpenCode Zen` connect row to `oauth_login_slug: Some("opencodezen")` so Enter triggers `/login opencodezen`.
2. Add slash commands:
   - `/logout [provider|all]`
   - `/auth` (status)
3. Add REPL handler branches:
   - `/auth`: display auth status in TUI/REPL output.
   - `/logout`: parse provider target, call `oauth_login::logout(...)`, print result, and include usage/errors.
4. Keep existing provider/model switching behavior unchanged.

## Validation

- `cargo test -p dcode-ai-cli` targeted tests for connect rows and command list.
- `cargo test -p dcode-ai-cli approval_parse_tests -- --nocapture` sanity check for TUI module integrity.
