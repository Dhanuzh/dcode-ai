# Question Modal Picker Design

**Issue:** [#32](https://github.com/madebyaris/dcode-ai/issues/32)
**Date:** 2026-03-24
**Status:** Approved

## Summary

Replace the current number-key-based Ask Question option selection with a modal popup using arrow key navigation, matching the UX of existing pickers (model, branch, permission, agent). Include a "Chat about this" option (when custom answers are allowed) that falls back to inline free-text input.

## Motivation

The current Ask Question tool displays options as `[0] suggested`, `[1] option`, `[2] option` and requires the user to type a number. This is inconsistent with the rest of the TUI, which uses arrow-key navigation in modal pickers. The goal is a unified, readable, Claude Code-like selection experience.

## Design

### State & Data Model

New fields in `TuiSessionState`:

```rust
question_modal_open: bool,
question_modal_index: usize,           // currently highlighted option (0 = suggested)
question_modal_scroll: usize,          // viewport scroll offset for long option lists
```

The modal reads from the existing `active_question: Option<InteractiveQuestionPayload>` — no separate payload copy. This is consistent with how `active_approval` works.

The options list is built as:
1. **Suggested answer** (index 0, pre-selected, visually distinguished)
2. **Options from payload** (index 1..n)
3. **"Chat about this"** (last item, **only shown when `allow_custom` is true**)

Helper methods `open_question_modal()` / `close_question_modal()` follow the existing pattern (see `open_model_picker` / `close_model_picker` in `state.rs`):
- `open_question_modal()`: sets `question_modal_open = true`, `question_modal_index = 0`, `question_modal_scroll = 0`
- `close_question_modal()`: sets `question_modal_open = false`, resets index and scroll

No changes to `InteractiveQuestionPayload` or `QuestionSelection` enums.

### `allow_custom` Edge Case

When `allow_custom` is `false`:
- **"Chat about this"** option is **not shown** in the modal
- **Esc** is a **no-op** (modal stays open, user must pick an option)
- The user can only select the suggested answer or one of the listed options

When `allow_custom` is `true`:
- **"Chat about this"** is the last item in the list
- **Esc** closes the modal and falls back to inline text input

### Key Event Handling

When `question_modal_open` is true, key events are intercepted before any other handler (same priority as existing pickers):

| Key | Action |
|-----|--------|
| `↑` / `k` | Move highlight up, saturate at 0 |
| `↓` / `j` | Move highlight down, saturate at last item |
| `Enter` | Confirm selection |
| `Esc` | If `allow_custom`: close modal, fall back to inline text input. If `!allow_custom`: no-op |

No number keys, no search, no other modifiers.

The event handling slots into `handle_key_event` alongside the model/permission/agent picker blocks, checked early so it captures input before the composer. Mouse events on the transcript are swallowed while the modal is open (consistent with existing picker behavior at `app.rs:2593-2598`).

### Rendering

Modal uses `centered_rect()` (existing helper) to create a popup overlay:

```
┌─── Question ─────────────────────────┐
│                                      │
│  What would you like to do?          │
│                                      │
│  ► Suggested: refactor the module    │  ← highlighted
│    Option 1: rewrite from scratch    │
│    Option 2: add tests first         │
│    Chat about this                   │  ← only if allow_custom
│                                      │
│  ↑↓ select · Enter confirm · Esc     │
└──────────────────────────────────────┘
```

Styling (existing theme conventions):
- **Title**: question prompt text, `fg(theme::ASSISTANT)`, bold
- **Selected item**: `bg(theme::USER)`, `fg(Color::Black)`, bold, `►` prefix
- **Unselected items**: `fg(theme::TEXT)`, indented with spaces
- **"Chat about this"**: `fg(theme::MUTED)`, italic
- **Footer**: keybinding hints, `fg(theme::MUTED)`. When `!allow_custom`, omit "Esc" from hints
- **Border**: `Borders::ALL`, standard block

Modal width adapts to content (capped at ~60% terminal width). If options exceed visible area, viewport scrolling using `question_modal_scroll` (same logic as model picker's `MODEL_PICKER_MAX_ROWS`).

**Composer hint suppression:** While the question modal is open, the composer hint line (currently shows `"Enter / 0 = suggested..."`) is suppressed or replaced since the user interacts via the modal, not the composer.

**Empty options list:** When `options` is empty, the modal shows only "Suggested: ..." and (if `allow_custom`) "Chat about this". This is valid and works as expected.

### Integration & Flow

**Opening the modal:**
- When `active_question` is set (question payload arrives from core), call `open_question_modal()`.
- The existing inline rendering code for questions (~lines 977-1056 in `app.rs`) is bypassed when the modal is active.

**Closing the modal — selection paths:**

| Selection | Action |
|-----------|--------|
| Suggested answer | Send `QuestionSelection::Suggested` via existing channel, call `close_question_modal()`, clear `active_question` |
| Regular option | Send `QuestionSelection::Option { option_id }` via existing channel, call `close_question_modal()`, clear `active_question` |
| "Chat about this" | Call `close_question_modal()`, keep `active_question` alive, switch to inline text input mode (reuse existing `allow_custom` / `[c]` flow) |
| Esc (allow_custom) | Same as "Chat about this" |
| Esc (!allow_custom) | No-op, modal stays open |

**What stays the same:**
- `InteractiveQuestionPayload` struct — unchanged
- `QuestionSelection` enum — unchanged
- `parse_tui_question_answer` — kept as fallback for the inline text input path
- `/auto-answer` slash command — still works, bypasses modal entirely

### Testing

- Unit tests for index bounds (up at 0, down at last)
- Unit tests for selection mapping (index → correct `QuestionSelection` variant)
- Unit tests for `allow_custom: false` behavior (no "Chat about this", Esc is no-op)
- Manual verification of modal rendering and keyboard flow

## Files Affected

- `crates/cli/src/tui/state.rs` — add modal state fields, `open_question_modal()` / `close_question_modal()` methods
- `crates/cli/src/tui/app.rs` — rendering (new modal draw function), key event handling, question activation logic, composer hint suppression, mouse event swallowing
- `crates/core/src/tools/ask_question.rs` — no changes expected
