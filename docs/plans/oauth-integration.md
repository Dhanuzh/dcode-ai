# dcode-ai OAuth-first provider integration plan

## Goal
Make `dcode-ai` OAuth-first (no API-key-only onboarding) with these providers:
- Anthropic OAuth login
- OpenAI OAuth device login
- GitHub Copilot device login
- Antigravity OAuth login

Reference implementation source in sibling project:
- `/mnt/d/d-projects/rust-projects/d-code/crates/dcode-providers/src/{anthropic.rs,openai.rs,copilot.rs,antigravity.rs,oauth.rs,types.rs}`
- `/mnt/d/d-projects/rust-projects/d-code/crates/dcode-cli/src/login.rs`

## Current gap in dcode-ai
- Providers in `dcode-ai` are currently API-key driven via `DcodeAiConfig`.
- TUI onboarding and `/apikey` flow assume API key entry.
- Provider list does not include `copilot` and `antigravity`.

## Execution phases

### Phase 1 — Project fork + identity
1. Keep cloned repo in `/mnt/d/d-projects/rust-projects/dcode-ai`.
2. Rename public identity:
   - binary name `dcode-ai` -> `dcode-ai`
   - docs/user-agent strings from `dcode-ai` -> `dcode-ai`

### Phase 2 — Shared auth store + OAuth utils
1. Add auth store module in `crates/common` (or `crates/core`):
   - persisted `~/.dcode-ai/auth.json`
   - structs: `ProviderAuth`, `OpenAiOAuth`, `CopilotAuth`, `AntigravityAuth`
2. Add PKCE utilities (`generate_pkce`, `url_encode`).
3. Add secure token refresh helpers for OpenAI + Antigravity.

### Phase 3 — CLI login/logout/status commands
1. Add commands:
   - `dcode-ai login <anthropic|openai|copilot|antigravity>`
   - `dcode-ai logout <provider|all>`
   - `dcode-ai auth status`
2. Add slash commands:
   - `/login`, `/logout`, `/auth`
3. Keep `/apikey` available only as temporary fallback (hidden/deprecated).

### Phase 4 — Provider runtime integration
1. Extend provider enum with `Copilot` and `Antigravity`.
2. Update provider factory in `crates/core/src/provider/factory.rs`.
3. Implement runtime adapters for:
   - Anthropic OAuth-backed requests
   - OpenAI OAuth-backed requests (+ auto-refresh)
   - Copilot chat endpoint flow (+ token exchange)
   - Antigravity endpoint flow (+ token refresh)

### Phase 5 — OAuth-first onboarding
1. Replace API key modal with provider OAuth connect modal.
2. If not authenticated, trigger `/login <provider>` flow from TUI.
3. `needs_onboarding()` should check OAuth auth store instead of API key presence.

### Phase 6 — Hardening and performance
1. Add token cache + proactive refresh (avoid request stalls).
2. Add retries/backoff for auth and streaming endpoints.
3. Add provider health checks in `/doctor`.
4. Add integration tests for each login + first prompt.

## Deliverable order (recommended)
1. OpenAI OAuth end-to-end
2. Anthropic OAuth end-to-end
3. Copilot end-to-end
4. Antigravity end-to-end
5. Remove API-key-first onboarding paths

## Notes
- Full migration is non-trivial because `dcode-ai` architecture differs from `d-code`.
- Reusing code is possible, but adapters are required for `dcode_ai_common::message` and provider trait differences.
