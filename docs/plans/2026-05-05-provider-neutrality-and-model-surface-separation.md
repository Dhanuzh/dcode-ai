# Provider neutrality + model surface separation

## Goal

1. Remove recommendation bias in provider onboarding/connect UI.
2. Show explicit fallback when no provider is connected and no models are available.
3. Keep OpenAI and Copilot model surfaces separate in model picker and `/models` output.

## Changes

- Remove `(recommended)` tag from OpenAI onboarding option.
- Simplify connect modal list to neutral provider rows without section headers.
- Add active-surface connection detection in REPL model tooling.
- In model picker:
  - Show `Copilot` as a separate surface row.
  - Avoid showing Copilot auth as OpenAI auth.
  - Show fallback lines when provider isn’t connected or models are unavailable.
- In `/models` (stdio):
  - print `no provider connected ...` fallback when disconnected
  - print `no models available ...` when model list is empty
  - include active surface label in model list heading.

## Validation

- `cargo test -p dcode-ai-cli` for connect-modal + repl parser tests
- manual `/models` checks in both OpenAI and Copilot surfaces

