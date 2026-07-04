# TUI Monolith Splitting Plan

## Goal
Split `crates/cli/src/tui/app.rs` (5.4k lines) into smaller, manageable modules to improve maintainability and readability.

## 1. Create Module Structure
- `crates/cli/src/tui/render/`: All ratatui drawing logic.
- `crates/cli/src/tui/handlers/`: Input (keyboard/mouse) and event handling.
- `crates/cli/src/tui/actions/`: Business logic for commands and shortcuts.

## 2. Phase 1: Extract Rendering
- Move modal rendering functions (`render_command_palette`, `render_connect_modal`, etc.) to `render/modals.rs`.
- Move transcript rendering to `render/transcript.rs`.
- Move status bar and sidebar rendering to `render/components.rs`.
- Create a central `render/mod.rs` to export these.

## 3. Phase 2: Extract Event Handling
- Move `handle_key_event` and its large match arms to `handlers/keyboard.rs`.
- Move `handle_mouse_event` to `handlers/mouse.rs`.
- Move `AgentEvent` processing to `handlers/agent.rs`.

## 4. Phase 3: Extract Action Logic
- Move slash command execution (the `/` commands) to `actions/commands.rs`.
- Move shortcut actions (Ctrl+C, Ctrl+L, etc.) to `actions/shortcuts.rs`.

## 5. Phase 4: Refactor `App` struct
- `App` in `app.rs` should become a high-level orchestrator.
- Use the new modules to delegate work.

---

# REPL Monolith Splitting Plan

## Goal
Split `crates/cli/src/repl.rs` (5k lines) into smaller modules.

## 1. Create Module Structure
- `crates/cli/src/repl/commands/`: Implementation of slash commands.
- `crates/cli/src/repl/completion.rs`: Tab-completion logic.
- `crates/cli/src/repl/history.rs`: History management.

## 2. Phase 1: Extract Commands
- Move the giant `match` for slash commands into separate functions in `repl/commands/`.

## 3. Phase 2: Extract Completion
- Move `Completer` implementation and associated logic.
