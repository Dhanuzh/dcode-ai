<div align="center">

# ✻ dcode-ai

### The Rust-native coding agent for your terminal

One binary · your Claude or GitHub Copilot sign-in · no API keys, no Electron, no Node

[![Release](https://img.shields.io/github/v/release/Dhanuzh/dcode-ai?style=for-the-badge&color=d97757&labelColor=211d1b)](https://github.com/Dhanuzh/dcode-ai/releases)
[![CI](https://img.shields.io/github/actions/workflow/status/Dhanuzh/dcode-ai/ci.yml?branch=main&style=for-the-badge&label=CI&labelColor=211d1b)](https://github.com/Dhanuzh/dcode-ai/actions)
[![Rust](https://img.shields.io/badge/rust-1.88%2B-e8e2dc?style=for-the-badge&logo=rust&logoColor=white&labelColor=211d1b)](https://www.rust-lang.org)
[![License: MIT](https://img.shields.io/badge/license-MIT-7fbf7f?style=for-the-badge&labelColor=211d1b)](#license)

**[Install](#-install)** · **[Quick start](#-quick-start)** · **[Features](#-features)** · **[Configuration](#%EF%B8%8F-configuration)** · **[Docs](#-documentation)** · **[Changelog](CHANGELOG.md)**

<br>

<img src="docs/assets/hero.svg" alt="dcode-ai in a terminal: it reads a file, shows a diff of its edit and reports that the tests pass" width="860">

</div>

<br>

## ✨ Why dcode-ai

<table>
<tr>
<td width="50%" valign="top">

### 💳 Uses what you already pay for
Your **Claude plan** (through your own signed-in `claude`) or your **GitHub Copilot** subscription. OAuth sign-in only, no API keys to paste.

</td>
<td width="50%" valign="top">

### 🧩 Speaks the Claude Code ecosystem
Plugins, marketplaces, `.mcp.json`, skills, commands, agents and hooks work unchanged, with about 2,700 plugins a keystroke away.

</td>
</tr>
<tr>
<td width="50%" valign="top">

### 🚀 Built to spawn work
Detached sessions, sub-agents in isolated git worktrees, background agents, `/goal` loops, NDJSON streams, Unix-socket IPC and a CI mode.

</td>
<td width="50%" valign="top">

### ⚡ Small, fast, careful with credit
One Rust binary that starts instantly. Lean prompts, tools loaded on demand, prompt caching, automatic compaction, and `/usage` shows where every token goes.

</td>
</tr>
</table>

## 📦 Install

<table>
<tr><th>Linux / macOS</th><th>Homebrew</th><th>Windows (PowerShell)</th></tr>
<tr>
<td>

```bash
curl -sSL https://raw.githubusercontent.com/Dhanuzh/dcode-ai/main/install.sh | bash
```

</td>
<td>

```bash
brew tap dhanuzh/dcode-ai https://github.com/Dhanuzh/dcode-ai
brew install dcode-ai
```

</td>
<td>

```powershell
irm https://raw.githubusercontent.com/Dhanuzh/dcode-ai/main/install.ps1 | iex
```

</td>
</tr>
</table>

Prebuilt for **Linux** (x86_64, arm64), **macOS** (Intel, Apple silicon) and **Windows** (x86_64). Every archive ships a `.sha256` checksum, and the installer verifies it.

**Staying current:** run `/update` inside a session, or `dcode-ai update` (`--check` only looks). When `/usr/local/bin` needs sudo, the installer offers `~/.local/bin` instead, so updates never need sudo. If you do pick a system folder, `/update` asks for the password in a masked popup.

<details>
<summary><b>Pin a version, install without sudo, or build from source</b></summary>

```bash
# a specific release
curl -sSL https://raw.githubusercontent.com/Dhanuzh/dcode-ai/main/install.sh | DCODE_AI_VERSION=v1.0.28 bash

# user-local, no sudo
curl -sSL https://raw.githubusercontent.com/Dhanuzh/dcode-ai/main/install.sh | DCODE_AI_INSTALL_DIR="$HOME/.local/bin" bash

# from source (Rust 1.88+)
cargo build --release
sudo cp target/release/dcode-ai /usr/local/bin/
```

On Windows, `search_code` uses ripgrep: `winget install BurntSushi.ripgrep.MSVC`.

</details>

## 🏁 Quick start

```bash
cd my-project
dcode-ai
```

The first launch asks you to sign in:

| Provider | Sign in | How it works |
| --- | --- | --- |
| **Claude** | `dcode-ai login anthropic` | Runs your own signed-in `claude` as the model, so usage counts against your Claude plan. dcode-ai's own tools do the work. |
| **GitHub Copilot** | `dcode-ai login copilot` | GitHub device login. `/models` lists everything on your plan (Claude, GPT, Gemini, …). Only your own prompts count as premium requests. |

Switch any time with `/provider` and `/model` (Claude aliases: `opus`, `sonnet`, `haiku`, `fable`). Prefer API billing? Set `[provider.anthropic] backend = "api"` and sign in with Anthropic's `ant` CLI. See [configuration](docs/configuration.md).

```bash
dcode-ai -p "explain the architecture of this project"   # one-shot answer
dcode-ai -c                                              # continue the last session
dcode-ai -r                                              # pick a session to resume
dcode-ai spawn --prompt "refactor the auth module"       # detached; attach later
```

> [!TIP]
> Press `?` on an empty prompt for every shortcut, and type `/` to browse all slash commands.

## 🧰 Features

### ⌨️ In the terminal

| | |
| --- | --- |
| **Permission modes** | `Shift+Tab` cycles default → ⏵⏵ accept edits → ⏸ plan → ⏵⏵ **auto** (auto runs everything except deletions and risky commands) |
| **Talk while it works** | `Enter` steers the running turn, and `Tab` queues a message for after it |
| **Rewind** | `Esc` `Esc` restores code, conversation or both to any earlier message |
| **Context** | `@` fuzzy file search, `[Image #N]` paste (`Ctrl+V`, or `Alt+V` on Windows/WSL), `!` shell mode, `# note` memory |
| **Dashboard** | `Ctrl+\` opens a full-screen view with sessions and agents, the transcript, and context · changes · agents tabs. Review and revert per hunk, drag to select and copy, resize the rails |
| **Diffs** | Approvals show a line-numbered diff, and `/diff` opens a full-screen viewer with hunk revert |
| **Insight** | `/context` grid, `/usage` token breakdown (fresh, cached, output, cache hit rate), `/cost`, `/effort`, `Ctrl+T` thinking |
| **Looks** | Claude's dark, light, colorblind-friendly and ANSI themes with live `/theme` preview, plus `tokyonight`, `catppuccin`, `gruvbox`, `dracula`, `nord` … |
| **Comfort** | Desktop notifications, `c` copies links, `/terminal-setup` for Shift+Enter, `/vim` keys, `--plain` for screen readers |

### 👀 See it, share it

- **Agent Office** (`/office`): every agent as an animated pixel-art character at a desk — typing, reading, running commands, raising a hand when it needs you.
- **Live file map**: every file the agent reads and edits, as a tree that lights up while it works (dashboard `Alt+4`, `/files`).
- **`/timeline`**: scrub through the session step by step; jump back to any of your messages.
- **`/watch`**: a live, read-only web page of the session for a teammate (or your phone); local by default, `/watch lan` for your network.
- **`dcode-ai join <id>`**: a second terminal (or a teammate over SSH) on the same running session.
- **`/handoff <note>`**: hand a session to a teammate as one file, with what's done and what's next.
- **`/sounds`** and **`/speak`**: a bell when it needs you; the last reply read aloud.

### 🗂️ Sessions that last

- **Crash-safe:** the session saves after every step of a turn, so a crash or a closed terminal loses nothing.
- **`/resume` picker:** a tree of sessions with their forks and sub-agents, a search over titles *and* what was said, and auto-generated titles.
- **Automatic compaction**, like Claude Code: older history is summarized by a cheap model when the context fills up, including in the middle of a long turn.
- **`/export` / `/share`** the conversation as Markdown with secrets masked, to a file, the clipboard or a secret gist.

### 🤖 Agents and automation

- **Sub-agents** run in their own git worktrees with parent/child lineage. When one finishes with changes, you choose to merge, keep or discard them. They can also run in the background while you keep chatting.
- **`/goal <objective>`** keeps working across turns until the objective is done, within a budget you set.
- **Fallback models** take over when the main one is overloaded.
- **`/btw`** answers a side question mid-turn without adding it to the conversation.
- **Browser control:** the agent drives a real Chrome or Edge window. It follows your tabs, waits for text to appear, takes screenshots it can see, and records steps you can replay with `/browser save`.
- **Reviews:** `/verify` (read-only reviewer over your diff), `/second-opinion` (a model from another family), `/security-review`, `/pr-comments`.
- **Headless:** `--json`, `--stream ndjson`, Unix-socket IPC for detached sessions, and stable exit codes.
- **CI:** `dcode-ai ci` prints a JSON result, writes a GitHub step summary and exits 0 / 1 / 2 (budget). It also ships as a GitHub Action.
- **Editors:** `dcode-ai acp` speaks the Agent Client Protocol (Zed and others).

### 🧩 Extensible the Claude Code way

- **Starter packs:** `dcode-ai setup` installs plugins for your role (Backend, UI/UX, DevOps, Automation, Data, Security) and your languages. It's optional and can be shared as a team file.
- **Plugins and marketplaces:** the `/plugins` browser comes with Anthropic's official, community and knowledge-work marketplaces already added.
- **MCP:** `.mcp.json` (local, project, user scopes) and a `/mcp` panel with status, in-session OAuth sign-in, token refresh, reconnect and per-folder on/off. `dcode-ai mcp serve` exposes dcode-ai's own tools to other agents.
- **Skills, commands, agents, hooks:** `SKILL.md`, `commands/**/*.md`, `agents/*.md`, `hooks.json` and a status-line command.
- **Language servers:** picked per project (rust-analyzer, typescript, pyright, gopls, clangd) or configured in `.lsp.json`.

### 🛡️ Safe by default

- Permission modes from *plan* (read-only) to *bypass*. Edits and unfamiliar commands ask first.
- Shell commands are parsed, not substring-matched: read-only ones just run and destructive ones are refused.
- `policy.toml` allow / ask / deny rules, testable with `dcode-ai policy test execute_bash "git push"`.
- Optional shell sandbox: Landlock on Linux, Seatbelt on macOS.
- Secrets are masked in tool output and exports. A spending cap is available via `[session] max_cost_usd`.

### 🔧 Built-in tools

**Files:** `read_file` (with line ranges, PDF and notebooks), `edit_file`, `apply_patch`, `write_file`, `notebook_edit` …
**Search:** `search_code`, `code_intel`, `repo_map` · **Shell:** `execute_bash`, `run_background`, `run_validation`
**Git** · **Web:** `web_search`, `fetch_url`, `browser` · **Agents:** `spawn_subagent`, `ask_question`, `update_plan`, `invoke_skill`
Rarely used tools load on demand through `tool_search`, which keeps every request small.

## ⚙️ Configuration

```toml
# ~/.dcode-ai/config.toml     (per project: .dcode.toml, shared · .dcode-ai/config.local.toml, yours)
[provider]
default = "anthropic"          # anthropic | copilot

[provider.anthropic]
backend = "claude-code"        # your Claude account; "api" = Console OAuth via `ant`
model = "claude-sonnet-5-5"

[permissions]
mode = "default"               # plan | default | accept-edits | auto | dont-ask | bypass-permissions
sandbox_bash = true

[ui]
theme = "claude"
working_style = "bar"          # bar | dots | plain
```

Every option is documented in [docs/configuration.md](docs/configuration.md).

## 📚 Documentation

| Guide | What's inside |
| --- | --- |
| [**Usage**](docs/usage.md) | Keys, slash commands, CLI, sessions, sub-agents, permissions, tools, browser, automation |
| [**Extensibility**](docs/extensibility.md) | MCP, plugins, skills, commands, custom agents, hooks, language servers, ACP, CI |
| [**Configuration**](docs/configuration.md) | Every config key, providers and sign-in |
| [**IPC & NDJSON**](docs/ipc-ndjson.md) | Event envelopes and socket commands for orchestration |
| [**Architecture**](docs/architecture.md) | Crates, the turn loop, token accounting |
| [**Benchmarks**](docs/benchmarks.md) | Measuring performance before and after a change |
| [**Changelog**](CHANGELOG.md) | What changed in each release |

## 🛠️ Development

```bash
cargo build                                   # debug build
cargo test --workspace                        # all tests
cargo clippy --workspace --all-targets -- -D warnings
```

| Crate | Role |
| --- | --- |
| `common` | Shared types, config, events |
| `core` | Agent loop, providers, tools |
| `runtime` | Sessions, compaction, IPC, supervision |
| `cli` | The `dcode-ai` binary and terminal UX |
| `autoresearch` | Experiment loops |

New work starts as a plan in [`docs/plans/`](docs/plans/).

## License

MIT © Dhanush

<div align="center">
<sub>Built in Rust · made for people who live in the terminal</sub>
</div>
