# Live Provider Login + Session Name UX Plan (2026-05-04)

## Goal
Fix provider/login UX so OAuth providers can be connected and used immediately in the same `dcode-ai` session, and add explicit manual session naming.

## Issues Observed
1. `/login <provider>` in REPL/TUI prints shell guidance instead of running OAuth flow in-process.
2. After successful OAuth login, active runtime provider/config does not switch live, which makes users think login failed until restart.
3. `/provider` in TUI opens provider picker, but users expect a connect/login popup workflow.
4. Provider label can show only `Copilot` when OpenAI surface is configured for Copilot endpoint, which is confusing.
5. Session naming is auto-derived, but users also need a direct command to set/clear names.

## Implementation Steps
1. Add REPL helper to parse OAuth provider names and execute `oauth_login::login(...)` inline.
2. After login success, apply provider config to runtime immediately:
   - switch default provider
   - set provider-specific base URL where needed (`openai` vs `copilot` endpoint)
   - rebuild provider via `apply_dcode_ai_config`
   - persist to global config
3. Change `/provider` (no args, TUI) to open connect modal (`/connect`) for login-first UX.
4. Add `/session-name [text]` command:
   - no arg: show current name
   - non-empty arg: set name
   - `clear`: remove name
   - persist via session save
5. Adjust provider labeling so OpenAI remains visible while indicating Copilot endpoint.
6. Run format + targeted tests for `common`, `runtime`, and `cli`.

## Validation
- `cargo fmt`
- `cargo test -p dcode-ai-common`
- `cargo test -p dcode-ai-runtime`
- `cargo test -p dcode-ai-cli`
