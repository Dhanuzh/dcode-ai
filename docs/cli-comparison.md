# CLI Coding Agents: Comprehensive Comparison

**Generated:** Analysis of dcode-ai vs Claude Code, Aider, Continue.dev, SWE-agent, Devin, and others.

---

## 1. dcode-ai (Rust-native, 2025)

| Attribute | Detail |
|-----------|--------|
| **Language** | Rust (single static binary, no Node.js/Python) |
| **UI** | Full TUI (ratatui), streaming, themes, mouse capture (F12) |
| **Providers** | OpenAI, Anthropic, OpenRouter, **OpenCode Zen (big-pickle/MiniMax)**, OpenAI-compatible |
| **Thinking** | Live streaming `✦ thinking` tokens visible in real-time |
| **Tool visibility** | `⚡ bash  ls -la  running… → ✓ bash` — visible with status |
| **Sessions** | Per-workspace, resumable, NDJSON event log, full lineage |
| **Sub-agents** | Git worktree isolation, visible parent→child lineage |
| **Permissions** | Tiered policy (`Denied`/`Ask`/`Allowed`) + Unix-socket approval IPC |
| **IPC** | Unix domain sockets + NDJSON |
| **Persistence** | `<id>.json` (state) + `<id>.events.jsonl` (event log) |
| **Hooks** | PreToolUse, PostToolUse, PostToolFailure via JSON |
| **Context** | Compactable history, checkpointing, harness instructions |
| **Skills** | AGENTS.md sections, `.dcode-ai/skills/`, `.claude/skills/` compatible |
| **Modes** | TUI, one-shot (`run`), detached (`spawn` + `attach`) |
| **Output** | `--json`, `--stream ndjson`, `--stream text` |
| **Themes** | `/theme` picker with live preview |
| **Branch picker** | Status-bar chip or `/branch` |
| **Mouse** | F12 toggle (wheel scroll vs. text selection) |
| **Approvals** | Interactive via Unix socket IPC, headless-aware |
| **Speed** | Tokio async runtime, thin LTO release build |
| **Install** | `cargo build --release && cp target/release/dcode-ai /usr/local/bin/` |

---

## 2. Claude Code (Anthropic, Feb 2025)

| Attribute | Detail |
|-----------|--------|
| **Language** | Go (native binary) |
| **UI** | REPL/TUI, streaming, light/dark themes |
| **Providers** | Anthropic only (Haiku/Sonnet/Opus), AWS Bedrock, Google Vertex, MS Foundry |
| **Thinking** | Extended thinking mode (configurable budget), not live-streamed |
| **Tool visibility** | Implicit in conversation prose |
| **Sessions** | JSONL transcripts, resumable, forkable |
| **Sub-agents** | Up to 10 parallel, clean contexts, return summaries |
| **Permissions** | Allow/Deny/Ask rules with pattern matching (prefix, not regex) |
| **IPC** | Primarily stdio / subprocess |
| **Persistence** | `.claude/` JSONL transcripts, `~/.claude.json` state |
| **Hooks** | PostToolUse shell commands (deterministic automation) |
| **Context** | 200K tokens (1M with premium), auto-compaction |
| **Skills** | Markdown files auto-applied on context match |
| **Modes** | Interactive REPL, `-p` print mode, `-c` resume |
| **Output** | Plain text, JSON (`--output-format json`) |
| **MCP** | Full MCP protocol support, 300+ integrations |
| **Plugins** | Packaged extensions with MCP + hooks + skills |
| **Remote** | SSH remote execution support |
| **Enterprise** | Managed settings, `/etc/claude-code/managed-settings.json` |
| **Install** | `curl -fsSL https://claude.ai/install.sh \| bash` |

### Key Strengths vs dcode-ai
- **MCP ecosystem**: 300+ server integrations (GitHub, databases, Sentry, etc.)
- **Enterprise deployment**: Managed settings, SSO, audit trails
- **SSH remote**: Work on remote servers via SSH
- **Plugin system**: Distributable extension bundles
- **Opus/Sonnet/Haiku**: Purpose-built for Anthropic models

### Key Weaknesses vs dcode-ai
- **Single provider lock-in**: No OpenAI, no OpenRouter, no MiniMax
- **No live thinking stream**: Extended thinking is opaque
- **No visible tool status**: Tool calls buried in prose
- **No git worktree isolation**: Sub-agents share repo context
- **No Unix socket IPC**: No programmatic session control
- **No caveman/lite mode**: No token-efficiency features
- **No NDJSON event streams**: Limited automation friendliness
- **No multi-provider fallback**: Stuck on Anthropic pricing

---

## 3. Aider (Paul J. Lu, Python, 2014–2025)

| Attribute | Detail |
|-----------|--------|
| **Language** | Python (pip install) |
| **UI** | Terminal REPL, Markdown chat, ANSI colors |
| **Providers** | OpenAI, Anthropic, Gemini, DeepSeek, Groq, Ollama, LM Studio, xAI, Azure, Cohere, OpenRouter, GitHub Copilot, Vertex AI, Bedrock, any OpenAI-compatible |
| **Thinking** | No native thinking stream (depends on model) |
| **Tool visibility** | File diffs shown after edits |
| **Sessions** | Git commits track every change, map files |
| **Sub-agents** | Architect/Editor dual-model mode (Architect plans, Editor edits) |
| **Permissions** | Trust-level CLI flags (`--no-commit`, `--read-only`) |
| **IPC** | Python API, subprocess stdio |
| **Persistence** | Git commits, `.aider*` config files |
| **Hooks** | Linting/testing automation on save |
| **Context** | Repository map (whole-repo awareness), tree-sitter language pack |
| **Skills** | No native skill system |
| **Modes** | Chat modes: `code`, `architect`, `ask`, `help` |
| **Output** | Plain text, `--json` output |
| **Edit formats** | Search/replace, whole-file, unified diff |
| **Multi-model** | Architect (planning) + Editor (editing) simultaneous |
| **Voice** | Voice-to-code via microphone |
| **Benchmarking** | Own LLM leaderboards (code editing, refactoring) |
| **Install** | `pip install aider-chat` or Docker |

### Key Strengths vs dcode-ai
- **Multi-model leaderboard**: Quantitative benchmarks for LLM code editing
- **Repository map**: Whole-codebase awareness from day one
- **Dual Architect/Editor**: Simultaneous planning + execution models
- **Git-first**: Every change committed, full history
- **Voice-to-code**: Speech input
- **Most provider options**: 15+ provider families
- **100+ languages**: Via tree-sitter

### Key Weaknesses vs dcode-ai
- **Python dependency**: Not a static binary
- **No TUI**: Plain terminal REPL, no streaming UI
- **No thinking stream**: No live reasoning visibility
- **No visible tool status**: Edits shown post-hoc
- **No sub-agent isolation**: Architect/Editor share context
- **No Unix socket IPC**: Limited automation
- **No event log persistence**: Just git commits + chat history
- **No permission policy system**: Trust-level flags only
- **No worktree isolation**: No parallel isolated exploration

---

## 4. Continue.dev (CLI, TypeScript/Go hybrid)

| Attribute | Detail |
|-----------|--------|
| **Language** | TypeScript (VS Code extension) + Go CLI |
| **UI** | VS Code sidebar + CLI REPL |
| **Providers** | OpenAI, Anthropic, Azure, Ollama, LM Studio, custom |
| **Thinking** | Model-dependent |
| **Tool visibility** | VS Code inline decorations |
| **Sessions** | VS Code workspace context |
| **Sub-agents** | Limited (no native worktree isolation) |
| **Permissions** | VS Code workspace permissions |
| **IPC** | VS Code extension protocol |
| **Persistence** | VS Code state |
| **Context** | Codebase index, embeddings |
| **Skills** | Custom prompts per project |
| **Modes** | Inline autocomplete + chat |
| **Output** | VS Code UI |
| **Embeddings** | Local embeddings for code search |
| **Install** | VS Code marketplace / npm |

### Key Strengths vs dcode-ai
- **IDE integration**: VS Code first-class
- **Local embeddings**: Offline code search
- **Inline autocomplete**: IDE-like suggestions

### Key Weaknesses vs dcode-ai
- **IDE dependency**: Needs VS Code
- **No standalone CLI**: Not terminal-first
- **No visible thinking stream**: No live reasoning
- **No permission policy**: Bound to VS Code permissions
- **No Unix socket IPC**: No programmatic control
- **No event log**: No audit trail

---

## 5. SWE-agent (Princeton, Python, 2023–2025)

| Attribute | Detail |
|-----------|--------|
| **Language** | Python |
| **UI** | Terminal REPL, Docker sandbox output |
| **Providers** | OpenAI, Anthropic, Gemini, local models |
| **Thinking** | No native thinking stream |
| **Tool visibility** | Command traces in terminal |
| **Sessions** | SWE-bench benchmark format |
| **Sub-agents** | No native sub-agents |
| **Permissions** | Docker sandbox isolation |
| **IPC** | Stdout/stderr pipes |
| **Persistence** | Benchmark results JSON |
| **Context** | SWE-bench issue format |
| **Skills** | No native skill system |
| **Modes** | SWE-bench evaluation mode |
| **Output** | JSON benchmark results |
| **Focus** | Academic benchmark agent research |
| **Install** | `pip install swe-agent` |

### Key Strengths vs dcode-ai
- **SWE-bench leaderboard**: Gold standard for code repair agents
- **Docker sandbox**: Clean execution environment
- **Academic rigor**: Reproducible benchmarks

### Key Weaknesses vs dcode-ai
- **Academic focus**: Not a general-purpose developer tool
- **No TUI**: Plain terminal
- **No thinking stream**: No live reasoning
- **No visible tool status**: Command traces only
- **No session persistence**: Benchmark format
- **No Unix socket IPC**: No programmatic control
- **No permission policy**: Docker-based only
- **No skills**: Not extensible
- **No multi-provider abstraction**: Limited provider support

---

## 6. Devin (Cognition, 2024)

| Attribute | Detail |
|-----------|--------|
| **Language** | Web/cloud service (no CLI binary) |
| **UI** | Web dashboard, no CLI |
| **Providers** | Proprietary Cognition models |
| **Thinking** | Proprietary reasoning |
| **Tool visibility** | Web UI activity feed |
| **Sessions** | Web dashboard session management |
| **Sub-agents** | Proprietary task decomposition |
| **Permissions** | Web dashboard controls |
| **IPC** | API only |
| **Persistence** | Cloud |
| **Context** | Proprietary |
| **Skills** | Proprietary |
| **Modes** | Web UI only |
| **Output** | Web dashboard |
| **Focus** | Autonomous software engineering |

### Key Strengths vs dcode-ai
- **Autonomous execution**: Full task automation
- **Proprietary reasoning**: High-quality model

### Key Weaknesses vs dcode-ai
- **No CLI**: Web-only, not terminal-first
- **No local binary**: Cloud dependency
- **No visible thinking stream**: Proprietary
- **No Unix socket IPC**: API only
- **No self-hosted**: Locked to Cognition platform
- **No open architecture**: Not extensible

---

## Head-to-Head Comparison Matrix

| Feature | dcode-ai | Claude Code | Aider | Continue | SWE-agent | Devin |
|---------|:--------:|:-----------:|:-----:|:--------:|:---------:|:-----:|
| **Rust-native binary** | ✅ | ❌ (Go) | ❌ (Python) | ❌ (TS+Go) | ❌ (Python) | ❌ (Cloud) |
| **No Electron/Node** | ✅ | ✅ | ✅ | ❌ (VS Code) | ✅ | ❌ (Web) |
| **Live thinking stream** | ✅ | ❌ (opaque) | ❌ | ❌ | ❌ | ❌ |
| **Visible tool status** | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| **Git worktree isolation** | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| **Unix socket IPC** | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| **NDJSON event streams** | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| **Multi-provider (5+)** | ✅ | ❌ (Anthropic-only) | ✅ | ⚠️ (limited) | ⚠️ (limited) | ❌ |
| **MiniMax/big-pickle** | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| **Permission policy tiers** | ✅ | ✅ | ❌ | ❌ | ❌ | ❌ |
| **Session lineage** | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| **Hook system (JSON)** | ✅ | ⚠️ (shell only) | ⚠️ (linting) | ❌ | ❌ | ❌ |
| **Skills system** | ✅ | ✅ | ❌ | ⚠️ (prompts) | ❌ | ❌ |
| **Theme picker** | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| **Mouse capture toggle** | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| **Slash commands** | ✅ | ✅ | ✅ | ❌ | ❌ | ❌ |
| **Image attach** | ✅ | ✅ | ✅ | ❌ | ❌ | ❌ |
| **TUI with streaming** | ✅ | ✅ | ❌ (REPL) | ❌ (VS Code) | ❌ (REPL) | ❌ (Web) |
| **JSON/NDJSON output** | ✅ | ✅ (JSON) | ⚠️ (basic) | ❌ | ⚠️ (benchmark) | ❌ |
| **Detached + attach** | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| **Context compaction** | ✅ | ✅ | ❌ | ❌ | ❌ | ❌ |
| **Checkpointing** | ✅ | ✅ | ❌ | ❌ | ❌ | ❌ |
| **MCP protocol** | ❌ | ✅ | ❌ | ⚠️ (partial) | ❌ | ❌ |
| **Plugin system** | ❌ | ✅ | ❌ | ❌ | ❌ | ❌ |
| **SSH remote** | ❌ | ✅ | ❌ | ❌ | ❌ | ❌ |
| **Enterprise/SSO** | ❌ | ✅ | ❌ | ❌ | ❌ | ✅ |
| **Voice-to-code** | ❌ | ❌ | ✅ | ❌ | ❌ | ❌ |
| **Repo map** | ❌ | ❌ | ✅ | ❌ | ❌ | ❌ |
| **Architect/Editor dual** | ❌ | ❌ | ✅ | ❌ | ❌ | ❌ |
| **SWE-bench** | ❌ | ❌ | ❌ | ❌ | ✅ | ❌ |
| **Leaderboards** | ❌ | ❌ | ✅ | ❌ | ❌ | ❌ |
| **Caveman/lite mode** | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |
| **Token efficiency skills** | ✅ | ❌ | ❌ | ❌ | ❌ | ❌ |

---

## Summary: What Makes dcode-ai Unique

### dcode-ai Advantages (unmatched)

1. **Rust-native single binary** — No Python, no Node, no Electron. Fast startup, minimal footprint.
2. **Live thinking stream** — See model reasoning as it happens (`✦ thinking`). No other CLI shows this.
3. **Visible tool execution** — Every tool call shows name + args + status in real-time.
4. **Git worktree isolation** — Sub-agents run in isolated branches, safe for parallel exploration.
5. **Unix socket IPC + NDJSON** — Programmatic session control, pipe-friendly automation.
6. **MiniMax (big-pickle) provider** — First-class support alongside Anthropic/OpenAI/OpenRouter.
7. **Permission policy tiers** — `Denied`/`Ask`/`Allowed` with hooks, headless-aware.
8. **Session lineage** — Parent/child tracking visible in metadata and events.
9. **Theme picker + mouse toggle** — Rich TUI features not found elsewhere.
10. **Caveman token-efficiency skills** — `/caveman`, `/caveman-commit`, `/caveman-review`, `/compress`.

### Where dcode-ai Lags

| Gap | Impact | Mitigation |
|-----|--------|------------|
| No MCP protocol | Can't connect 300+ external services | Plan: MCP client implementation |
| No SSH remote | Can't work on remote servers | Plan: SSH tunnel support |
| No enterprise/SSO | Not suitable for large orgs | Plan: Enterprise config layer |
| No VS Code plugin | No IDE integration | Plan: LSP integration |
| Smaller community | Fewer docs, examples | Build it via OSS growth |
| No repo map | Less codebase awareness | Plan: Code map generator |
| No benchmark leaderboards | Hard to measure vs. alternatives | Consider adding SWE-bench |

---

## Architecture Comparison

```
dcode-ai Architecture:
┌─────────────────────────────────────────────────────┐
│ CLI (ratatui TUI, REPL, streaming)                  │
│ crates/cli/src/                                      │
├─────────────────────────────────────────────────────┤
│ Runtime (session lifecycle, IPC, persistence)       │
│ crates/runtime/src/service.rs, session_store.rs      │
├─────────────────────────────────────────────────────┤
│ Core (agent loop, providers, tools, hooks)           │
│ crates/core/src/agent.rs, provider.rs, tools/         │
├─────────────────────────────────────────────────────┤
│ Common (types, events, config, messages)             │
│ crates/common/src/                                   │
└─────────────────────────────────────────────────────┘
→ Single binary, Unix sockets, NDJSON, tokio async

Claude Code Architecture:
┌─────────────────────────────────────────────────────┐
│ CLI (Go binary, REPL)                               │
├─────────────────────────────────────────────────────┤
│ MCP Layer (300+ servers)                            │
├─────────────────────────────────────────────────────┤
│ Subagent Layer (up to 10 parallel)                  │
├─────────────────────────────────────────────────────┤
│ Core Layer (tools, context, permissions)             │
├─────────────────────────────────────────────────────┤
│ Hooks Layer (shell commands, deterministic)          │
└─────────────────────────────────────────────────────┘
→ Go binary, MCP protocol, SSH remote, enterprise

Aider Architecture:
┌─────────────────────────────────────────────────────┐
│ REPL (Python, chat interface)                       │
├─────────────────────────────────────────────────────┤
│ Architect Model (planning) + Editor Model (editing)  │
├─────────────────────────────────────────────────────┤
│ Git Integration (commits per change)                │
├─────────────────────────────────────────────────────┤
│ Repository Map (whole-codebase awareness)           │
├─────────────────────────────────────────────────────┤
│ 15+ Provider adapters (OpenAI, Anthropic, etc.)     │
└─────────────────────────────────────────────────────┘
→ Python, git-first, dual-model, leaderboards
```

---

## Conclusion

**dcode-ai** is the only CLI agent that combines:
- **Rust-native performance** (single binary, no Python/Node)
- **Live thinking visibility** (real-time reasoning stream)
- **Visible tool execution** (status-tracked tool calls)
- **Git worktree isolation** (safe parallel sub-agents)
- **Unix socket IPC** (programmatic automation)
- **MiniMax provider** (big-pickle support)

It's positioned as the **terminal-first, token-efficient, Rust-native** alternative to Claude Code (Go, Anthropic-only) and Aider (Python, git-first). The main gaps are MCP protocol support and enterprise features — both are reasonable next steps given the clean Rust architecture.
