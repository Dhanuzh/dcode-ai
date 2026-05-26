# Improvements Roadmap

Status of the review-driven hardening pass, plus what remains to make dcode-ai
competitive with the strongest terminal coding agents (Claude Code, Aider,
Codex CLI, opencode/Crush, Gemini CLI).

## Done

- **Context-window gauge (UI).** The status bar shows `ctx ▰▰▰▱▱ NN%` — last turn's
  input tokens vs the model's real context window, color-coded (green/amber/red)
  so the user sees compaction approaching. Built on the tokenizer + `model_limits`.
- **Diff-stat summary (UI).** Tool diffs (e.g. `edit_file`) now lead with a
  `+adds −dels` line above the existing lane/gutter-styled unified diff.
- **Real tokenizer.** `runtime/token_count.rs` counts with the `o200k_base` BPE
  instead of `chars / 4`. Context budgeting and compaction triggers are now
  accurate for code/JSON/CJK. Falls back to a heuristic if the tables fail to load.
- **Anthropic prompt caching.** `anthropic_compat::anthropic_request_body` sets
  `ephemeral` cache breakpoints on the system prompt and tool schemas — the large
  static prefix repeated every turn. Cached-prefix tokens are folded back into
  reported usage so totals don't appear to drop on a cache hit.
- **Panic-safe terminal.** `tui/app.rs::install_terminal_panic_hook` restores the
  terminal before the process aborts. Under `panic = "abort"` no `Drop` runs, so
  without this a TUI panic left the terminal stuck in raw/alt-screen.
- **Supply-chain gate.** `deny.toml` + a `cargo-deny` CI job
  (advisories, licenses, bans, sources).
- **Docs.** `architecture.md`, `configuration.md`, and this roadmap.
- **Per-model pricing.** `cost.rs` prices via a model-keyed table (longest-match
  on the model name), not a hardcoded Sonnet rate. `CostTracker::for_model` is
  wired in the agent.
- **Conversation-prefix caching.** A third `ephemeral` breakpoint sits on the
  final message, so multi-turn history caches incrementally on top of the
  system + tools breakpoints (`anthropic_compat::mark_last_block_cacheable`).
- **Benchmarks.** `crates/runtime/benches/token_count.rs` (criterion) covers BPE
  token counting and per-slice context estimation.
- **MCP HTTP transport.** `tools/mcp.rs` now has an `McpTransport` trait with two
  impls: the original stdio client and a Streamable-HTTP client (JSON-RPC POST,
  JSON or SSE responses, `Mcp-Session-Id` threading). Selected by a `url` field
  on the server config. Supports custom `headers` (e.g. `Authorization`) with
  `${ENV_VAR}` expansion for auth'd hosted servers.
- **Cache-read cost tier.** `StreamChunk::CacheUsage` carries Anthropic cache
  read/creation tokens; `CostTracker` prices them at 0.1x / 1.25x of input while
  still reporting total input tokens to the UI.
- **Edit-tool precedence.** `edit_file` / `apply_patch` / `replace_match`
  descriptions now state when to use each (single / batch / coordinate-based),
  so the model stops picking the wrong one.
- **Semantic compaction.** Compaction now pins a "Preserved Artifacts" block
  (files touched, commands run, errors seen — pulled from tool results too)
  above the conversation digest, instead of letting that state be summarized away.
- **MCP auth headers.** HTTP transport sends configurable `headers` (e.g.
  `Authorization`) with `${ENV_VAR}` expansion so secrets stay out of config.
- **Retry/backoff.** `provider/retry.rs` retries transient `RateLimited` requests
  with server-hint-aware exponential backoff; wired into every HTTP provider
  (Anthropic, OpenAI, OpenRouter, and MiniMax via its OpenAI wrap).
- **Repo-map ranking (v2, PageRank).** `code_map.rs` builds a directed reference
  graph over source files and ranks them by iterative PageRank (damping 0.85),
  so a file used by central files outranks one used by leaves even at equal raw
  counts. Inbound-ref count kept as the displayed `score`. Exposed as the
  read-only `repo_map` tool, and — opt-in via `harness.include_repo_map` — as a
  ranked "Repo Map" section prepended to the system prompt so the agent starts
  oriented (Aider-style auto-context).

## In progress

1. **Split `tui/app.rs`.** 8025 → ~5500 LOC so far (15 modules extracted behind the
   existing tests): `path_parse`, `git`, `terminal`, `palette`, `layout`, `mouse`,
   `slash_entries`, `composer_input`, `oauth_status`, `branch_picker`, `markdown`,
   `answer_parse`, `paste`, `render_helpers`, and `transcript` (the ~665-line
   `transcript_lines_and_hits` renderer, lifted as a verbatim move under its
   characterization tests — block content, tool status/detail, line/hit alignment).
   Remaining: the run loop and key/event dispatch. **Decomposition in progress** —
   sub-state-machines are being lifted onto crossterm-free, unit-tested
   `TuiSessionState` methods, leaving the loop only a key→intent mapping:
   Seven overlay sub-machines are now tested `TuiSessionState` methods, with the
   loop reduced to a key→intent mapping per overlay:
   `apply_history_search_key` (+`history_search_matches`), `apply_branch_picker_key`
   (→`TuiCmd`), `apply_question_modal_key` (→outcome), `apply_command_palette_key`,
   `apply_info_modal_key`, `apply_model_picker_key` (→`ModelPickerAction`), and
   `apply_session_picker_key` (→session id). Plus `ApprovalRequest::allow_pattern`
   deduped the approval "always-allow" sites.
   Ten overlay sub-machines are now lifted (history-search, branch-picker,
   question-modal, command-palette, info-modal, model-picker, session-picker,
   provider-picker, pins-modal, connect-modal). app.rs is down to ~5200 LOC.
   Remaining run-loop pieces (api-key modal, anthropic-OAuth modal, subagent
   modal, the main composer key-match, approval verdict arms) interleave
   onboarding/async flows and shared locals — they need a wider keymap
   restructure rather than the clean per-overlay lift.

## Medium priority

2. **Windows support.** PTY, tmux, and the Unix-socket IPC are Unix-only.
   Needs cfg-gating plus a named-pipe (or loopback-TCP) IPC path — and a Windows
   environment to verify at runtime, which this workspace doesn't have.
3. **LSP-backed code intelligence.** `code_intel` is heuristic; wiring an LSP
   client would match opencode/Crush-class navigation.

## Lower priority

4. **Repo-map v2.** Current `repo_map` is a reference-count heuristic; a real
   PageRank over a symbol-definition/use graph would rank hubs more precisely,
   and feeding the top-N into initial context would close the Aider gap.
