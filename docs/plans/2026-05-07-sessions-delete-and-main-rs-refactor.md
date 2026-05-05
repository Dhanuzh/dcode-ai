# Sessions Delete + main.rs Refactor Plan

## Architecture Understanding (pre-work)

### Current State

**try_main() god function** (`crates/cli/src/main.rs`):
- ~800 lines, single async fn handling all commands
- 20+ match arms in `match cli.command` with heavy duplication
- Interactive session setup (None arm) repeated 3×: `cli.resume`, `cli.no_resume`, default fallthrough
- Each arm independently: builds config, creates runtime, wires event channels, spawns stream, runs REPL
- Output struct types (`RunCommandOutput`, `SpawnCommandOutput`, etc.) are local to main.rs
- No centralized error handling or common patterns extracted

**Session store** (`crates/runtime/src/session_store.rs`):
- `save()`, `load()`, `load_snapshot()`, `list()` — no `delete()` or `prune()`
- Files: `<workspace>/.dcode-ai/sessions/<id>.json` (full state) + `<id>.events.jsonl` (event log)
- Worktree dirs: `<workspace>/.dcode-ai/worktrees/<id>/` and git branches `dcode-ai/<id>`

**Session lifecycle** (`crates/runtime/src/supervisor.rs`):
- `cleanup_stale_sessions()` exists but only marks stale Running→Error, doesn't delete
- Worktree cleanup is manual via `WorktreeManager::remove_worktree()`
- No prune command exposed through CLI

### Key Files to Change

| File | Change |
|------|--------|
| `crates/runtime/src/session_store.rs` | Add `delete()` method |
| `crates/runtime/src/worktree.rs` | May need `delete_worktree_for_session()` helper |
| `crates/runtime/src/session_store.rs` | Add `cleanup_orphaned_events()` or integrate into delete |
| `crates/cli/src/main.rs` | Add Sessions Delete/Prune commands + refactor try_main |
| `crates/common/src/event.rs` | Maybe new `SessionDeleted` event variant |

## Phase 1: Session Store Delete + Prune

### Step 1.1: Add `SessionStore::delete()`

```rust
impl SessionStore {
    pub async fn delete(&self, session_id: &str) -> Result<(), SessionStoreError> {
        let json_path = self.sessions_dir.join(format!("{session_id}.json"));
        let events_path = self.sessions_dir.join(format!("{session_id}.events.jsonl"));

        let mut deleted_any = false;
        if json_path.exists() {
            tokio::fs::remove_file(&json_path).await
                .map_err(|e| SessionStoreError::Io(e.to_string()))?;
            deleted_any = true;
        }
        if events_path.exists() {
            let _ = tokio::fs::remove_file(&events_path).await;
        }

        if !deleted_any {
            return Err(SessionStoreError::NotFound(session_id.to_string()));
        }
        Ok(())
    }
}
```

Add `NotFound` variant to `SessionStoreError`.

### Step 1.2: Add `delete_session()` and `prune_sessions()` to supervisor or new module

Two operations:

**Delete single session** (`dcode-ai sessions delete <id>`):
1. Load session to verify it exists + check status
2. If Running, send cancel first (via IPC or kill)
3. Remove worktree if present (via WorktreeManager)
4. Remove session JSON + events file
5. Emit event if supervisor has event_tx

**Prune sessions** (`dcode-ai sessions prune`):
- `--keep-last N` (default 20): keep N most recent completed sessions, delete rest
- `--older-than HOURS` (default 168 = 7 days): delete sessions older than N hours
- `--status`: only prune sessions matching status filter
- `--dry-run`: preview without deleting
- Skips Running sessions

### Step 1.3: Wire into CLI

Add to `Command::Sessions` subcommand:

```
dcode-ai sessions delete <id> [--json]
dcode-ai sessions prune [--keep-last 20] [--older-than 168] [--status completed] [--dry-run] [--json]
```

## Phase 2: Refactor try_main() → CommandRouter

### Goal

Extract a `CommandRouter` struct from the giant `try_main()` function. Each command handler becomes a method on `CommandRouter` that receives just what it needs.

### Design

```rust
/// Holds shared dependencies for command dispatch.
/// Created at the top of try_main(), then each command variant
/// calls the corresponding method.
struct CommandRouter {
    config: DcodeAiConfig,
    workspace_root: PathBuf,
    orchestration_context: Option<OrchestrationContext>,
}

impl CommandRouter {
    // ——— Session commands ———
    async fn cmd_run(&self, opts: RunOptions) -> anyhow::Result<()>;
    async fn cmd_serve(&self, opts: ServeOptions) -> anyhow::Result<()>;
    async fn cmd_spawn(&self, opts: SpawnOptions) -> anyhow::Result<()>;
    async fn cmd_sessions(&self, opts: SessionsOptions) -> anyhow::Result<()>;
    async fn cmd_resume(&self, opts: ResumeOptions) -> anyhow::Result<()>;
    async fn cmd_logs(&self, opts: LogsOptions) -> anyhow::Result<()>;
    async fn cmd_attach(&self, opts: AttachOptions) -> anyhow::Result<()>;
    async fn cmd_status(&self, opts: StatusOptions) -> anyhow::Result<()>;
    async fn cmd_cancel(&self, opts: CancelOptions) -> anyhow::Result<()>;

    // ——— Resource commands ———
    fn cmd_skills(&self, opts: SkillsOptions) -> anyhow::Result<()>;
    fn cmd_mcp(&self, opts: McpOptions) -> anyhow::Result<()>;
    async fn cmd_memory(&self, opts: MemoryOptions) -> anyhow::Result<()>;
    fn cmd_models(&self, opts: ModelsOptions) -> anyhow::Result<()>;
    fn cmd_doctor(&self, opts: DoctorOptions) -> anyhow::Result<()>;
    fn cmd_config(&self, opts: ConfigOptions) -> anyhow::Result<()>;
    fn cmd_completion(&self, shell: ClapShell);
    async fn cmd_index(&self, opts: IndexOptions) -> anyhow::Result<()>;
    async fn cmd_autoresearch(&self, opts: AutoresearchOptions) -> anyhow::Result<()>;
    async fn cmd_login(&self, provider: OAuthProvider) -> anyhow::Result<()>;
    fn cmd_logout(&self, target: LogoutTarget) -> anyhow::Result<()>;
    fn cmd_auth(&self) -> anyhow::Result<()>;

    // ——— Interactive mode (the default None path) ———
    async fn cmd_interactive(&self, opts: InteractiveOptions) -> anyhow::Result<()>;
}
```

### Extraction Strategy

1. Create `crates/cli/src/commands/` module directory
2. Move each handler into its own file, or group related ones
3. Create `crates/cli/src/commands/mod.rs` with the `CommandRouter` struct
4. `try_main()` becomes ~50 lines of dispatch:

```rust
async fn try_main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    // ... init tracing, config, orchestration_context ...
    let router = CommandRouter { config, workspace_root, orchestration_context };

    match cli.command {
        Some(Command::Run { ... }) => router.cmd_run(RunOptions { ... }).await?,
        Some(Command::Serve { ... }) => router.cmd_serve(ServeOptions { ... }).await?,
        // ... one line per variant ...
        None => router.cmd_interactive(InteractiveOptions { ... }).await?,
    }
    Ok(())
}
```

### Duplication to Eliminate

The `None` arm currently has 3 paths that all do nearly the same thing:
1. `cli.resume` → resume last session
2. `cli.no_resume` → fresh session (no auto-resume)
3. default fallthrough → try auto-resume, fallback to fresh

Each path:
- Creates/takes approval_handler
- Builds runtime
- Optionally spawns stream task
- Creates Repl
- Optionally runs with TUI

Extract method:
```rust
async fn build_interactive_runtime(
    &self,
    config: DcodeAiConfig,
    safe: bool,
    session_id: Option<String>,
    use_tui: bool,
) -> Result<(SessionRuntime, Option<JoinHandle<()>>), ...>
```

### Order of Operations

1. First, move the session delete/prune into `SessionStore` (Phase 1)
2. Then refactor main.rs (Phase 2) to make room for the new commands
3. Wire sessions delete/prune handlers into CommandRouter

### Testing Strategy

- `SessionStore::delete()`: unit test with temp dir
- `prune_sessions()`: unit test with mock sessions
- CommandRouter: integration test via `Cli::try_parse_from` + ensure dispatch doesn't panic
- Existing tests in main.rs should keep working

## Risk Assessment

| Risk | Mitigation |
|------|------------|
| Refactor breaks existing CLI behavior | Keep original functions as thin wrappers during transition; test each command via existing patterns |
| Session delete removes .last_session pointer | Update LastSessionStore if deleted session was the last |
| Worktree cleanup fails on git conflicts | Use `--force` flag, log warnings, don't block session deletion |
| CommandRouter becomes another god struct | Keep each handler method focused (<50 lines); extract helpers for shared patterns |
