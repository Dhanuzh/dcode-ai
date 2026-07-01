# TODO: Codebase Improvements

## 1. MCP Connector Support (SharePoint, Jira, etc.)

Connect dcode-ai to external services via the **Model Context Protocol (MCP)**.

### Current capabilities
- [x] MCP stdio transport (spawn process per call)
- [x] MCP HTTP/SSE transport (per-request client)
- [x] Tool discovery (`tools/list`)
- [x] Tool execution (`tools/call`)
- [x] Config: `[mcp.servers]` with name, command, args, env, cwd, url, headers, enabled

### P0 — Connection reuse + timeouts (unblocks real use)
- [ ] **Keep-alive for stdio**: keep MCP process alive across tool calls, `shutdown` only on session end
- [ ] **Session-id threading for HTTP**: reuse `McpSessionId` across requests
- [ ] `startup_timeout_sec` in McpServerConfig (default 15s, prevents hangs on `initialize`)
- [ ] `tool_timeout_sec` in McpServerConfig (per-call timeout, prevents silent hangs)

### P0 — Per-tool approval policy (safety)
- [ ] `[mcp_servers.<name>.tools.<tool>]` config section with `enabled` + `approval` fields
- [ ] `approval = "auto" | "always" | "deny"` (same model as Codex `AppToolApproval`)
- [ ] Respect `[permissions]` deny/allow/ask lists for MCP tools by name match

### P1 — Startup events + visibility
- [ ] `AgentEvent::McpStartupUpdate { server, status }` — emitted during `initialize`
- [ ] `AgentEvent::McpStartupComplete { ready, failed, cancelled }` — summary after all servers
- [ ] Show per-connector status in TUI status bar (e.g. `◆1: jira` instead of just `◆1`)
- [ ] `dcode-ai mcp status` — show live status of each configured server

### P1 — Tool list disk cache
- [ ] Cache `tools/list` response to `~/.dcode-ai/workspaces/<id>/mcp-tools-<name>.json`
- [ ] Revalidate on TTL expiry or config `force_refetch = true`

### P2 — Resource support
- [ ] Implement `resources/list` + `resources/read` in `McpTransport`
- [ ] Expose MCP resources alongside tools
- [ ] Needed for connectors that expose data as resources (SharePoint files)

### Files
| File | Change |
|------|--------|
| `crates/common/src/config.rs` | `keep_alive`, `tool_timeout_sec`, `startup_timeout_sec`, per-tool config fields |
| `crates/core/src/tools/mcp.rs` | Connection reuse, timeout, resources, startup events |
| `crates/runtime/src/supervisor.rs` | Wire startup events, emit per-connector status |
| `crates/common/src/event.rs` | Add `McpStartupUpdate` / `McpStartupComplete` variants |
| `crates/cli/src/main.rs` | `mcp status` command, richer `list_mcp_servers` output |

---

## 2. Split monolith files (maintainability)

The 6 largest files make up **~40% of all code**. Each is doing too much.

### `crates/cli/src/tui/app.rs` (5,552 lines)
- [ ] Extract event loop → `app/event_loop.rs`
- [ ] Extract rendering → `app/rendering.rs`
- [ ] Extract input handling → `app/input.rs`
- [ ] Keep core wiring in `app/mod.rs`

### `crates/cli/src/repl.rs` (5,443 lines)
- [ ] Extract completion → `repl/completion.rs`
- [ ] Extract history → `repl/history.rs`
- [ ] Extract agent profiles → `repl/profiles.rs`
- [ ] Extract input handling → `repl/input.rs`
- [ ] Keep loop + command dispatch in `repl/mod.rs`

### `crates/runtime/src/supervisor.rs` (3,078 lines)
- [ ] Extract session lifecycle → `session.rs`
- [ ] Extract prune logic → `prune.rs`
- [ ] Extract sub-agent spawning → `subagent.rs`
- [ ] Keep supervisor core in `mod.rs`

### `crates/cli/src/tui/state.rs` (3,067 lines)
- [ ] Split into focused modules: `state/transcript.rs`, `state/connection.rs`, `state/settings.rs`

### `crates/common/src/config.rs` (1,866 lines)
- [ ] Extract provider configs → `config/provider.rs`
- [ ] Extract MCP config → `config/mcp.rs`
- [ ] Extract UI config → `config/ui.rs`
- [ ] Extract workspace helpers → `config/workspace.rs`
- [ ] Keep top-level merge + env logic in `config/mod.rs`

### `crates/cli/src/main.rs` (2,562 lines)
- [ ] Extract subcommand handlers into per-command modules (`cmd/session.rs`, `cmd/config.rs`, `cmd/mcp.rs`, etc.)

---

## 3. Increase test coverage

**86/145 files have zero test functions. Only 1 integration test file.**

### Unit tests (high priority)
- [ ] `mcp.rs` — transport round-trips, timeout behavior, invalid server responses
- [ ] `approval.rs` — policy resolution, handler chaining, timeout handling
- [ ] `session_store.rs` — save/load round-trips, corruption recovery, concurrent access
- [ ] `ipc.rs` — command serialization, approval forwarding, disconnection handling
- [ ] `worktree.rs` — creation, cleanup, parallel worktree isolation
- [ ] `provider/retry.rs` — backoff, max retries, error classification
- [ ] `tools/ask_question.rs` — question lifecycle, timeout, selection mapping

### Integration tests (high priority)
- [ ] MCP tool execution end-to-end (start fake server, discover tools, call tool)
- [ ] Session resume flow (create → save → load → continue)
- [ ] IPC approval flow (send approve command over Unix socket)
- [ ] Sub-agent lifecycle (spawn, work, collect result)
- [ ] Config merge priority (defaults < global < project < local < env)

### Property-based tests (medium priority)
- [ ] Message serialization round-trips (any content → JSON → Message)
- [ ] Event envelope idempotency (envelope → JSON → envelope)
- [ ] Config partial merge associativity (merge A + B + C == merge (A+B) + C)

---

## 4. Provider refactoring (reduce duplication)

`openai.rs`, `anthropic.rs`, `minimax.rs`, `openrouter.rs`, `openai_compat.rs` each duplicate:
- HTTP client setup
- Retry + backoff logic
- Streaming response parsing
- Error classification
- Key resolution

- [ ] Extract shared HTTP client → `provider/transport.rs`
- [ ] Extract shared retry logic → `provider/retry.rs` (already exists, use it)
- [ ] Extract common SSE/event-stream parser → `provider/sse.rs`
- [ ] Unify `StreamChunk` handling across all providers
- [ ] Add generic rate-limit detection in shared layer

---

## 5. Error type audit

Many functions return `Result<_, String>`, losing structured error info across crate boundaries.

- [ ] Audit all `Result<_, String>` in public APIs
- [ ] Replace with typed errors using `thiserror`
- [ ] Ensure `ProviderError`, `ConfigError`, `SessionError` unify at crate boundaries
- [ ] Add `#[source]` chains so `anyhow::Error` captures full context
- [ ] Remove `unwrap()` calls from non-test code (audit with clippy)

---

## 6. `autoresearch` crate — dead or isolated?

`crates/autoresearch/` (3,033 lines) — not imported by any other crate. Autonomous research loop.

- [ ] Decide: wire into CLI as `dcode-ai research` or document as standalone tool
- [ ] If keeping: add tests (currently zero), add `#[deny(dead_code)]`
- [ ] If removing: move to `archive/` or extract to separate repo

---

## 7. IPC protocol hardening

Current: newline-delimited JSON over Unix sockets. Works but fragile.

- [ ] Add length-prefix message framing (4-byte big-endian length + JSON body) to prevent corruption on buffer boundary
- [ ] Add heartbeat pings every 30s to detect dead peers
- [ ] Add reconnection on EOF for persistent subscriptions
- [ ] Document protocol in `docs/ipc.md`

---

## 8. Build + CI improvements

- [ ] Add `cargo udeps` to CI (detect unused dependencies)
- [ ] Add `cargo-deny` or `cargo-audit` for dependency vulnerability scanning
- [ ] Cache `target/` in CI across jobs
- [ ] Run clippy on all targets with `-D warnings`
- [ ] Add benchmark infrastructure for provider latency, tool execution

---

## Files reference

| File | Lines | Issue |
|------|-------|-------|
| `crates/cli/src/tui/app.rs` | 5,552 | Monolith — split into `app/` module |
| `crates/cli/src/repl.rs` | 5,443 | Monolith — split into `repl/` module |
| `crates/runtime/src/supervisor.rs` | 3,078 | Too many responsibilities |
| `crates/cli/src/tui/state.rs` | 3,067 | Monolith — split into `state/` |
| `crates/cli/src/main.rs` | 2,562 | Inline subcommands |
| `crates/common/src/config.rs` | 1,866 | Mixed concerns |
| `crates/core/src/tools/mcp.rs` | 662 | Already tracking in section 1 |
| `crates/autoresearch/` | 3,033 | Orphan crate |
