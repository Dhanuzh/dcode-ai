# DCODE_AI CLI DX Autoresearch

Improve the DCODE_AI CLI's developer experience through better session management, auto-resume intelligence, and visibility.

## Files
- Editable: `crates/cli/src/main.rs` — CLI entry, commands
- Editable: `crates/cli/src/repl.rs` — REPL/TUI wiring
- Editable: `crates/cli/src/tui/` — Terminal UI components
- Editable: `crates/runtime/src/session_store.rs` — Session persistence
- Fixed: `crates/common/src/session.rs` — Shared session types

## Metric
- cmd: `cargo test --package dcode-ai-cli 2>&1`
- regex: `(\d+)\s+passed`
- goal: maximize

## Constraints
- Time budget: 300 seconds per experiment
- Must pass all existing tests
- Must maintain backward compatibility with session format

## Instructions

Focus on ONE of these areas per experiment:

### 1. Session Discovery (Quick Win)
Add a `session_summary` preview when listing sessions. Extract the last user message or use existing `session_summary` field from metadata.

### 2. Better Startup Feedback
Show project context (repo name, branch) in the banner or sidebar on startup/resume.

### 3. Session Listing Improvements
Add a `--recent` flag to `dcode-ai sessions` that shows the most recent sessions with timestamps.

### 4. Dead Code Cleanup
Remove unused helper structs/functions in `crates/cli/src/`:
- `StreamStats::elapsed_secs` 
- `open_external_editor` method
- Unused approval handlers

### 5. Performance
Add session metadata caching for faster `dcode-ai sessions` listing at scale.

## Experiment Format
Each experiment should:
1. Make a targeted change to ONE file
2. Verify tests pass
3. Record the metric improvement
4. If metric drops or tests fail, the change is discarded
