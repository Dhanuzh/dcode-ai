# Worktree management CLI plan

Date: 2026-06-03

## Goal

Expose the existing runtime worktree manager through stable CLI commands so
users can inspect, prune, and merge isolated agent worktrees without manually
digging through `.dcode-ai/worktrees`.

## Current state

- `runtime::worktree::WorktreeManager` already supports create, remove, list,
  changed files, ahead/behind counts, merge, and git prune.
- Session metadata already stores `worktree_path`, `branch`, and `base_branch`.
- `supervisor::prune_sessions` can remove associated worktrees when pruning
  sessions, but there is no direct `dcode-ai worktrees ...` command.

## CLI surface

- `dcode-ai worktrees list [--json]`
  - Human output: one row per dcode-ai worktree with session id, branch, base
    branch, ahead/behind, changed file count, and path.
  - JSON output: `{ "worktrees": [...] }`.
- `dcode-ai worktrees prune [--json]`
  - Runs `git worktree prune` and reports the remaining dcode-ai worktrees.
  - Does not delete `.dcode-ai/worktrees/<session-id>` directories directly.
- `dcode-ai worktrees merge <session-id> [--base <branch>] [--json]`
  - Merges `dcode-ai/<session-id>` into the base branch.
  - Defaults `--base` to the listed worktree metadata base branch.
  - Reports a stable machine-readable error on failure.

## Safety rules

- Do not add destructive directory deletion to the CLI in this slice.
- Keep merge explicit by requiring a session id.
- Prefer JSON output for automation and concise human rows for terminal use.
- Fail loudly outside git repositories.

## Validation

- Unit-test runtime worktree listing/change helpers where possible.
- Add CLI parser/command tests for JSON and human output.
- Run targeted CLI/runtime tests and `cargo fmt`.
