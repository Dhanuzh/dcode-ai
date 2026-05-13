# Sub-agent Error Surface Fix (2026-05-12)

## Problem

When a spawned sub-agent fails, parent `dcode-ai` UI often shows only:

- `Sub-agent ... done: error`
- `Sub-agent finished with status: error`

The underlying child failure reason is not visible inline, making diagnosis slow.

## Goal

Surface child failure reason directly in parent-visible output while preserving existing success semantics.

## Plan

1. Add runtime-side status detail for failed child completions:
   - Keep status as `"completed"` on success.
   - On failure, emit `"error: <short child error>"` in `ChildSessionCompleted.status`.
2. Improve `spawn_subagent` tool failure message:
   - Include shortened child `output` reason in `ToolResult.error` for non-completed child runs.
3. Verify with crate-level checks:
   - `cargo test -p dcode-ai-core`
   - `cargo test -p dcode-ai-runtime`

## Notes

- No protocol/schema change required.
- Success path remains unchanged.
