# OpenAI Codex + Anthropic OAuth Integration Plan (2026-05-04)

## Goal
Harden and complete OAuth login integration for:
- OpenAI Codex device flow (`dcode-ai login openai`)
- Anthropic PKCE flow (`dcode-ai login anthropic`)

Reference implementation reviewed from sibling project:
- `/mnt/d/d-projects/rust-projects/d-code/crates/dcode-providers/src/openai.rs`
- `/mnt/d/d-projects/rust-projects/d-code/crates/dcode-providers/src/anthropic.rs`
- `/mnt/d/d-projects/rust-projects/d-code/crates/dcode-cli/src/login.rs`

## Current State in `dcode-ai`
- OAuth command surface already exists (`login`, `logout`, `auth`).
- OpenAI OAuth token is saved, but runtime provider does not refresh expired tokens.
- OpenAI device-flow response parsing expects numeric `interval`, but upstream can return string.
- Anthropic pasted code path does not normalize callback fragments (e.g. `code#state=...`).

## Implementation Plan
1. Harden CLI OAuth login parsing:
   - Accept `interval` as number or string for OpenAI device flow.
   - Normalize Anthropic authorization code input before exchange.
2. Integrate OpenAI OAuth refresh into runtime provider:
   - Detect near-expired OAuth token.
   - Refresh via `grant_type=refresh_token`.
   - Persist refreshed token set back to `~/.dcode-ai/auth.json`.
   - Fail loudly if refresh fails and no API key fallback exists.
3. Keep provider priority rules unchanged:
   - Explicit API key remains highest precedence.
   - OAuth is preferred when API key is absent.
4. Validate with targeted tests and compile checks for `dcode-ai-cli` and `dcode-ai-core`.

## Success Criteria
- `dcode-ai login openai` is resilient to `interval` type changes from OpenAI auth endpoint.
- `dcode-ai login anthropic` accepts pasted codes even if callback fragment is present.
- Expired OpenAI OAuth sessions auto-refresh at runtime without manual re-login.
- Credential failures remain explicit and actionable (no silent success).
