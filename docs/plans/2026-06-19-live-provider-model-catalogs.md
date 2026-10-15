# Live provider model catalogs

## Goal

Make every model-picker and `dcode-ai models` result come from the active provider at runtime. Remove built-in model ID catalogs and never disguise discovery failures as a successful, stale list.

## Scope

1. Change runtime model discovery to return `Result<Vec<String>, ModelCatalogError>`.
2. Query the active provider's real model endpoint for OpenCode Zen, OpenAI, Anthropic, OpenRouter, ChatGPT Codex, and Copilot surfaces.
3. Remove all static fallback model-ID lists from production discovery code.
4. Make `dcode-ai models` asynchronous and show the live active-provider catalog in human and JSON output.
5. Update REPL/TUI callers to display explicit discovery errors while preserving the currently configured model as a usable picker entry.
6. Add focused tests for response parsing and loud failure behavior.
7. Update provider/model documentation and run format, targeted tests, check, and clippy.

## Invariants

- User-configured model values remain supported; configuration is not a model catalog.
- A provider HTTP/auth/parse failure returns an error, not an empty or hardcoded success result.
- Empty successful catalogs are reported as errors.
- MiniMax/OpenCode Zen remains the primary provider surface.
- No JavaScript, Node.js, or Electron code is introduced.

## Non-goals

- Guessing pricing, context size, or multimodal capability from incomplete provider catalog fields.
- Automatically changing a user's configured model.
- Treating test fixtures and documentation examples as production model catalogs.

## Verification

- Runtime unit tests for provider catalog parsing and errors.
- CLI command tests for live-catalog JSON shape where networking is mocked or disabled.
- `cargo fmt --check --all`
- `cargo check --workspace`
- `cargo clippy --workspace -- -D warnings`

## Follow-up: Copilot 404 compatibility

The Copilot token exchange returns HTTP 404 for some valid accounts. The chat
provider already handles this by using the stored GitHub token directly. Model
discovery must share that behavior, then query Copilot's live `/models`
endpoint. If the live endpoint remains unavailable, the picker stays empty and
reports unavailability; it must never substitute a built-in model list.
