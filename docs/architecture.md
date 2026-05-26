# Architecture

dcode-ai is a Rust workspace of five crates. Data flows: **TUI → runtime supervisor → core agent loop → provider → tools → back up the stack as streamed events.**

```
crates/
  common/       shared types: config, messages, tools, sessions, events, auth
  core/         agent loop, providers, tool implementations, skills, approval
  runtime/      process/PTY exec, IPC, session store, context manager, supervisor
  cli/          TUI (ratatui), REPL, streaming render, slash commands, entrypoint
  autoresearch/ experiment loop runner (separate feature surface)
```

## Crate responsibilities

### `common`
Provider-agnostic types shared everywhere. `config.rs` (the largest, ~1.8k LOC)
holds the full `DcodeAiConfig` tree. `message.rs` defines `Message`,
`MessageContent` (text or multimodal `Parts`), and roles. `tool.rs` defines
`ToolCall`/`ToolDefinition`/`ToolResult`. No I/O, no async — pure data.

### `core`
The brain.
- `agent.rs` — the turn loop: send messages → stream provider chunks → execute
  tool calls → append results → repeat until the model stops requesting tools.
- `provider/` — one module per backend (`anthropic`, `openai`, `openrouter`,
  `minimax`, `openai_compat`, `anthropic_compat`, `claude_cli`). `factory.rs`
  selects one from config. `anthropic_compat.rs` builds the request body and
  parses the SSE stream; `anthropic.rs` is a thin transport over it.
- `tools/` — each tool is a `ToolExecutor` (`definition()` + async `execute()`),
  registered into a `ToolRegistry` (`mod.rs`) as read-only or full sets.
- `skills.rs` / `skill_installer.rs` — discovery and install of skills from
  `AGENTS.md` and skill directories.
- `approval.rs` — permission-mode gating for tool execution.

### `runtime`
The machinery around the agent.
- `supervisor.rs` (~2.8k LOC) — owns session lifecycle, child agents, IPC wiring.
- `context_manager.rs` — token budgeting + compaction (sliding window + summary),
  now backed by a real BPE tokenizer (`token_count.rs`).
- `session_store.rs` / `last_session.rs` — persistence and resume.
- `process.rs` / `pty.rs` / `tmux.rs` — command execution surfaces (**Unix-only**).
- `ipc.rs` — Unix-socket control plane for detached sessions.

### `cli`
- `tui/app.rs` (~8k LOC) — the interactive terminal UI. **The biggest file in the
  repo and the prime refactor target** (see the roadmap).
- `stream.rs` / `render/` — turn streamed events into rendered markdown/diffs/status.
- `slash_commands.rs`, `file_mentions.rs`, `image_attach.rs` — input affordances.
- `main.rs` — argument parsing, headless mode, dispatch into TUI or one-shot.

### `autoresearch`
Standalone experiment loop (`loop_runner.rs`, `program.rs`, `metric_parser.rs`).
Independent of the chat agent.

## The agent turn loop (core/agent.rs)

1. Assemble messages (system + memory + sliding window from `context_manager`).
2. Call the active `Provider::chat`, receive a `Receiver<StreamChunk>`.
3. Stream `TextDelta` / `InternalDelta` (reasoning) / `ToolUse` / `Usage` / `Done`.
4. For each `ToolUse`: gate through `approval`, run via `ToolRegistry`, append a
   `Role::Tool` result message.
5. If any tool ran, loop back to step 2; otherwise finish the turn.

## Token accounting

Two distinct paths, do not conflate them:
- **Authoritative** — `StreamChunk::Usage { input_tokens, output_tokens }` parsed
  from the provider stream and accumulated in `cost.rs::CostTracker`. With
  Anthropic prompt caching, cached-prefix tokens are folded into `input_tokens`.
- **Estimated** — `context_manager::estimate_tokens` using the `o200k_base` BPE.
  Used only to decide *when to compact* and to show a pre-send budget. Never used
  for billing.

## Prompt caching (Anthropic)

`anthropic_compat::anthropic_request_body` marks two `ephemeral` cache
breakpoints: the system prompt block and the last tool definition. These are the
large static prefix repeated every turn, so the cache hit covers system + all
tool schemas. GA under `anthropic-version: 2023-06-01` — no beta header needed.
