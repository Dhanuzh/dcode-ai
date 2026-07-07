# Codex CLI vs dcode-ai Comparison

| Feature | Codex CLI | dcode-ai |
|---------|-----------|----------|
| **Developer** | OpenAI | Dhanush (Independent) |
| **Language** | Rust | Rust |
| **License** | Apache-2.0 | Open Source |
| **Primary Model** | OpenAI models (GPT-4, o1, etc.) | MiniMax M2.5 (default), supports multiple providers |
| **Installation** | `curl -fsSL https://chatgpt.com/codex/install.sh \| sh`<br>`npm install -g @openai/codex`<br>`brew install --cask codex` | `curl -sSL https://raw.githubusercontent.com/Dhanuzh/dcode-ai/main/install.sh \| bash`<br>`cargo build --release` |
| **Terminal UI** | Full-screen TUI | Full TUI with command palette, themes, mouse support |
| **Streaming** | Yes | Yes with live reasoning tokens (`✦ thinking`) |
| **Tool Execution** | Visible | Visible with name + argument preview + live status |
| **Session Persistence** | Yes | Yes (JSON state + JSONL event log) |
| **Session Resumption** | Yes | Yes with resume, replay, attach |
| **Sub-agents** | Yes (with worktree isolation) | Yes (with worktree isolation, parent/child lineage) |
| **Worktrees** | Yes | Yes (git worktrees for isolated agent runs) |
| **Permission Modes** | - `suggest` (default)<br>- `auto-edit`<br>- `full-auto`<br>- `sandbox` | - `default` (read/web auto-allowed, edits/commands ask)<br>- `plan` (analysis only)<br>- `accept-edits` (file edits auto-accepted)<br>- `dont-ask` (read-only automatic)<br>- `bypass-permissions` (fully autonomous) |
| **Sandbox** | Yes (containerized) | Yes (Landlock on Linux) |
| **Providers** | OpenAI only | MiniMax, Anthropic, OpenAI, OpenRouter, OpenAI-compatible |
| **Model Switching** | Via config | `/model` command, `F2` cycle |
| **Provider Switching** | No | `/provider` command |
| **Local Models** | No | `/connect ollama`, `/connect lmstudio`, `/connect vllm` |
| **File Operations** | Read, write, edit | Read, write, edit, rename, move, copy, delete |
| **Code Search** | Ripgrep-based | Ripgrep-based + LSP code intelligence |
| **Shell Execution** | Yes | Yes (PTY-backed, sandboxed) |
| **Git Integration** | Yes | Yes (status, diff, worktrees, branches) |
| **Web Access** | Yes (internet access) | Yes (DuckDuckGo search, URL fetch) |
| **Image Support** | Yes (screenshots) | Yes (`@file` mentions, `Ctrl+V` image paste) |
| **Custom Instructions** | `AGENTS.md` | `AGENTS.md`, `.dcode-airc`, `.dcode-ai/instructions.md`, skills |
| **Skills System** | Yes | Yes (auto-discovered from multiple directories) |
| **Commands** | `/agent`, `/status`, `/compact`, etc. | `/model`, `/provider`, `/theme`, `/permission`, `/agent`, `/sessions`, `/branch`, `/compact`, `/new`, `/resume`, `/editor`, `/apikey`, `/connect`, `/memory`, `/doctor` |
| **Theme Support** | Limited | Yes (`/theme` with live preview) |
| **Agent Profiles** | Yes | Yes (`@build`, `@plan`, `@review`, `@fix`, `@test`) |
| **Headless Mode** | Yes | Yes (`run`, `spawn`, `attach`, `status`, `logs`) |
| **JSON Output** | Yes | Yes (`--json`, `--stream ndjson`) |
| **IPC** | No | Yes (Unix domain sockets for programmatic control) |
| **Orchestration** | Limited | Yes (env vars for orchestration context) |
| **Exit Codes** | Limited | Defined (0, 1, 10, 11, 13, 130) |
| **IDE Integration** | VS Code, Cursor, Windsurf | Terminal-first (no IDE integration) |
| **Desktop App** | Yes (`codex app`) | No |
| **Cloud Agent** | Yes (Codex Web) | No |
| **Documentation** | Comprehensive | Comprehensive |
| **Community** | Large (OpenAI) | Growing (independent) |
| **Pricing** | Included in ChatGPT plans | Free (requires API keys) |

## Key Differences

1. **Provider Lock-in**: Codex CLI is tied to OpenAI models, while dcode-ai supports multiple providers (MiniMax, Anthropic, OpenAI, OpenRouter, local models).

2. **Permission System**: dcode-ai has a more granular permission system with 5 modes vs Codex's 4 modes, plus Landlock sandboxing on Linux.

3. **TUI Features**: dcode-ai offers more TUI features like command palette (`Ctrl+P`), theme picker, and live reasoning token streaming.

4. **Session Management**: dcode-ai has more robust session management with JSONL event logs and programmatic IPC control.

5. **Local Model Support**: dcode-ai can connect to local models via Ollama, LM Studio, or vLLM, while Codex CLI cannot.

6. **Skills System**: dcode-ai has a more flexible skills system that auto-discovers skills from multiple directories.

7. **Agent Profiles**: Both have agent profiles, but dcode-ai's profiles are more integrated with the TUI (Tab cycling).

8. **Sandbox**: Codex uses containerized sandboxing, while dcode-ai uses Landlock on Linux (lighter weight but less isolation).

9. **Platform Support**: Codex CLI supports Windows, macOS, and Linux. dcode-ai focuses on Unix-like systems (Linux, macOS).

10. **Maturity**: Codex CLI is backed by OpenAI with extensive resources, while dcode-ai is an independent project with a growing feature set.

## When to Use Which

**Use Codex CLI when:**
- You're already in the OpenAI ecosystem
- You need Windows support
- You want container-based sandboxing
- You prefer an established tool with large community support
- You need IDE integration (VS Code, Cursor, Windsurf)

**Use dcode-ai when:**
- You want multi-provider support (MiniMax, Anthropic, OpenRouter, local models)
- You prefer a more granular permission system
- You need programmatic control via IPC
- You want a richer TUI experience with themes and live reasoning
- You work primarily in Unix-like environments
- You want to use local models via Ollama, LM Studio, or vLLM