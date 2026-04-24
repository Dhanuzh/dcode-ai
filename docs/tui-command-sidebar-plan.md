# TUI Command Palette + Sidebar Plan

## Goals

- Add `Ctrl+P` to open a centered command palette without overwriting the current draft message.
- Keep the existing `/...` inline slash suggestion panel for fast command completion from the composer.
- Add a right sidebar with session/context information and a placeholder area for future task/todo data.

## Implementation

- Extend `TuiSessionState` with command palette UI state:
  - `command_palette_open`
  - `command_palette_query`
- Reuse the existing slash command source (`SLASH_COMMANDS`) for both inline slash completion and the new command palette.
- Update the TUI event loop so that when the command palette is open it captures:
  - `Ctrl+P` and `Esc` to close
  - `Up` / `Down` to move selection
  - `Enter` to copy the selected command into the main composer
  - text input / backspace to filter the command list
- Split the main terminal area horizontally when there is enough width:
  - left: transcript, status, inline slash panel, composer
  - right: sidebar with session info, usage, and a task/todo placeholder
- Render the command palette as a centered popup overlay using Ratatui widgets.

## Constraints

- Avoid changing existing command execution semantics outside the new `Ctrl+P` flow.
- Hide the sidebar automatically on narrow terminals instead of making the transcript unusable.
- Keep the sidebar informative now, but structured so task/todo items can be added later.

## Validation

- `cargo fmt`
- targeted build/test for `dcode-ai-cli`
- lints on edited files

## Status bar (toolbar)

- With the right sidebar visible: show **idle/busy**, **model**, **agent**, **permission** (and a high-visibility **`BYPASS`** chip when `BypassPermissions` is active), and **elapsed time**. Omit session id, token counts, and cost (those stay in the sidebar).
- On narrow terminals where the sidebar is hidden, session / in-out tokens / cost are shown again on the status bar so nothing critical disappears.
