# Drag-and-drop image path autostage fix

## Goal

When a user drags an image file into the full-screen TUI composer, the path should be staged as a real image attachment automatically before send, so the model receives native image input instead of plain path text.

## Root cause

Current image staging is triggered only by:

- `Event::Paste(...)` (`stage_pasted_image_paths`)
- `Ctrl+V` clipboard image flow
- `/image` command

Some terminal drag-and-drop flows insert text via key/input events rather than `Event::Paste`, so the image path remains plain composer text and is never staged.

## Scope

- TUI-only fix in `crates/cli/src/tui/app.rs`
- Keep existing `/image` and `Ctrl+V` behavior unchanged
- Keep provider-side fail-loud behavior for non-vision models unchanged

## Implementation

1. Add a shared parser/normalizer for candidate image path lines:
   - Trim surrounding quotes
   - Accept `file://...` URI form
   - Decode `%20` and other percent-escapes in file URIs
   - Unescape common shell backslash escapes (`\ `, `\(`, etc.)
2. Reuse parser in existing `stage_pasted_image_paths` logic.
3. On `Enter` submit path (before dispatching `TuiCmd::Submit`), run the same staging pass against expanded composer text so drag-inserted path text is detected even when no paste event fired.
4. If staging succeeds, continue submission normally (prompt text + native image attachment).
5. If explicit image path candidates were detected but import fails, surface `[image] ...` error in transcript and still allow text submission.

## Validation

- Unit tests for parser normalization (quoted, `file://`, percent-encoded, escaped-space forms)
- Unit test that non-image or non-path lines are ignored
- `cargo test -p dcode-ai-cli` targeted to the TUI test module

