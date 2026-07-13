# dcode-ai

```text
  ___
 /   \
| x x |
|  ^  |    DCODE - AI
|_____|
 |   |
```

**Rust-native coding agent. Single binary. Terminal-first.**

---

## About

**dcode-ai** is a terminal-first coding agent written entirely in Rust. Delivered as a single static binary — no Electron, no browser shell, no JavaScript wrapper. It gives you a full TUI with streaming assistant output, live reasoning tokens, visible tool execution, per-workspace session persistence, resumable conversations, and sub-agents with optional git-worktree isolation.

Built for developers who want a fast, local-first AI coding assistant that stays out of the browser and works in the terminal where the code lives.

---

## Features

| Feature                        | Description                                                                                               |
| ------------------------------ | --------------------------------------------------------------------------------------------------------- |
| **Streaming reasoning tokens** | See the model think in real time (`✦ thinking`), not just the final reply                                 |
| **Visible tool execution**     | Each tool call renders with name + argument preview + live status (`⚡ bash ls -la  running…` → `✓ bash`) |
| **Live TUI**                   | Full interactive terminal UI with command palette, themes, mouse support                                  |
| **Session persistence**        | Every conversation saved. Resume, replay, or attach to any session                                        |
| **Sub-agents & worktrees**     | Spawn child agents with parent/child lineage and isolated git worktrees                                   |
| **Runtime IPC**                | Control detached sessions programmatically — send prompts, receive events                                 |
| **Headless automation**        | One-shot prompts, NDJSON streaming, JSON output — CI/pipe friendly                                        |
| **Multi-provider**             | MiniMax, Anthropic, OpenAI, OpenRouter, OpenAI-compatible — switch inline                                 |
| **Theme picker**               | `/theme` opens an interactive dropdown with live preview                                                  |
| **File & image attach**        | `@file` mentions with workspace completion, `Ctrl+V` image paste                                          |
| **Agent profiles**             | `@build`, `@plan`, `@review`, `@fix`, `@test` — role-specific agent behavior                              |
| **Permission modes**           | Default / Plan / AcceptEdits / DontAsk / Bypass — control what the agent can do                           |
| **Skill discovery**            | Auto-loads skills from `AGENTS.md`, skill directories, and instructions files                             |

---

## Install

### Quick install (latest release)

**Linux / macOS:**

```bash
curl -sSL https://raw.githubusercontent.com/Dhanuzh/dcode-ai/main/install.sh | bash
```

**Windows (PowerShell):**

```powershell
irm https://raw.githubusercontent.com/Dhanuzh/dcode-ai/main/install.ps1 | iex
```

Installs to `%LOCALAPPDATA%\Programs\dcode-ai` and adds it to your user PATH.
Pin a version with `$env:DCODE_AI_VERSION = "vX.Y.Z"` first; override the
directory with `$env:DCODE_AI_INSTALL_DIR`. The `search_code` tool uses
ripgrep — install it with `winget install BurntSushi.ripgrep.MSVC` if you
don't have it.

### Pin a specific version

```bash
curl -sSL https://raw.githubusercontent.com/Dhanuzh/dcode-ai/main/install.sh | DCODE_AI_VERSION=vX.Y.Z bash
```

Release archives include `.sha256` checksum files. The installer verifies them
with `sha256sum` or `shasum` when available; older releases without checksum
files install with a warning.

### User-local install (no sudo)

```bash
mkdir -p ~/.local/bin
curl -sSL https://raw.githubusercontent.com/Dhanuzh/dcode-ai/main/install.sh | DCODE_AI_INSTALL_DIR="$HOME/.local/bin" bash
```

### Build from source

```bash
# requires Rust toolchain (MSRV 1.88+)
cargo build --release
./target/release/dcode-ai
```

First launch walks through onboarding: pick a provider, paste your API key, and you're ready.

---

## Getting Started

After installation, launch `dcode-ai` in any project directory:

```bash
cd my-project
dcode-ai
```

On first run you'll be guided through:

1. **Provider selection** — choose MiniMax, Anthropic, OpenAI, or OpenRouter
2. **API key setup** — paste your key or set the appropriate env var
3. **Ready to go** — start chatting with the agent

For a one-shot task without entering the TUI:

```bash
dcode-ai run --prompt "explain the architecture of this project"
```

For detached background execution:

```bash
dcode-ai spawn --prompt "refactor authentication module"
dcode-ai status   # check running sessions
dcode-ai attach <session-id>   # attach to live output
```

---

## Key Maps

| Key      | Action                                                                  |
| -------- | ----------------------------------------------------------------------- |
| `Ctrl+P` | Command palette                                                         |
| `F2`     | Cycle model                                                             |
| `Tab`    | Cycle agent profile (`@build` / `@plan` / `@review` / `@fix` / `@test`) |
| `Esc`    | Cancel current turn (while running)                                     |
| `Esc` (idle) | Backtrack — pick a past message, edit it, rewind the conversation   |
| `Ctrl+V` | Paste image from clipboard                                              |

### Slash commands (type `/` in TUI)

| Command        | Description                       |
| -------------- | --------------------------------- |
| `/model`       | Switch model                      |
| `/models`      | Fetch models from the active provider |
| `/provider`    | Switch provider                   |
| `/theme`       | Theme picker (live preview)       |
| `/permission`  | Permission mode                   |
| `/agent`       | Agent profile                     |
| `/sessions`    | Session picker                    |
| `/branch`      | Git branch picker                 |
| `/compact`     | Summarize transcript              |
| `/compact --preview` | Preview preserved compaction context |
| `/new`         | New session                       |
| `/resume <id>` | Resume session by ID (in-process) |
| `/editor`      | Open `$EDITOR` for composing      |
| `/apikey`      | Set/update API key                |
| `/connect`     | Provider connect modal            |

### Inline shortcuts

| Syntax          | Action                                 |
| --------------- | -------------------------------------- |
| `@path/to/file` | File mention with workspace completion |
| `!command`      | Inline shell execution                 |

---

## TUI

<p align="center">
  <img src="docs/Screenshot%202026-06-02%20144304.png" alt="dcode-ai TUI — streaming assistant output, visible tool execution, and live status bar" width="900">
</p>

### Features

| Feature         | How                                                               |
| --------------- | ----------------------------------------------------------------- |
| Command palette | `Ctrl+P`                                                          |
| Model picker    | `F2` cycle, or `/model`                                           |
| Theme picker    | `/theme` — live-preview dropdown                                  |
| Agent profile   | `Tab` cycle, or `@build` / `@plan` / `@review` / `@fix` / `@test` |
| Permission mode | `/permission` — interactive mode selector                         |
| Branch picker   | status-bar chip or `/branch`                                      |
| Slash commands  | `/` — autocompletes as you type                                   |
| File mentions   | `@path/to/file` — completes from workspace                        |
| Inline shell    | `!cmd`                                                            |
| Image attach    | `Ctrl+V` (paste from clipboard)                                   |
| Cancel turn     | `Esc`                                                             |
| External editor | configured via `/editor`                                          |
| Session picker  | `/sessions`                                                       |
| Doctor          | `/doctor` — run diagnostics                                       |
| Memory          | `/memory` — persistent notes across sessions                      |

Reasoning tokens stream under a `✦ thinking` chip and commit as a dim italic block before the assistant reply. Tool calls render with their argument preview (file path, shell command, query, etc.) so you see exactly what the agent ran.

---

## Providers

Configured in `~/.dcode-ai/config.toml` (global) or `.dcode-ai/config.local.toml` (workspace — workspace wins).

| Provider               | Key env              | Default model          |
| ---------------------- | -------------------- | ---------------------- |
| MiniMax (OpenCode Zen) | `OPENCODE_API_KEY`   | `MiniMax-M2.5`         |
| Anthropic              | `ANTHROPIC_API_KEY`  | `claude-sonnet-4-6`    |
| OpenAI                 | `OPENAI_API_KEY`     | configurable           |
| OpenRouter             | `OPENROUTER_API_KEY` | configurable           |
| OpenAI-compatible      | —                    | any `base_url` + model |

Switch provider inline: `/provider minimax`, `/provider openai`, `/provider anthropic`, `/provider opencodezen`.

**Local models:** `/connect ollama`, `/connect lmstudio`, or `/connect vllm` probes the local
server, picks an available model, and persists the provider config — no manual base-URL setup.

API keys entered via `/apikey` or onboarding are stored in `~/.dcode-ai/credentials.toml`
(created `0600`), not in the shareable config files. Env vars and existing inline
config keys keep working.

Switch model within provider: `/model <name>`.

`/models` and `dcode-ai models` fetch the active provider's live model catalog.
The CLI does not fall back to a built-in model list; authentication, HTTP, empty
catalog, and response-format failures are reported explicitly.

### Per-provider setup

**MiniMax (default):**

```bash
export OPENCODE_API_KEY="your-key"
```

**Anthropic:**

```bash
export ANTHROPIC_API_KEY="your-key"
```

**OpenAI:**

```bash
export OPENAI_API_KEY="your-key"
```

**OpenRouter:**

```bash
export OPENROUTER_API_KEY="your-key"
```

---

## Commands

### Core

```
dcode-ai                        # TUI
dcode-ai run --prompt "..."     # one-shot
dcode-ai spawn --prompt "..."   # detached session
dcode-ai attach <id>            # attach to running session
dcode-ai status                 # list sessions
dcode-ai logs <id>              # stream event log
dcode-ai resume <id>            # resume finished session
dcode-ai cancel <id>            # cancel running session
dcode-ai web                    # local web chat (browser UI) for a live session
```

### Web chat

```
dcode-ai web [--port 8642] [--prompt "..."] [--model ...] [--safe]
```

Starts a normal agent session plus a loopback-only HTTP server and prints a
tokenized URL (`http://127.0.0.1:<port>/?t=<secret>`). The embedded page
streams tokens, thinking, tool calls, cost, and renders approval prompts and
interactive questions — the same event stream the TUI uses, over SSE. The
server binds `127.0.0.1` only and rejects requests without the printed token.
A thin VS Code extension that embeds this page lives in `editors/vscode/`.

### Output modes

```
--json                          structured result
--stream ndjson                 streaming event log
--stream text                   raw text
```

---

## Config

Config lives in two locations (workspace overrides global):

- `~/.dcode-ai/config.toml` — global defaults
- `.dcode-ai/config.local.toml` — workspace-local (gitignored)

### Example

```toml
[provider]
default = "opencodezen"

[provider.opencodezen]
api_key_env = "OPENCODE_API_KEY"
base_url = "https://opencode.ai/zen/v1"
model = "MiniMax-M2.5"

[model]
default_model = "MiniMax-M2.5"
max_tokens = 8192
enable_thinking = true
thinking_budget = 5120

[ui]
theme = "transparent"
code_line_numbers = false
onboarding_completed = true

[permissions]
mode = "default"

[harness]
project_instructions_path = ".dcode-airc"
local_instructions_path = ".dcode-ai/instructions.md"
skill_directories = [".dcode-ai/skills", ".claude/skills"]
```

### Config resolution

Values are resolved in this order (later wins):

1. Compiled defaults
2. `~/.dcode-ai/config.toml` (global)
3. `.dcode-ai/config.local.toml` (workspace)
4. Environment variables (`DCODE_AI_API_KEY`, `DCODE_AI_MODEL`, etc.)
5. CLI flags (`--model`, `--safe`, `--verbose`)

### Memory

Persistent notes stored in `.dcode-ai/memory.json`:

```bash
dcode-ai memory                                    # list notes
dcode-ai memory add "prefer async/await patterns"  # add note
```

In TUI: `/memory` to list, `/memory <text>` to add.

---

## Permissions

dcode-ai uses a tiered permission system to control what the agent can do.

### Modes

| Mode                 | Behavior                                                              |
| -------------------- | --------------------------------------------------------------------- |
| `default`            | Read/web tools auto-allowed; edits and commands ask for approval      |
| `plan`               | Analysis/research only — no writes or shell execution                 |
| `accept-edits`       | File edits auto-accepted; commands still ask                          |
| `dont-ask`           | Read-only automatic execution; no approval prompts                    |
| `bypass-permissions` | Fully autonomous — all tools auto-allowed (trusted environments only) |

Switch mode in TUI: `/permission`, or via CLI flag: `--permission-mode bypass-permissions`.

### How it works

- **Allowed tier** — auto-executed (reads, searches, web fetches)
- **Ask tier** — prompts for approval before execution (writes, unknown commands)
- **Denied tier** — always blocked (destructive operations like `rm -rf /`, `sudo`)

Shell commands are classified structurally (quoting-aware parsing, pipeline splitting),
not by substring match: provably read-only commands (`ls`, `cat`, `grep`, `rg`,
`git status/log/diff`, …) auto-run in every mode except Plan, while known-destructive
shapes (`sudo`, `rm -rf /`, `mkfs`, `curl | sh`, fork bombs) are denied outright.

### Sandbox (Linux)

```toml
[permissions]
sandbox_bash = true
```

Confines `execute_bash` children with Landlock: read/execute everywhere, writes only
beneath the workspace and scratch dirs (`/tmp`, `/dev/null`, …). Best-effort on kernels
without Landlock support.

### Headless runs

For headless/automation use `dont-ask` or `bypass-permissions`. If an approval-blocked tool is reached in headless mode, dcode-ai exits with code 13.

---

## Tools

The agent has access to these built-in tools:

### Filesystem

| Tool             | Description                                   |
| ---------------- | --------------------------------------------- |
| `read_file`      | Read file contents                            |
| `list_directory` | List directory entries                        |
| `write_file`     | Create or overwrite a file                    |
| `edit_file`      | Replace exact string in a file                |
| `apply_patch`    | Apply one or more exact replacements          |
| `replace_match`  | Replace by exact path/line/column coordinates |
| `rename_path`    | Rename a file or directory                    |
| `move_path`      | Move a file or directory                      |
| `copy_path`      | Copy a file                                   |
| `delete_path`    | Delete a file or directory                    |

### Code search

| Tool            | Description                                       |
| --------------- | ------------------------------------------------- |
| `search_code`   | Ripgrep-based search with structured JSON results |
| `query_symbols` | LSP-first code intelligence for symbols, definitions, references, and diagnostics; falls back to fast local lookup when `rust-analyzer` is unavailable |

### Shell

| Tool           | Description                                             |
| -------------- | ------------------------------------------------------- |
| `execute_bash` | Run shell commands in workspace (PTY-backed, sandboxed) |

### Git

| Tool         | Description                    |
| ------------ | ------------------------------ |
| `git_status` | Show working tree status       |
| `git_diff`   | Show diff (staged or unstaged) |

### Web

| Tool         | Description                               |
| ------------ | ----------------------------------------- |
| `web_search` | Public web search via DuckDuckGo          |
| `fetch_url`  | Fetch and normalize text content of a URL |

### Validation

| Tool             | Description                               |
| ---------------- | ----------------------------------------- |
| `run_validation` | Run build/test/lint commands with timeout |

### Agent utilities

| Tool             | Description                                     |
| ---------------- | ----------------------------------------------- |
| `spawn_subagent` | Delegate work to a child session                |
| `ask_question`   | Ask the user a structured question with options |
| `invoke_skill`   | Load a skill's instructions by name             |

### MCP tools

If MCP servers are configured, their tools appear as `mcp__<server>__<tool>`.

---

## Sessions

Every conversation is automatically persisted to disk.

### Session storage

```
<workspace>/.dcode-ai/sessions/
├── <session-id>.json              # session state
└── <session-id>.events.jsonl      # append-only event log

<workspace>/.dcode-ai/worktrees/
└── <session-id>/                  # optional git worktree
```

### Commands

```bash
dcode-ai status                 # list all sessions
dcode-ai logs <id>              # replay event log
dcode-ai resume <id>            # continue a saved session
dcode-ai attach <id>            # follow live output
dcode-ai cancel <id>            # stop a running session
dcode-ai worktrees list         # inspect isolated agent worktrees
dcode-ai worktrees prune        # prune stale git worktree metadata
dcode-ai worktrees merge <id>   # merge dcode-ai/<id> into its base branch
```

Machine-readable session automation uses JSON/NDJSON. See
[`docs/ipc-ndjson.md`](docs/ipc-ndjson.md) for event envelope and IPC command
schemas.

### Session lifecycle

1. **Create** — start a session (TUI, `run`, or `spawn`)
2. **Active** — agent is processing a turn
3. **Idle** — waiting for user input (TUI) or session end
4. **Persisted** — saved to disk on exit
5. **Resumed** — loaded back from disk via `resume`

Each session tracks: id, model, workspace, token counts, estimated cost, timestamps, and optional orchestration metadata.

---

## Sub-agents

Spawn child agents to handle tasks in parallel:

```bash
dcode-ai run --prompt "..." --agent build --worktree
```

### How sub-agents work

```
Parent Session
├── spawn_subagent(task: "write tests for auth")
│   └── Child Session (isolated session ID)
│       ├── Inherits conversation summary
│       ├── Runs in isolated git worktree
│       ├── Uses bypass-permissions (no interactive prompts)
│       └── Returns result to parent
└── Continues with child's output
```

- **Worktree isolation**: `<repo>/.dcode-ai/worktrees/<session-id>` on branch `dcode-ai/<session-id>`
- **Timeout**: 600 seconds (10 minutes)
- **Permissions**: `bypass-permissions` (no blocking waits)
- **Lineage**: Parent/child IDs cross-referenced in metadata

---

## Skills

dcode-ai discovers skills automatically from:

- `AGENTS.md` sections in the workspace root
- `.dcode-ai/skills/` (workspace-local)
- `.claude/skills/` (workspace, compatible with Claude Code)
- `~/.dcode-ai/skills/` (user-level)

Each skill is a directory with a `SKILL.md` file containing structured instructions. Skills are loaded on-demand via the `invoke_skill` tool or `/command` invocation.

### Custom commands

Flat prompt files become slash commands: `.dcode-ai/commands/<name>.md` (or
`.claude/commands/`, workspace or user level) runs as `/name`. The file body is sent
as the prompt with `$ARGUMENTS` replaced by whatever you type after the command.
Optional YAML frontmatter sets `description`, `model`, and `permission_mode`.

---

## Automation / Orchestration

### NDJSON event streams

```bash
dcode-ai run --prompt "fix the build" --stream ndjson | while read -r event; do
    echo "$event" | jq '.event_type'
done
```

### JSON output

```bash
dcode-ai run --prompt "summarize README.md" --json
```

### Runtime IPC

Detached sessions expose a platform IPC endpoint:

```bash
dcode-ai spawn --prompt "refactor module X"
dcode-ai ipc <session-id> send '{"type":"SendMessage","content":"follow up"}'
```

Endpoint location: Unix uses `$XDG_RUNTIME_DIR/dcode-ai/<session-id>.sock` or `/tmp/dcode-ai/<session-id>.sock`; Windows uses a loopback TCP endpoint recorded under `%LOCALAPPDATA%\\dcode-ai\\<session-id>.<port>.tcp`.

### Orchestration metadata

Set these env vars to inject orchestration context:

```
DCODE_AI_ORCH_NAME=github-actions
DCODE_AI_ORCH_RUN_ID=run-123
DCODE_AI_ORCH_TASK_ID=task-456
```

### Exit codes

| Code  | Meaning                       |
| ----- | ----------------------------- |
| `0`   | Success                       |
| `1`   | Internal failure              |
| `10`  | Configuration failure         |
| `11`  | Runtime/provider/tool failure |
| `13`  | Approval-blocked headless run |
| `130` | Cancelled                     |

---

## Custom Instructions

dcode-ai loads instructions from multiple sources, merged in order:

1. **Built-in system prompt** — core behavior rules
2. **`AGENTS.md`** — project-level instructions (workspace root)
3. **`.dcode-airc`** — dcode-ai project instructions (configurable path)
4. **`.dcode-ai/instructions.md`** — personal / local (gitignored)
5. **Skills** — available skills listed for on-demand invocation
6. **Orchestration context** — injected via `DCODE_AI_ORCH_*` env vars

---

---

## Architecture

dcode-ai is a Rust workspace with five crates:

```
dcode-ai
├── dcode-ai-common    — shared types, config, events, session metadata
├── dcode-ai-core      — agent loop, LLM providers, tool protocol, harness
├── dcode-ai-runtime   — session lifecycle, IPC, persistence, worktrees, supervision
├── dcode-ai-cli       — terminal UX — TUI, REPL, streaming, onboarding
└── dcode-ai-autoresearch — automated research capabilities
```

Single binary output: `dcode-ai`. No runtime dependencies beyond a working terminal and network access for LLM calls.

Performance baseline commands are documented in [`docs/benchmarks.md`](docs/benchmarks.md).

---

## Design Principles

1. **Terminal-native** — every interaction works in a standard terminal, no mouse required
2. **Predictable** — the agent shows what it intends to do before doing it
3. **Interruptible** — Esc or Ctrl+C cleanly cancels any in-flight operation
4. **Transparent** — token costs, tool calls, and model responses are always visible
5. **Fast** — sub-100ms startup, <10ms local tool execution, <200ms session resume

---

## Build From Source

```bash
cargo build --release
./target/release/dcode-ai
```

Tests:

```bash
cargo test --workspace
```

---

## Author

Dhanush
