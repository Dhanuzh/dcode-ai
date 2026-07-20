# Feature Roadmap

> Future features to add to dcode-ai, beyond what's already in `TODO.md` and `docs/improvements-roadmap.md`.

## ⏱ Pending — prioritized (start here)

**0. Blocker — verify the build.** A large body of web-chat + webhook work is written but not yet compiled on a Rust toolchain. Run `cargo build && cargo test -p dcode-ai-runtime`, fix any errors, before layering on more.

**1. Web chat — small items still open**
- [x] Webhook emitter: documented in `docs/configuration.md` (`[[web.webhooks]]`); `secret_header` is now the key for a real HMAC-SHA256 signature (`X-Dcode-Signature: sha256=<hex>` over the exact body, GitHub-style; RFC 4231 test vectors in `service.rs`).
- [x] Drop `?t=` from web requests — cookie-first auth everywhere (fetch + EventSource); `?t=` only used as an automatic fallback if cookies are blocked (first 403 flips it on).
- [x] Side-by-side (two-column) diff view in the file explorer — Split/Unified toggle; runs of −/+ lines are paired row-by-row with filler cells.
- [~] Mermaid / LaTeX rendering — in-page rendering stays blocked (external multi-MB libs vs. the CSP-self-contained page); mermaid blocks now get a one-click "Render in mermaid.live" link generated from the code. LaTeX remains a plain code block.
- [ ] Centralized frontend state refactor (or Preact) — prevents the state-desync bug class.
- [~] Frontend test suite — `crates/cli/tests/web_page_smoke.rs`: static invariants over the embedded page (no raw control chars, unique element ids and function definitions, brace balance, attach-marker escape, and a page↔server cross-check that every `/api/*` route the page calls exists in `web_server.rs`). Each guards a bug class this page actually hit. A real JS-runtime suite still needs Node infra.

**2. Highest-value next features (self-contained)**
- [x] Ollama / local provider — `/connect ollama` (also `lmstudio`, `vllm`) in the TUI, and the same presets now appear in the **web provider dropdown** ("Ollama (local)" etc., no key needed): selecting one points the OpenAI-compat provider at localhost and the model list fills live from the server's `/models`.
- [x] Provider health checks — `dcode-ai doctor --check` live-probes each configured provider (fetches its model catalog with its own credentials, 10s timeout each) and reports `ok (N models)` / `FAIL: <why>` / `skipped (no key)`. Antigravity reports login state instead of probing (its Vertex probe issues billed generate calls). Plain `doctor` stays instant and hints at the flag.
- [~] Expand test coverage — started: `web_server.rs` gained a unit-test module (percent-decode, cookies, provider-key round-trip, rewind text matching, path-escape blocking) and `service.rs` covers attachment parsing + HMAC vectors. 84/145 files remain untested.

_(Everything below is the full backlog, grouped by area.)_

## Web Interface (`dcode-ai web`)

- [ ] **Bidirectional WebSocket Bridge** — Unify SSE events and HTTP POST commands into a single, low-latency bidirectional `/api/ws` channel.
- [x] **Workspace File-Tree Explorer** — 📁 modal with lazy per-directory tree (`/api/tree`), a read-only file viewer with highlighting (`/api/workspace-file`, size-capped, traversal-safe), “＠ Mention” to insert `@path` into the composer, and a **Diff** toggle showing `git diff HEAD` for the file with diff coloring (`/api/git-diff`).
- [ ] **WASM-based Pure Rust Frontend** — Compile the web interface with a Rust WASM framework (e.g., Leptos, Yew, or Dioxus) and embed the assets into the binary.
- [x] **Modern Responsive UI** — `web_chat.html` has a responsive dark/light layout: collapsible sidebar with session list, collapsible thinking + tool cards, live model/context/idle-working status, and a mobile breakpoint. (Vanilla CSS/JS, no Tailwind/Alpine — stays dependency-free.)
- [~] **Remote Webhook Emitter** — POSTs runtime events (`SessionCompleted`/`ToolApprovalRequired`/`TurnCompleted`/`Error`) to configured `[[web.webhooks]]` from the service event loop. _Still open: document the config, and replace the static `secret_header` with a real HMAC signature._
- [x] **Proper Rewind Resend Flow Integration** — Dedicated `rewindResend` sets `pendingPrompt` and re-sends on SSE `onopen` (race-free), not via generic `lifecycle`. Powers regenerate + edit-and-resend.
- [x] **Web Settings Panel** — permission-mode + extended-thinking toggles via `/api/settings` + `ApplySettings` (preserves the session model). _(max-tokens + temperature: done in follow-up.)_
- [x] **Shortcuts Modal & Export Chat** — `?` opens a shortcuts modal; `exportChat()` downloads the transcript as Markdown; global keydown wired.
- [x] **Code Block Copy Button** — hover "Copy" on code blocks via `decorateCode()` on render-finalize.
- [x] **Session Forking** — `/api/sessions/fork` clones snapshot + event log under a new id and switches to it (shutdown-first for a fresh snapshot). _(Whole-session fork; fork-at-a-point still open.)_
- [x] **Message Deletion** — delete a single user turn from the transcript.
- [x] **Richer Tool Rendering** — file-path header + diff line coloring for edit tools + copy button on tool output.
- [~] **Markdown Rendering Gaps** — task-list `- [ ]` / `- [x]` rendering done. _Mermaid + LaTeX still open._
- [x] **Web UI Polish Items** — desktop notifications on unfocused-tab completion, empty-state welcome, message timestamps, and per-session scroll-position restore.
- [ ] **Centralized Frontend State Refactor** — Migrate the web UI state management to a centralized pattern (or Preact) to prevent state-desync bugs.
- [x] **Security Hardening for Web Server** — `/api/file` canonicalizes the resolved path and confirms it stays within the sessions dir. Token now pinned to an HttpOnly/SameSite=Strict cookie on page load (accepted for all requests incl. EventSource) and scrubbed from the URL via `history.replaceState`, so it no longer sits in the address bar, history, or bookmarks. _(Completed: moving the auth token out of the URL, cookie-based auth + URL cleanup.)_
- [ ] **Frontend Testing Suite** — Establish unit/integration tests for the frontend web chat interface.

## Providers & Models

- [ ] **Google Gemini provider** — native `genai` SDK integration (currently not listed in `ProviderKind`)
- [ ] **AWS Bedrock provider** — support for Claude models via Bedrock API
- [ ] **Azure OpenAI provider** — endpoint-compatible Azure deployment support
- [ ] **Ollama/local provider** — local LLM support via Ollama API for offline/air-gapped use
- [ ] **Google Vertex AI provider** — GCP-managed model hosting
- [x] **Provider health checks** — `dcode-ai doctor --check` validates each provider's credentials, endpoint reachability, and model availability (see prioritized section above)
- [ ] **Auto-fallback chain** — when primary provider fails (quota exhausted), try fallback providers in order
- [ ] **Per-session provider override** — allow `--provider` flag or `/provider` slash command to switch provider mid-session

## Tools & Agent Capabilities

- [ ] **LSP-backed code intelligence** — replace heuristic `code_intel` with real Language Server Protocol integration for goto-definition, hover docs, completions, references
- [ ] **Tree-sitter AST queries** — expose structural code search (find functions with certain annotations, etc.)
- [ ] **GitHub/GitLab PR integration** — create/update PRs, post review comments, respond to PR review threads from within the agent
- [ ] **GitHub Issues integration** — read, create, update, comment on issues
- [ ] **Database introspection tool** — connect to PostgreSQL/MySQL/SQLite and explore schemas, run queries, get EXPLAIN plans
- [ ] **Docker/podman tool** — inspect containers, read logs, exec into running containers
- [ ] **HTTP request tool** — make ad-hoc HTTP requests from the agent (curl replacement with structured output)
- [ ] **File watcher tool** — watch files/directories for changes and report diffs incrementally
- [ ] **Terminal recording/replay** — record and replay terminal sessions for debugging
- [ ] **Environment diff tool** — compare environment variables, installed packages, system state before/after changes
- [ ] **Structured logging viewer** — tail and search structured logs (JSON-Lines, etc.) with filtering

## Onboarding & UX

- [ ] **Interactive `dcode-ai init` wizard** — step-by-step setup: provider selection, API key entry (with live validation), workspace configuration
- [ ] **Dashboard/TUI session manager** — list, resume, delete sessions from within the TUI (currently only basic session picker)
- [ ] **Session renaming** — allow users to name sessions for easier identification
- [ ] **Session archiving** — archive old sessions instead of deleting, with search capability
- [ ] **Rich markdown preview** — render tables, task lists, and images inline in the transcript
- [ ] **Multi-session tabs** — switch between multiple active sessions in the TUI (tmux-style)
- [ ] **Composer multi-line editor** — proper multi-line text editing in the input area with syntax highlighting
- [ ] **Drag-and-drop file attachment** — drag files from the OS file manager into the TUI to attach them
- [ ] **Command palette improvements** — fuzzy-find commands, show keyboard shortcuts, recent commands history
- [ ] **Theme gallery** — browse and preview built-in themes from the TUI

## Context & Memory

- [~] **Persistent memory/chronicles** — the capture/recall loop now works: a `save_memory` tool lets the agent persist durable facts (preference/convention/decision/fact) to the workspace memory store, and the newest 30 notes are injected into the system prompt at session start (`## Persistent memory` section; per-note size cap). Auto-approved (writes only the app-owned memory.json). Shares the store with `/memory` and `dcode-ai memory add`. _Still open: embeddings/vector recall for large stores, and global (cross-workspace) memory._
- [ ] **Workspace indexing daemon** — background daemon that incrementally indexes workspace files for full-text search and code intelligence
- [ ] **Semantic code search** — embed-based code search ("find where we handle auth tokens") using local embeddings
- [ ] **Token budget management** — per-turn token budget with automatic context compaction when approaching limits
- [ ] **Selective context pinning** — pin specific files or search results to stay in context, survive compaction
- [ ] **Context window visualization** — show which files/topics occupy the most context tokens (pie chart or treemap in TUI)
- [ ] **Conversation branching** — fork a session at any point to explore alternative approaches without losing history
- [ ] **Automatic context pruning** — trim stale conversation turns based on recency and relevance heuristics

## IPC & Runtime

- [x] **Length-prefix message framing** — IPC sockets use 4-byte big-endian length prefixes (16 MiB cap). Readers auto-detect per connection (framed streams start 0x00; NDJSON starts `{`) so old peers still parse; `DCODE_AI_IPC_LEGACY=1` keeps NDJSON writes for external consumers. Event log files stay NDJSON. Documented in `docs/ipc-ndjson.md`; round-trip/legacy/truncation tests in `ipc.rs`.
- [ ] **mTLS or socket-auth for IPC** — authenticate CLI ↔ runtime connections to prevent unauthorized local control
- [ ] **Runtime process supervision** — auto-restart runtime if it crashes, with session recovery
- [ ] **Multi-user runtime** — run a single runtime daemon serving multiple users/sessions via authenticated IPC
- [ ] **WebSocket bridge** — expose runtime events over WebSocket for web-based UIs or remote monitoring
- [ ] **Headless mode improvements** — strict JSON output mode, machine-readable exit codes, structured error reporting for CI/CD pipelines
- [ ] **Session export/import** — export full session (messages + events) as a portable bundle; import to resume elsewhere

## Testing & Reliability

- [ ] **Property-based testing** — proptest for approval policy, wildcard matching, IPC serialization roundtrips, token counting edge cases
- [ ] **Integration test harness** — programmatic harness for end-to-end agent tests (mock provider, assert tool calls, verify file changes)
- [ ] **Fault injection testing** — simulate network failures, provider timeouts, disk full, corrupted session files
- [ ] **Smoke test suite** — quick CLI smoke tests run on every build: init, message, shutdown
- [ ] **Benchmark suite** — measure token-count throughput, IPC latency, TUI render speed, provider streaming latency
- [ ] **Snapshot testing for TUI** — ratatui snapshot tests for all modal renderers, diff hunks, status bar states

## Cross-Platform

- [ ] **Windows named-pipe IPC** — replace Unix sockets with named pipes on Windows
- [ ] **Windows PTY support** — implement `process::spawn` and `interactive_exec` via Windows Pseudo Console (ConPTY)
- [ ] **Windows terminal integration** — detect Windows Terminal, ConEmu, WezTerm; adapt TUI rendering accordingly
- [ ] **macOS native improvements** — detect and integrate with macOS file manager, notifications, open-in-editor
- [ ] **ARM64/aarch64 builds** — ensure builds and testing on ARM64 Linux (Raspberry Pi, AWS Graviton) and Apple Silicon (already runs via Rosetta, but native builds needed)

## Extensibility

- [ ] **Plugin system** — load external tool providers via dynamic library (`dlopen`/`LoadLibrary`) or subprocess protocol
- [ ] **Custom tool SDK** — allow users to write their own tools in Rust (or via WASM) and register them in config
- [ ] **MCP client improvements** — support for MCP tool annotations (destructive hints, idempotency), streaming results, pagination
- [ ] **Webhook system** — emit webhooks on session events (tool calls, completions, errors) for integration with CI/CD pipelines, Slack, etc.
- [ ] **Language-agnostic skill format** — allow skills to be written in any language via a well-defined manifest + script interface (currently Rust-only)
- [ ] **Pre/post hooks** — run shell commands or scripts before/after specific tool calls (e.g., auto-format code after `edit_file`)

## Security & Compliance

- [ ] **Credential encryption at rest** — encrypt stored API keys and tokens (currently plaintext in config)
- [ ] **Session audit log** — append-only signed audit trail of all tool calls, approvals, and file modifications
- [ ] **Command allow/deny lists** — more granular control over which shell commands can run without approval
- [ ] **File system sandbox** — restrict agent file access to workspace directory with configurable exceptions
- [x] **Secret redaction** — tool outputs are scrubbed at the agent's single choke point (before model history, UI events, hooks, and session logs): exact values of secret-named env vars + the credentials store are replaced with `[redacted:<NAME>]`, and token shapes (`sk-`, `sk-ant-`, `ghp_`/`github_pat_`, `xox?-`, `AKIA`, `AIza`, `glpat-`, `npm_`) plus PEM private-key blocks are redacted heuristically (`common/src/redact.rs`, unit-tested). Best-effort defense in depth, not a hard guarantee.
- [ ] **Approval policies by pattern** — allow/disallow specific tool+input patterns with saved policies (partially exists as `allow_pattern`)

## Performance & Engineering

- [ ] **Incremental compilation optimization** — organize crates to minimize rebuilds on common changes
- [ ] **Lazy loading TUI** — defer import of heavy dependencies (syntect, ratatui widgets) until TUI mode is entered
- [ ] **Zero-copy IPC** — use shared memory or Unix domain socket ancillary data for large payloads
- [ ] **Async tool execution** — run tools concurrently when possible (independent file reads, parallel searches)
- [ ] **Streaming tokenizer** — tokenize incrementally during streaming to avoid full-content re-tokenization
- [ ] **Snapshot-based session persistence** — use copy-on-write or journal-based persistence instead of full serialization on every event

## Documentation & Community

- [ ] **dcode-ai book** — comprehensive user guide and developer documentation (mdBook or similar)
- [ ] **Video tutorials** — screencast walkthroughs of common workflows
- [ ] **GitHub Action** — official `dcode-ai-action` for running agent tasks in CI/CD
- [ ] **VS Code extension** — lightweight extension that connects to the runtime IPC for in-editor agent interaction
- [ ] **Helix editor integration** — plugin or instructions for using dcode-ai from Helix
- [ ] **Neovim plugin** — `dcode-ai.nvim` for in-editor agent workflows
- [ ] **Awesome-dcode-ai** — curated list of community skills, configs, workflows
