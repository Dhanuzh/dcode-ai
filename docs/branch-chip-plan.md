# TUI Branch Chip + Branch Picker Popup

## Goal
Show the current git branch name on the status bar as a clickable chip. When clicked, open a centered branch picker popup to switch or create a branch.

## Changes

### 1. `crates/cli/src/tui/state.rs`
- Add `branch_picker_open: bool` to `TuiSessionState`
- Add `current_branch: String` to `TuiSessionState`
- Initialize both in `new()`
- Add `set_current_branch(&mut self, branch: &str)` helper
- Add `refresh_branch_picker_state(&mut self)` to reset popup state

### 2. `crates/cli/src/tui/app.rs`
- Add `TuiCmd::OpenBranchPicker` variant
- Add `BRANCH_CHIP_WIDTH` constant
- Add `git_current_branch()` helper — spawns a blocking thread for sync git command
- Add `git_list_branches()` — returns `Vec<String>`
- Add `git_switch_branch()` — `git checkout <branch>`
- Add `git_create_branch(name)` — `git checkout -b <name>`
- In toolbar rendering (after the "model" span):
  - Compute branch chip bounds using a pre-render pass to find the exact x-offset
  - Render a styled `⎇ main` chip with `UNDERLINED + BOLD` style (clickable)
  - Track `branch_chip_bounds: Option<Rect>` for hit-testing
- In click handler:
  - When status bar rect contains a click, check `branch_chip_bounds`
  - If clicked → send `TuiCmd::OpenBranchPicker`
- In key handler (when `branch_picker_open`):
  - `Esc` → close
  - `Enter` → switch to selected branch
  - `/<name> + Enter` → create new branch named `<name>`
  - `Up`/`Down` → navigate branch list
- Render popup when `branch_picker_open`:
  - Centered overlay using `centered_rect()`
  - Title: " git branch "
  - List of local branches; current branch marked with `*`
  - Selected row highlighted with `USER` bg
  - Footer: " Enter switch · /name new · Esc close"

### 3. `crates/cli/src/repl.rs`
- In `TuiCmd` match arm for `OpenBranchPicker`: set `branch_picker_open = true` in TUI state
- After switching branch: refresh `current_branch` from git and update TUI state

## Constraints
- Git commands run synchronously via `tokio::task::spawn_blocking` to avoid blocking async
- If not a git repo, branch chip is hidden
- If `git branch` fails, branch chip shows "—" and clicking shows error in popup
