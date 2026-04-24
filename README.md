# dcode-ai

<p align="center">
</p>

<p align="center">
  <strong>Rust-native coding agent. Single binary. Terminal-first.</strong>
</p>

<p align="center">
  <a href="#install">Install</a> ·
  <a href="#quick-start">Quick Start</a> ·
  <a href="#tui-features">TUI</a> ·
  <a href="#providers">Providers</a> ·
  <a href="#commands">Commands</a> ·
  <a href="#config">Config</a>
</p>

---

`dcode-ai` is a terminal-first coding agent written in Rust. One static binary. No Electron, no browser shell, no wrapper. Full TUI with streaming assistant output, live reasoning tokens, visible tool calls, per-workspace sessions, resumable conversations, and sub-agents with optional git-worktree isolation.

## What's New

- **Live reasoning tokens** — see the model's thinking stream as it happens (`✦ thinking`), not just the final reply.
- **Visible tool execution** — each tool call renders with name + argument preview + status (`⚡ bash  ls -la  running…` → `✓ bash`).
- **Theme picker** — `/theme` opens an interactive dropdown. Arrow keys preview live, Enter persists to workspace config.
- **Mouse toggle (F12)** — capture ON for wheel scroll, OFF for native click-drag text selection. State chip in transcript.
- **In-process session resume** — `/resume <id>` swaps sessions without restarting the process.
- **Big-pickle / OpenCode Zen provider** — first-class support alongside Anthropic, OpenAI, OpenRouter.

## Quick Start

```bash
# interactive TUI
dcode-ai

# one-shot prompt
dcode-ai run --prompt "explain this repo"

# detached background session
dcode-ai spawn --prompt "refactor module X"
dcode-ai status
dcode-ai attach <session-id>

# automation
dcode-ai run --prompt "..." --json
dcode-ai run --prompt "..." --stream ndjson
```

First launch walks onboarding: pick provider, paste key, done.

## TUI Features

| Feature              | How                                                               |
| -------------------- | ----------------------------------------------------------------- |
| Command palette      | `Ctrl+P`                                                          |
| Model picker         | `F2` cycle, or `/model`                                           |
| Theme picker         | `/theme` — live-preview dropdown                                  |
| Agent profile        | `Tab` cycle, or `@build` / `@plan` / `@review` / `@fix` / `@test` |
| Permission mode      | `/permission` — Default / Plan / AcceptEdits / DontAsk / Bypass   |
| Toggle mouse capture | `F12` — OFF lets you click-drag to select text                    |
| Branch picker        | status-bar chip or `/branch`                                      |
| Slash commands       | `/` — autocompletes as you type                                   |
| File mentions        | `@path/to/file` — completes from workspace                        |
| Inline shell         | `!cmd`                                                            |
| Image attach         | `Ctrl+V` (paste from clipboard)                                   |
| Cancel turn          | `Esc`                                                             |
| External editor      | configured via `/editor`                                          |
| Session picker       | `/sessions`                                                       |

Reasoning tokens stream under a `✦ thinking` chip and commit as a dim italic block before the assistant reply. Tool calls render with their argument preview (file path, shell command, query, etc.) so you see exactly what the agent ran.

## Providers

Configured in `~/.dcode-ai/config.toml` (global) or `.dcode-ai/config.local.toml` (workspace — workspace wins).

| Provider          | Key env              | Default model        |
| ----------------- | -------------------- | -------------------- |
| OpenCode Zen      | `OPENCODE_API_KEY`   | `big-pickle`         |
| Anthropic         | `ANTHROPIC_API_KEY`  | `claude-sonnet-4-6`  |
| OpenAI            | `OPENAI_API_KEY`     | configurable         |
| OpenRouter        | `OPENROUTER_API_KEY` | configurable         |
| OpenAI-compatible | —                    | any base_url + model |

Switch provider inline: `/provider openai`, `/provider anthropic`, `/provider opencodezen`. Model within provider: `/model <name>`.

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
```

### Slash (inside TUI / REPL)

```
/model          switch model
/provider       switch provider
/theme          theme picker
/permission     permission mode
/agent          agent profile
/sessions       session picker
/branch         git branch picker
/compact        summarize transcript
/new            new session
/resume <id>    resume by id (in-process)
/editor         open $EDITOR for composing
/apikey         set/update API key
/connect        provider connect modal
```

### Output modes

```
--json                          structured result
--stream ndjson                 streaming event log
--stream text                   raw text
```

## Config

Workspace config at `.dcode-ai/config.local.toml` — workspace overrides global.

```toml
[provider]
default = "opencodezen"

[provider.opencodezen]
api_key_env = "OPENCODE_API_KEY"
base_url = "https://opencode.ai/zen/v1"
model = "big-pickle"

[model]
default_model = "big-pickle"
max_tokens = 8192
enable_thinking = true
thinking_budget = 5120

[ui]
theme = "transparent"           # default, tokyonight, catppuccin, gruvbox, dracula, nord, light, transparent
code_line_numbers = false
onboarding_completed = true

[permissions]
mode = "default"                # default, plan, acceptEdits, dontAsk, bypassPermissions

[harness]
project_instructions_path = ".dcode-airc"
local_instructions_path = ".dcode-ai/instructions.md"
skill_directories = [".dcode-ai/skills", ".claude/skills"]
```

Session state, event logs, and checkpoints persist under `.dcode-ai/sessions/`.

## Automation

```bash
# JSON result
dcode-ai run --prompt "summarize README.md" --json

# NDJSON event stream (pipe-friendly)
dcode-ai run --prompt "..." --stream ndjson | jq .

# Unix-socket IPC (detached session control)
dcode-ai spawn --prompt "..."
dcode-ai ipc <session-id> send '{"kind":"user","text":"follow up"}'
```

Worker-process metadata via `DCODE_AI_ORCH_*` env vars for orchestrators.

## Sub-agents

```bash
# spawn child with lineage
dcode-ai run --prompt "..." --agent build --worktree
```

Child sessions track parent/child lineage and can run in isolated git worktrees — safe for parallel exploration.

## Skills

Discovers skills from:

- `AGENTS.md` sections in workspace
- `.dcode-ai/skills/` (workspace)
- `.claude/skills/` (workspace, compatible with Claude)
- `~/.dcode-ai/skills/` (user-level)

## Build From Source

```bash
cargo build --release
./target/release/dcode-ai
```

Tests:

```bash
cargo test --workspace
```

## License

MIT. See [LICENSE](LICENSE).
