# DCODE_AI CLI DX Research Program

## Objective
Improve the DCODE_AI CLI's developer experience through better session management, auto-resume intelligence, workspace awareness, and visibility into long-running work (e.g. sub-agents).

## Implemented (baseline for further research)
- **Transcript replay on TUI open / resume**: `.dcode-ai/sessions/<id>.events.jsonl` is replayed into the full-screen transcript before live events attach, so past user/assistant/tool lines reappear. The model already resumed from `<id>.json` (`messages`); the UI now matches that intent.
- **Sub-agent visibility**: Child sessions forward condensed activity (`ChildSessionActivity`) to the parent event stream; the TUI sidebar shows per-child phase/detail and the transcript logs `↳ …` lines while `spawn_subagent` is still running.
- **Sidebar context**: Workspace path, token/cost block, sub-agents panel, and quick path hints (`.dcode-ai/sessions`, `docs/research/`).
- **Config ergonomics**: `max_turns_per_run` (and alias `max_turn_per_run`) in `[session]` for turn budget.
- **Autoresearch hook**: `dcode-ai autoresearch once <program.md>` runs this file’s metric shell command once and prints the parsed metric (see below).

## Editable Files
- `crates/cli/src/main.rs` — CLI entry, `autoresearch` subcommand
- `crates/cli/src/repl.rs` — REPL / TUI wiring, replay before bridge
- `crates/cli/src/tui/` — `app.rs`, `state.rs`, `bridge.rs`, `replay.rs`
- `crates/cli/src/stream.rs` — human stream rendering for child events
- `crates/runtime/src/supervisor.rs` — resume, child session fanout → parent activity
- `crates/runtime/src/session_store.rs`
- `crates/common/src/event.rs` — `AgentEvent` variants (e.g. `ChildSessionActivity`)
- `crates/autoresearch/` — program parse, experiment runner (used by `dcode-ai autoresearch`)

## Fixed Files (contract / shared types)
- `crates/common/src/session.rs`
- `crates/common/src/config.rs`

## Metric
- cmd: `cargo test --package dcode-ai-cli 2>&1`
- regex: (\d+)\s+passed
- goal: maximize

## Constraints
- Time budget: 600 seconds
- Must not break existing functionality
- Must pass all tests
- Must maintain backward compatibility with session format (`*.json` + `*.events.jsonl`)

## Research Questions

### 1. Smart Session Discovery
How can we improve session auto-selection beyond "most recent" (`.dcode-ai/.last_session` + fallback by `updated_at`)?
- [ ] Add session preview with conversation snippets (e.g. from last `MessageReceived` in event log or `session_summary`)
- [ ] Add context-based session suggestion (project-aware cwd / git root)
- [ ] Add session tagging/labeling

### 2. Session Organization
How to help users find and manage sessions?
- [ ] Extend `dcode-ai sessions` search beyond current flags (content/date/status)
- [ ] Add session grouping by project / workspace root
- [ ] Add auto-cleanup of stale sessions (policy + `dcode-ai` command)

### 3. Better UX on Startup
What feedback should users get?
- [ ] Show explicit **session summary** on resume (meta `session_summary` / first lines of replay)
- [x] **Conversation history in TUI** — replay from `*.events.jsonl` (see Implemented)
- [ ] Show project context detection (e.g. inferred repo name, branch) in sidebar or banner

### 4. Performance Improvements
How to make session loading faster at scale?
- [ ] Lazy load or cap replayed events for huge logs (keep full history in `messages` for the model)
- [ ] Incremental session checkpoints
- [ ] Cache session metadata for `dcode-ai sessions` listing

### 5. Sub-agents & Orchestration (new)
- [ ] Optional aggregation: collapse per-child activity into one expandable row
- [ ] Surface child **exit status / output** snippet in sidebar when `ChildSessionCompleted` fires
- [ ] NDJSON / headless consumers: document `ChildSessionActivity` in IPC stream

## Running autoresearch (metric probe)

From the repo root:

```bash
dcode-ai autoresearch once docs/research/cli-dx-research.md
```

Runs the metric `cmd` once under `sh -c`, captures the first `(\d+) passed` group from `cargo test` output, and prints it. Use `--workspace <dir>` if not running from the repo root.

## Persistence reference (for researchers)
| Artifact | Role |
|----------|------|
| `.dcode-ai/sessions/<id>.json` | `SessionState`: `messages`, costs, `SessionMeta` — model context on resume |
| `.dcode-ai/sessions/<id>.events.jsonl` | UI event log (envelope or legacy bare `AgentEvent` lines) — TUI replay |
| `.dcode-ai/.last_session` | Single session id for auto-resume |

If the event log is missing but `messages` exist, the model still has chat history; the TUI cannot fully reconstruct tool rows without replay data.
