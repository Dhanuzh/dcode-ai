# Context Overflow Hardening Plan

## Goal

Reduce intermittent provider-side context errors and keep long-running `dcode-ai` sessions stable, especially for MiniMax-heavy workflows.

## Findings

- Pre-turn context check only emits warnings; it does not compact before the next provider request.
- Auto-summary prompt currently omits the actual conversation payload, so summaries are often low quality or empty.
- If AI summarization fails or returns empty text, fallback drops older context via sliding-window only, which can lose important state too aggressively.

## Implementation

- Add preflight compaction in `Supervisor` before each model request when context is already near/exceeding threshold.
- Improve `ContextManager::summary_prompt` to include formatted conversation content with bounded per-message excerpts.
- Add deterministic fallback extractive summary when AI summary output is empty.
- Add tests for summary prompt content and non-empty fallback summary behavior.

## Validation

- `cargo test -p dcode-ai-runtime context_manager`
- `cargo test -p dcode-ai-runtime supervisor`
