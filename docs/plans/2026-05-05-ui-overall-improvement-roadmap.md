# UI Overall Improvement Roadmap

## Goal

Stabilize TUI interaction quality (mouse, scroll, selection, popups, transcript readability) and make the UI feel consistent across terminals (tmux/non-tmux).

## Current Findings (2026-05-05)

1. Mouse capture was being forced ON in tmux startup path even when config requested OFF.
2. Mouse events could still be handled when capture was OFF, causing accidental scroll/click behavior on some terminals.
3. Tool output readability improved recently, but long diffs and long transcript sessions still need better navigation affordances.
4. Popup sizing and focus behavior have improved, but modal consistency is still uneven across flows.

## Immediate Fixes (This Pass)

1. Honor `ui.mouse_capture` exactly at startup (no tmux force-on override).
2. Ignore `Event::Mouse` when `mouse_capture_on == false`.

## Phase 1: Interaction Reliability

1. Add a visible status chip for mouse mode (`mouse:on/off`) in toolbar.
2. Add `Shift+Wheel` page-scroll and `Ctrl+Wheel` jump-scroll behavior when capture is ON.
3. Add explicit copy-mode hint in status line when capture is OFF.
4. Add tests for mouse-mode guards in event loop boundaries.

## Phase 2: Transcript Navigation

1. Add sticky section markers for latest user/assistant/tool blocks.
2. Add jump shortcuts: next/prev tool block, next/prev error, jump to latest assistant.
3. Add optional minimap-style progress indicator for long transcripts.
4. Keep search mode open while navigating matches with better match context.

## Phase 3: Diff and Tool UX

1. Syntax-emphasize file headers and hunk headers more strongly.
2. Add per-tool fold persistence by call_id for current session replay.
3. Add compact/expanded view toggle for tool details (`summary` vs `full`).
4. Add one-key “copy full diff” for last edit tool block.

## Phase 4: Modal Consistency

1. Standardize modal size tiers (`sm`, `md`, `lg`) and spacing tokens.
2. Standardize footer action grammar (`Enter confirm · Esc cancel ...`).
3. Ensure all modals support paste and keyboard-only completion.
4. Add modal stack policy (top-most input ownership only).

## Validation Checklist

1. `cargo fmt`
2. `cargo test -p dcode-ai-cli`
3. `cargo build --release -p dcode-ai-cli`
4. Manual checks:
   - tmux + non-tmux startup mouse mode
   - F12 toggle behavior
   - wheel scroll + text selection
   - diff readability in tool blocks
