# Copilot Provider + OpenAI Model Visibility Plan (2026-05-04)

## Goal
Fix provider UX so Copilot is visible/selectable, and ensure OpenAI model lists appear when user is logged in via OAuth (without requiring API key in config).

## Problems
1. Connect modal does not show Copilot, so users cannot discover/select it.
2. `/models` OpenAI list can be empty when only OAuth login exists, because model fetch path only checks API key.
3. Connect modal status/action logic cannot distinguish OpenAI vs Copilot row under current simple mapping.

## Reference
Use behavior parity from `d-code`:
- Explicit provider presence for `copilot` in provider lists.
- OAuth-based model/provider operation without forcing API key in config.

## Changes
1. Extend connect modal catalog entries with optional OAuth login slug.
2. Add Copilot row to connect modal.
3. Update connect modal rendering and selection handling to:
   - show correct "logged in" status for Copilot
   - route Enter to `/login copilot` for Copilot row.
4. Update OpenAI model fetch path to fall back to `auth.openai_oauth.access_token` when no API key is configured.
5. Add stable static fallback list for OpenAI models when remote fetch is unavailable.
6. Run fmt and CLI tests.

## Validation
- `cargo fmt`
- `cargo test -p dcode-ai-cli`
