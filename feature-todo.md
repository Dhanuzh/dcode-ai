# Feature Roadmap

> Future features to add to dcode-ai, beyond what's already in `TODO.md` and `docs/improvements-roadmap.md`.

## Web Interface (`dcode-ai web`)

- [ ] **Bidirectional WebSocket Bridge** — Unify SSE events and HTTP POST commands into a single, low-latency bidirectional `/api/ws` channel.
- [x] **Workspace File-Tree Explorer** — 📁 modal with lazy per-directory tree (`/api/tree`), a read-only file viewer with highlighting (`/api/workspace-file`, size-capped, traversal-safe), “＠ Mention” to insert `@path` into the composer, and a **Diff** toggle showing `git diff HEAD` for the file with diff coloring (`/api/git-diff`).
- [ ] **WASM-based Pure Rust Frontend** — Compile the web interface with a Rust WASM framework (e.g., Leptos, Yew, or Dioxus) and embed the assets into the binary.
- [ ] **Modern Responsive UI** — Update `web_chat.html` with a modern, responsive layout (Tailwind CSS, Alpine.js) featuring sidebars, collapsible tool execution details, and session history lists.
- [ ] **Remote Webhook Emitter** — Trigger configurable HTTP POST webhooks for runtime events (such as `SessionCompleted` or `ToolApprovalRequired`) to notify external services.
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
- [ ] **Provider health checks** — `dcode-ai doctor` subcommand that validates each provider's credentials, endpoint reachability, and model availability
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

- [ ] **Persistent memory/chronicles** — cross-session memory stored in a local vector DB (SQLite + embeddings), so the agent remembers user preferences, project conventions, and past decisions across sessions
- [ ] **Workspace indexing daemon** — background daemon that incrementally indexes workspace files for full-text search and code intelligence
- [ ] **Semantic code search** — embed-based code search ("find where we handle auth tokens") using local embeddings
- [ ] **Token budget management** — per-turn token budget with automatic context compaction when approaching limits
- [ ] **Selective context pinning** — pin specific files or search results to stay in context, survive compaction
- [ ] **Context window visualization** — show which files/topics occupy the most context tokens (pie chart or treemap in TUI)
- [ ] **Conversation branching** — fork a session at any point to explore alternative approaches without losing history
- [ ] **Automatic context pruning** — trim stale conversation turns based on recency and relevance heuristics

## IPC & Runtime

- [ ] **Length-prefix message framing** — replace delimiter-based IPC with length-prefixed frames to prevent desync on binary data or embedded newlines
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
- [ ] **Secret redaction** — automatically detect and redact API keys, tokens, passwords from tool outputs and conversation logs
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
