<div align="center">

# ✻ dcode-ai

**A Rust-native coding agent for your terminal.**
One binary. Your Claude or GitHub Copilot sign-in. No API keys, no Electron, no Node.

[![Release](https://img.shields.io/github/v/release/Dhanuzh/dcode-ai?style=flat-square&color=c96442)](https://github.com/Dhanuzh/dcode-ai/releases)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue?style=flat-square)](#license)
[![Rust](https://img.shields.io/badge/rust-1.88%2B-orange?style=flat-square&logo=rust)](https://www.rust-lang.org)
![Platforms](https://img.shields.io/badge/platform-linux%20%7C%20macOS%20%7C%20windows-lightgrey?style=flat-square)

[Install](#install) · [Quick start](#quick-start) · [Features](#features) · [Docs](#documentation) · [Changelog](CHANGELOG.md)

</div>

```text
╭──────────────────────────────────────────────╮
│ ✻ Welcome to dcode-ai!                       │
│                                              │
│   /help for help, /model to switch model     │
│   cwd: ~/code/my-app                         │
│   model: claude-sonnet-5-5 · v1.0.0          │
╰──────────────────────────────────────────────╯

> fix the off-by-one in token expiry [Image #1]

● Read(src/auth.rs)
  ⎿  Read 84 lines

● Update(src/auth.rs)
  ⎿  Updated src/auth.rs with 1 addition and 1 removal
       41 -     if now > expiry {
       41 +     if now >= expiry {

● Tokens now expire at the exact expiry instant. Tests pass.
──────────────────────────────────────────────────────────────
> Try "explain this codebase"
──────────────────────────────────────────────────────────────
  ⏵⏵ accept edits on (shift+tab to cycle)    claude-sonnet-5-5
```

## Why dcode-ai

- **Uses what you already pay for** — your Claude plan (through your own signed-in `claude`) or your GitHub Copilot subscription.
- **Speaks the ecosystem** — Claude Code plugins, marketplaces, `.mcp.json`, skills, commands, agents and hooks work unchanged.
- **Built to spawn work** — detached sessions, sub-agents in isolated git worktrees, NDJSON streams, IPC, a CI mode and a GitHub Action.
- **Small and fast** — one static Rust binary, instant start, everything local except the model call.

## Install

**Linux / macOS**

```bash
curl -sSL https://raw.githubusercontent.com/Dhanuzh/dcode-ai/main/install.sh | bash
```

**Homebrew (macOS / Linux)**

```bash
brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
brew install dcode-ai          # update: brew upgrade dcode-ai
```

**Windows (PowerShell)**

```powershell
irm https://raw.githubusercontent.com/Dhanuzh/dcode-ai/main/install.ps1 | iex
```

**Update** an installed copy in place (checksum-verified), or type `/update`
inside a session:

```bash
dcode-ai update          # or: dcode-ai update --check
```

The installer offers `~/.local/bin` when `/usr/local/bin` needs sudo, so
updates never need sudo.

<details>
<summary>Pin a version, install without sudo, or build from source</summary>

```bash
# a specific release
curl -sSL https://raw.githubusercontent.com/Dhanuzh/dcode-ai/main/install.sh | DCODE_AI_VERSION=v1.0.0 bash

# user-local, no sudo
curl -sSL https://raw.githubusercontent.com/Dhanuzh/dcode-ai/main/install.sh | DCODE_AI_INSTALL_DIR="$HOME/.local/bin" bash

# from source (Rust 1.88+)
cargo build --release
sudo cp target/release/dcode-ai /usr/local/bin/
```

Prebuilt for Linux x86_64 and arm64, macOS (Intel and Apple silicon) and
Windows x86_64. Release archives ship `.sha256` checksums, which the installer verifies. On Windows,
`search_code` uses ripgrep: `winget install BurntSushi.ripgrep.MSVC`.

</details>

## Quick start

```bash
cd my-project
dcode-ai
```

The first launch asks you to sign in:

| Provider | Sign in | How it works |
| --- | --- | --- |
| **Claude** | `dcode-ai login anthropic` | Runs your own signed-in `claude` as the model; usage counts against your Claude plan. dcode-ai's tools do the work. |
| **GitHub Copilot** | `dcode-ai login copilot` | GitHub device login. `/models` lists everything on your plan (Claude, GPT, Gemini, …). |

Switch any time with `/provider` and `/model` (Claude aliases: `opus`, `sonnet`, `haiku`, `fable`).
Prefer API billing? Set `[provider.anthropic] backend = "api"` and sign in with Anthropic's `ant` CLI —
see [configuration](docs/configuration.md).

```bash
dcode-ai -p "explain the architecture of this project"   # one-shot
dcode-ai -c                                              # continue the last session
dcode-ai spawn --prompt "refactor the auth module"       # detached, attach later
```

## Features

### In the terminal

| | |
| --- | --- |
| **Modes** | `Shift+Tab` cycles default → ⏵⏵ accept edits → ⏸ plan. Plan turns end with *Ready to code?* |
| **Rewind** | `Esc` `Esc` restores code, conversation, or both to any earlier message |
| **Context** | `@file` mentions, `[Image #N]` paste (`Ctrl+V`, `Alt+V` on Windows/WSL), `!cmd` shell, `# note` memory |
| **Dashboard** | `Ctrl+\` opens a full-screen view: sessions and agents, transcript, and context · changes · agents tabs (`Alt+1/2/3`) with per-hunk review and revert |
| **Insight** | `/context` usage grid, `/cost`, `/effort`, `Ctrl+T` thinking, `Ctrl+O` fold tool output |
| **Themes** | Claude's dark, light, colorblind-friendly and ANSI-only themes with a live `/theme` preview, plus `tokyonight`, `catppuccin`, `gruvbox`, `dracula`, `nord` … |
| **Accessible** | `--plain` output for screen readers and dumb terminals |

Press `?` on an empty prompt for every shortcut.

### Agents and automation

- **Sub-agents** run in their own git worktrees with parent/child lineage; when one finishes with changes you choose merge, keep or discard. Define your own in `agents/*.md`.
- **`/btw`** answers a side question mid-turn without adding it to the conversation.
- **Browser control** — the agent drives a real Chrome/Edge window to test web apps (`/browser`).
- **`/verify`** sends a read-only reviewer over your uncommitted diff and hands real problems back.
- **Headless**: `--json`, `--stream ndjson`, Unix-socket IPC for detached sessions, stable exit codes.
- **CI**: `dcode-ai ci` prints a JSON result, writes a GitHub step summary, and exits 0 / 1 / 2 (budget). Also a GitHub Action.
- **Editors**: `dcode-ai acp` speaks the Agent Client Protocol (Zed and others), including reopening sessions and image prompts.

### Extensible the Claude Code way

- **Starter packs** — `dcode-ai setup` installs plugins for your role (Backend, UI/UX, DevOps, Automation, Data, Security) and languages; optional, shareable as a team file.
- **Plugins and marketplaces** — `/plugins` browser; Anthropic's official, community and knowledge-work marketplaces are added for you (~2,700 plugins).
- **MCP** — `/mcp` panel with server status, in-session OAuth sign-in (`c` copies the URL), reconnect and sign out.
- **MCP** — `.mcp.json` (local, project, user), `dcode-ai mcp add|list|get|remove`, and `dcode-ai mcp serve` to expose dcode-ai's tools to other agents.
- **Skills, commands, agents, hooks** — `SKILL.md`, `commands/**/*.md`, `agents/*.md`, `hooks.json`, a status-line command.
- **Language servers** — picked per project (rust-analyzer, typescript, pyright, gopls, clangd) or from `.lsp.json` / plugin `lspServers`.

### Safe by default

- Permission modes from *plan* (read-only) to *bypass*; edits and unknown commands ask first.
- Shell commands are parsed, not substring-matched: read-only commands just run, destructive ones are refused.
- `policy.toml` allow / ask / deny rules, checked with `dcode-ai policy test execute_bash "git push"`.
- Optional sandbox for shell commands: Landlock on Linux, Seatbelt on macOS.
- Spending cap: `[session] max_cost_usd`.

### Built-in tools

Files (`read_file`, `edit_file`, `apply_patch`, `write_file`, …) · search (`search_code`, `code_intel`, `repo_map`) ·
shell (`execute_bash`, `run_background`, `run_validation`) · git · web (`web_search`, `fetch_url`) ·
`inspect_binary` (static look inside ELF / PE / Mach-O files) · `spawn_subagent`, `ask_question`, `update_plan`, `invoke_skill`.

## Configuration

```toml
# ~/.dcode-ai/config.toml   (per project: .dcode-ai/config.local.toml)
[provider]
default = "anthropic"          # anthropic | copilot

[provider.anthropic]
backend = "claude-code"        # your Claude account; "api" = Console OAuth via `ant`
model = "claude-sonnet-5-5"

[permissions]
mode = "default"               # plan | default | accept-edits | dont-ask | bypass-permissions
sandbox_bash = true

[ui]
theme = "claude"
```

Every option is in [docs/configuration.md](docs/configuration.md).

## Documentation

| Guide | What's inside |
| --- | --- |
| [Usage](docs/usage.md) | Keys, slash commands, CLI, sessions, sub-agents, permissions, tools, automation |
| [Extensibility](docs/extensibility.md) | MCP, plugins, skills, commands, custom agents, hooks, language servers, ACP, CI |
| [Configuration](docs/configuration.md) | Every config key, providers and sign-in |
| [IPC & NDJSON](docs/ipc-ndjson.md) | Event envelopes and socket commands for orchestration |
| [Architecture](docs/architecture.md) | Crates, the turn loop, token accounting |
| [Benchmarks](docs/benchmarks.md) | Measuring performance before and after a change |
| [Changelog](CHANGELOG.md) | What changed in each release |

## Development

```bash
cargo build                 # debug build
cargo test --workspace      # all tests
cargo clippy -- -D warnings
```

The workspace has five crates: `common` (shared types), `core` (agent loop, providers, tools),
`runtime` (sessions, IPC, supervision), `cli` (terminal UX) and `autoresearch`.
New work starts as a plan in [`docs/plans/`](docs/plans/).

## License

MIT © Dhanush
