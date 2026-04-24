# Configuration

dcode-ai uses a layered TOML configuration system with environment variable overrides.

## Config File Locations

| File | Scope | Description |
|------|-------|-------------|
| `~/.dcode-ai/config.toml` | Global | User-wide defaults |
| `<workspace>/.dcode-ai/config.local.toml` | Workspace | Project-specific overrides |

Settings merge in order: **defaults → global → workspace → environment variables**. Later sources override earlier ones.

## Full Configuration Reference

### `[provider]` — LLM Provider Settings

```toml
[provider]
default = "minimax"   # "minimax" | "openrouter" | "anthropic" | "openai"

[provider.minimax]
api_key_env = "MINIMAX_API_KEY"     # Environment variable to read
api_key = ""                         # Or set directly (not recommended)
base_url = "https://api.minimax.io/anthropic"
model = "MiniMax-M2.7"
temperature = 0.7

[provider.openai]
api_key_env = "OPENAI_API_KEY"
base_url = "https://api.openai.com"
model = "gpt-4o-mini"
temperature = 0.7

[provider.anthropic]
api_key_env = "ANTHROPIC_API_KEY"
base_url = "https://api.anthropic.com"
model = "claude-3-7-sonnet-latest"
temperature = 1.0

[provider.openrouter]
api_key_env = "OPENROUTER_API_KEY"
base_url = "https://openrouter.ai/api"
model = "openai/gpt-4o-mini"
temperature = 0.7
site_url = ""       # Optional referrer URL
app_name = ""       # Optional app name header
```

### `[model]` — Model Settings

```toml
[model]
default_model = "MiniMax-M2.7"
max_tokens = 8192
enable_thinking = false
thinking_budget = 5120

[model.aliases]
# Built-in aliases (pre-configured):
# default    → MiniMax-M2.7
# minimax    → MiniMax-M2.7
# m2.7       → MiniMax-M2.7
# coding     → MiniMax-M2.7
# reasoning  → MiniMax-M2.7
# openai     → gpt-4o-mini
# gpt4o      → gpt-4o
# claude     → claude-3-7-sonnet-latest
# openrouter → openai/gpt-4o-mini

# Add your own:
fast = "gpt-4o-mini"
smart = "claude-3-7-sonnet-latest"
```

### `[permissions]` — Permission System

```toml
[permissions]
mode = "default"   # "default" | "plan" | "accept-edits" | "dont-ask" | "bypass-permissions"

# Pattern-based allow/deny lists (supports wildcards)
allow = []         # e.g., ["execute_bash:cargo *", "write_file:src/*"]
deny = []          # e.g., ["execute_bash:rm *", "delete_path:*"]
ask = []           # Force ask for specific patterns
```

See [Permissions](./permissions.md) for full details on each mode.

### `[session]` — Session Management

```toml
[session]
history_dir = ".dcode-ai/sessions"       # Relative to workspace
max_turns_per_run = 128             # Max agent turns per session run
max_tool_calls_per_turn = 200       # Max tool calls in a single turn
checkpoint_interval = 5             # Save checkpoint every N turns
last_session_file = ".dcode-ai/.last_session"
auto_compact_on_finish = false      # Auto-summarize when session ends
```

### `[harness]` — System Prompt and Instructions

```toml
[harness]
built_in_enabled = true                           # Include dcode-ai's built-in system prompt
project_instructions_path = ".dcode-airc"              # Project instructions file
local_instructions_path = ".dcode-ai/instructions.md"  # Local (personal) instructions
skill_directories = [".dcode-ai/skills", ".claude/skills"]  # Skill discovery paths
```

### `[mcp]` — Model Context Protocol

```toml
[mcp]
expose_in_safe_mode = false   # Allow MCP tools in safe/read-only mode

[[mcp.servers]]
name = "my-server"
command = "npx"
args = ["-y", "@my/mcp-server"]
env = { API_KEY = "..." }
cwd = "/optional/working/directory"
enabled = true
```

### `[memory]` — Persistent Memory

```toml
[memory]
file_path = ".dcode-ai/memory.json"
max_notes = 128
auto_compact_on_finish = false

[memory.context]
context_window_target = 0              # 0 = auto-detect from provider
auto_detect_context_window = true
query_provider_models_api = true       # Fetch model limits from provider API
max_retained_messages = 50
auto_summarize_threshold = 75          # Percentage of context window used before summarizing
enable_auto_summarize = true
```

### `[hooks]` — Lifecycle Hooks

```toml
# Shell commands that run at various lifecycle points
[hooks]
session_start = []
session_end = []
pre_tool_use = []
post_tool_use = []
post_tool_failure = []
approval_requested = []
subagent_start = []
subagent_stop = []
```

Each hook is an object with:

```toml
[[hooks.session_start]]
command = "echo 'session started'"
matcher = ""        # Optional regex to match on
blocking = false    # If true, waits for completion
```

### `[web]` — Web Request Settings

```toml
[web]
timeout_secs = 15
max_fetch_chars = 25000
default_search_limit = 5
user_agent = "dcode-ai/0.5 (+https://github.com/user/dcode-ai)"
```

### `[ui]` — Interface Settings

```toml
[ui]
editor = ""               # External editor command (e.g., "vim", "code --wait")
theme = ""                # UI theme (optional)
hide_tips = false          # Hide usage tips
scroll_speed = 3           # Scroll speed in TUI
mouse_capture = true       # Enable TUI mouse capture (disables normal terminal selection)
code_line_numbers = false  # Show line numbers in fenced code blocks (TUI assistant output)
onboarding_completed = false
```

---

## Environment Variables

Environment variables override config file values.

### Provider Selection and Keys

| Variable | Description |
|----------|-------------|
| `DCODE_AI_DEFAULT_PROVIDER` | Override default provider (`minimax`, `openrouter`, `anthropic`, `openai`) |
| `DCODE_AI_MODEL` | Override the active model |
| `MINIMAX_API_KEY` | MiniMax API key |
| `MINIMAX_BASE_URL` | MiniMax API base URL |
| `MINIMAX_MODEL` | MiniMax model name |
| `OPENAI_API_KEY` | OpenAI API key |
| `OPENAI_BASE_URL` | OpenAI-compatible base URL |
| `OPENAI_MODEL` | OpenAI model name |
| `ANTHROPIC_API_KEY` | Anthropic API key |
| `ANTHROPIC_BASE_URL` | Anthropic API base URL |
| `ANTHROPIC_MODEL` | Anthropic model name |
| `OPENROUTER_API_KEY` | OpenRouter API key |
| `OPENROUTER_BASE_URL` | OpenRouter base URL |
| `OPENROUTER_MODEL` | OpenRouter model name |
| `OPENROUTER_SITE_URL` | OpenRouter site URL header |
| `OPENROUTER_APP_NAME` | OpenRouter app name header |

### Runtime Behavior

| Variable | Description |
|----------|-------------|
| `DCODE_AI_EDITOR` | Override external editor command |
| `DCODE_AI_EDITOR_MODE` | Set to `vi` or `vim` for vi keybindings in REPL |
| `DCODE_AI_MEMORY_PATH` | Override memory file path |
| `DCODE_AI_WEB_TIMEOUT_SECS` | Override web request timeout |
| `DCODE_AI_WEB_MAX_FETCH_CHARS` | Override max characters for web fetches |
| `DCODE_AI_DEBUG_REQUEST` | Enable debug logging for MiniMax requests |
| `DCODE_AI_SKIP_CONTEXT_API` | Set to `1` to skip provider model API queries |
| `DCODE_AI_CONTEXT_API_CACHE_TTL_SECS` | Cache TTL for model context API |
| `XDG_RUNTIME_DIR` | IPC socket directory (fallback: `/tmp/dcode-ai/`) |

### Orchestration (CI/Automation)

| Variable | Description |
|----------|-------------|
| `DCODE_AI_ORCH_NAME` | Orchestrator name |
| `DCODE_AI_ORCH_RUN_ID` | Orchestration run identifier |
| `DCODE_AI_ORCH_TASK_ID` | Task identifier |
| `DCODE_AI_ORCH_TASK_REF` | Task reference |
| `DCODE_AI_ORCH_PARENT_RUN_ID` | Parent run ID |
| `DCODE_AI_ORCH_CALLBACK_URL` | Callback URL for orchestrator |
| `DCODE_AI_ORCH_META_*` | Arbitrary metadata (prefix stripped, key lowercased) |

---

## Example Configurations

### Minimal Setup

```toml
# ~/.dcode-ai/config.toml
[provider]
default = "minimax"

[provider.minimax]
api_key = "your-key-here"
```

### Multi-Provider Setup

```toml
# ~/.dcode-ai/config.toml
[provider]
default = "minimax"

[provider.minimax]
api_key_env = "MINIMAX_API_KEY"

[provider.anthropic]
api_key_env = "ANTHROPIC_API_KEY"

[provider.openai]
api_key_env = "OPENAI_API_KEY"

[model.aliases]
fast = "gpt-4o-mini"
smart = "claude-3-7-sonnet-latest"
default = "MiniMax-M2.7"
```

### CI/Automation Setup

```toml
# .dcode-ai/config.local.toml (in the project)
[permissions]
mode = "bypass-permissions"

[session]
max_turns_per_run = 50
auto_compact_on_finish = true
```

### Workspace with Custom Instructions and MCP

```toml
# .dcode-ai/config.local.toml
[harness]
project_instructions_path = ".dcode-airc"
local_instructions_path = ".dcode-ai/instructions.md"

[[mcp.servers]]
name = "database"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-postgres"]
env = { DATABASE_URL = "postgresql://localhost/mydb" }
enabled = true
```
