# Commands

Complete reference for all `dcode-ai` CLI commands, subcommands, and flags.

## Global Usage (No Subcommand)

```bash
dcode-ai [OPTIONS]
```

When invoked without a subcommand, dcode-ai starts an interactive session. Behavior depends on flags:

| Flag | Short | Type | Default | Description |
|------|-------|------|---------|-------------|
| `--prompt` | `-p` | string | — | Run a one-shot prompt and exit |
| `--safe` | `-s` | flag | false | Start in read-only safe mode |
| `--resume` | `-r` | flag | false | Resume the most recent session |
| `--no-resume` | — | flag | false | Force a new session (skip auto-resume) |
| `--run` | — | flag | false | Start interactive run mode |
| `--model` | — | string | — | Override the default model |
| `--enable-thinking` | `-t` | flag | false | Enable extended thinking/reasoning |
| `--thinking-budget` | — | u32 | 5120 | Token budget for extended thinking |
| `--max-tokens` | — | u32 | 8192 | Max response tokens |
| `--verbose` | `-v` | flag | false | Verbose debug logging |
| `--json` | — | flag | false | Output structured JSON (for CI) |
| `--stream` | — | enum | `human` | Stream format: `human`, `ndjson`, or `off` |
| `--no-tui` | — | flag | false | Use line-oriented REPL instead of full-screen TUI |
| `--permission-mode` | — | enum | — | Permission mode (see [Permissions](./permissions.md)) |
| `--max-turns` | — | u32 | — | Max agent turns per run |

### Examples

```bash
# Start interactive TUI session
dcode-ai

# One-shot prompt
dcode-ai -p "refactor the error handling in src/lib.rs"

# Safe mode (read-only analysis)
dcode-ai -s

# Resume last session
dcode-ai -r

# Force new session
dcode-ai --no-resume

# Use a specific model
dcode-ai --model "claude-3-7-sonnet-latest"

# Enable thinking with custom budget
dcode-ai -t --thinking-budget 10000

# CI-friendly JSON output
dcode-ai -p "list all TODO comments" --json

# NDJSON event stream
dcode-ai -p "fix the tests" --stream ndjson

# Line REPL (no full-screen TUI)
dcode-ai --no-tui

# Bypass all permission prompts
dcode-ai --permission-mode bypass-permissions
```

---

## Subcommands

### `dcode-ai run`

Run a one-shot task with explicit stream control.

```bash
dcode-ai run --prompt "..." [OPTIONS]
```

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--prompt` | string | **required** | The task to execute |
| `--stream` | enum | `human` | Stream format: `human`, `ndjson`, `off` |
| `--model` | string | — | Override model |
| `--json` | flag | false | Structured JSON output |
| `--safe` | flag | false | Read-only mode |
| `--permission-mode` | enum | — | Permission mode |

```bash
dcode-ai run --prompt "add input validation" --stream ndjson
dcode-ai run --prompt "analyze code quality" --safe --json
```

---

### `dcode-ai spawn`

Launch a background session that runs without interactive input.

```bash
dcode-ai spawn --prompt "..." [OPTIONS]
```

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--prompt` | string | **required** | The task to execute |
| `--model` | string | — | Override model |
| `--safe` | flag | false | Read-only mode |
| `--json` | flag | false | Structured JSON output |
| `--permission-mode` | enum | `accept-edits` | Permission mode |

```bash
dcode-ai spawn --prompt "write comprehensive tests for the auth module"
dcode-ai spawn --prompt "document all public APIs" --model "MiniMax-M2.7"
```

---

### `dcode-ai sessions`

List and filter saved sessions.

```bash
dcode-ai sessions [OPTIONS]
```

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--json` | flag | false | JSON output |
| `--status` | enum | — | Filter by status: `running`, `completed`, `cancelled`, `failed` |
| `--since-hours` | u32 | — | Show sessions from the last N hours |
| `--search` | string | — | Search sessions by text |
| `--limit` | usize | 20 | Max number of sessions to show |

```bash
dcode-ai sessions
dcode-ai sessions --status running
dcode-ai sessions --since-hours 24 --limit 5
dcode-ai sessions --search "auth" --json
```

---

### `dcode-ai resume`

Resume a previously saved session.

```bash
dcode-ai resume <SESSION_ID> [OPTIONS]
```

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--prompt` | string | — | Send a follow-up prompt after resuming |
| `--model` | string | — | Override model for this session |
| `--safe` | flag | false | Resume in read-only mode |
| `--stream` | enum | `human` | Stream format |
| `--no-tui` | flag | false | Use line REPL |
| `--permission-mode` | enum | — | Permission mode |

```bash
dcode-ai resume abc123
dcode-ai resume abc123 --prompt "continue where you left off"
dcode-ai resume abc123 --model "claude-3-7-sonnet-latest"
```

---

### `dcode-ai logs`

Stream or dump the event log for a session.

```bash
dcode-ai logs <SESSION_ID> [OPTIONS]
```

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--follow` | flag | false | Follow the log in real-time (like `tail -f`) |
| `--json` | flag | false | Raw JSON output |

```bash
dcode-ai logs abc123
dcode-ai logs abc123 --follow
dcode-ai logs abc123 --json
```

---

### `dcode-ai attach`

Attach to a running session's output stream.

```bash
dcode-ai attach <SESSION_ID> [--json]
```

---

### `dcode-ai status`

Show metadata for a session.

```bash
dcode-ai status <SESSION_ID> [--json]
```

---

### `dcode-ai cancel`

Cancel a running session.

```bash
dcode-ai cancel <SESSION_ID> [--json]
```

---

### `dcode-ai skills`

Manage agent skills.

```bash
dcode-ai skills [--json]              # List all discovered skills
dcode-ai skills list [--json]         # Same as above
dcode-ai skills add <SOURCE> [OPTIONS]
dcode-ai skills remove <NAME> [OPTIONS]
dcode-ai skills update [NAME]         # Update one or all skills
```

**`dcode-ai skills add`:**

| Flag | Short | Type | Default | Description |
|------|-------|------|---------|-------------|
| `--skill` | `-s` | string[] | all | Specific skills to install (repeatable) |
| `--global` | `-g` | flag | false | Install globally to `~/.dcode-ai/skills/` |

**`dcode-ai skills remove`:**

| Flag | Short | Type | Default | Description |
|------|-------|------|---------|-------------|
| `--global` | `-g` | flag | false | Remove from global skills |

```bash
dcode-ai skills
dcode-ai skills add https://github.com/user/skill-repo -s rust-patterns
dcode-ai skills remove rust-patterns --global
dcode-ai skills update
```

---

### `dcode-ai mcp`

List configured MCP (Model Context Protocol) servers.

```bash
dcode-ai mcp [--json]
```

---

### `dcode-ai memory`

Manage persistent memory notes.

```bash
dcode-ai memory [--json]              # List memory notes
dcode-ai memory list [--json]         # Same as above
dcode-ai memory add <TEXT> [--kind <KIND>]
```

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--kind` | string | `note` | Type of memory entry |

```bash
dcode-ai memory
dcode-ai memory add "prefer async/await over thread spawning"
dcode-ai memory add "API uses bearer token auth" --kind note
```

---

### `dcode-ai models`

List available models for the current provider.

```bash
dcode-ai models [--json]
```

---

### `dcode-ai doctor`

Run diagnostic checks on your configuration.

```bash
dcode-ai doctor [--json]
```

Checks API key availability, provider connectivity, config file validity, and tool dependencies.

---

### `dcode-ai config`

Display the current runtime configuration.

```bash
dcode-ai config [--json]
```

---

### `dcode-ai completion`

Generate shell completion scripts.

```bash
dcode-ai completion <SHELL>
```

Supported shells: `bash`, `zsh`, `fish`, `power-shell`, `elvish`. Default: `bash`.

---

### `dcode-ai index`

Manage the CLI index cache (used for agent self-awareness of available commands).

```bash
dcode-ai index build [--json]
dcode-ai index show [--json]
```

---

### `dcode-ai autoresearch`

Run automated research on a program/topic.

```bash
dcode-ai autoresearch once <PROGRAM> [--workspace <PATH>]
```

| Flag | Type | Default | Description |
|------|------|---------|-------------|
| `--workspace` | path | current directory | Workspace for research output |

---

## Stream Modes

The `--stream` flag controls output format:

| Mode | Description |
|------|-------------|
| `human` | Terminal-friendly output with colors, markdown rendering, and TUI support (default) |
| `ndjson` | Newline-delimited JSON events — one event per line, for machine consumption |
| `off` | Minimal output — final result only |

## Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | General error (provider failure, config issue, etc.) |
| non-zero | Task failure or cancellation |
