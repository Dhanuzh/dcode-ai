# Configuration

dcode-ai is configured via TOML. Two locations, merged with local overriding global:

| Scope   | Path                                      |
| ------- | ----------------------------------------- |
| Global  | `~/.dcode-ai/config.toml`                 |
| Project | `<workspace>/.dcode-ai/config.local.toml` |

All sections are optional; every field has a default (see
`crates/common/src/config.rs`). API keys resolve from the config value first,
then the environment variable named by `api_key_env`, then the OAuth auth store.

## `[provider]` — backend selection

```toml
[provider]
default = "anthropic"   # anthropic | openai | openrouter | minimax | opencodezen

[provider.anthropic]
api_key_env = "ANTHROPIC_API_KEY"   # env var to read the key from
api_key     = ""                    # or set the key inline (overrides env)
base_url    = "https://api.anthropic.com"
model       = "claude-sonnet-4-6"
temperature = 1.0

[provider.openai]
api_key_env = "OPENAI_API_KEY"
base_url    = "https://api.openai.com/v1"
model       = "gpt-4o"
temperature = 1.0

[provider.openrouter]
api_key_env = "OPENROUTER_API_KEY"
base_url    = "https://openrouter.ai/api/v1"
model       = "..."
site_url    = ""        # optional OpenRouter attribution headers
app_name    = ""
```

Anthropic requires an `sk-ant-*` key — OAuth/subscription tokens are rejected by
`api.anthropic.com/v1/messages`. Prompt caching is automatic for this provider.

## `[model]` — generation behavior

```toml
[model]
default_model   = "claude-sonnet-4-6"
max_tokens      = 8192
enable_thinking = true        # stream extended-thinking tokens
thinking_budget = 4096
aliases         = { fast = "claude-haiku-4-5-20251001" }   # /model fast
```

## `[permissions]` — what the agent may do

```toml
[permissions]
mode = "default"   # default | plan | acceptEdits | dontAsk | bypass
allow = []          # tool/command patterns auto-approved
deny  = []          # patterns always blocked
ask   = []          # patterns that force a prompt
startup_approve_all = false
```

## `[session]` — persistence & loop limits

```toml
[session]
history_dir            = ".dcode-ai/sessions"
max_turns_per_run      = 100
max_tool_calls_per_turn = 50
checkpoint_interval    = 1
auto_compact_on_finish = false
```

## `[harness]` — instructions & skills

```toml
[harness]
built_in_enabled          = true
project_instructions_path = ".dcode-airc"
local_instructions_path   = ".dcode-ai/instructions.md"
skill_directories         = [".dcode-ai/skills"]
include_repo_map          = false   # prepend a PageRank-ranked repo map to the
                                     # system prompt (top 25 files). Off by default
                                     # — it adds tokens every session.
```

## `[mcp]` — Model Context Protocol servers

Two transports. **stdio** spawns a local process; **Streamable HTTP** posts
JSON-RPC to a URL. A server with `url` set uses HTTP; otherwise it spawns `command`.

```toml
[mcp]
expose_in_safe_mode = false

# stdio transport
[[mcp.servers]]
name    = "my-server"
command = "node"
args    = ["server.js"]
enabled = true
# env = { KEY = "value" }, cwd = "..."

# Streamable-HTTP transport
[[mcp.servers]]
name    = "remote"
url     = "https://mcp.example.com/rpc"
enabled = true
# Auth / extra headers. Values support ${ENV_VAR} expansion so secrets
# stay out of the file.
headers = { Authorization = "Bearer ${MCP_TOKEN}" }
```

The HTTP transport accepts both `application/json` and `text/event-stream`
responses and threads the `Mcp-Session-Id` header through after initialize.

## `[hooks]` — lifecycle shell commands

Each is a list of `HookCommand`. Available events:
`session_start`, `session_end`, `pre_tool_use`, `post_tool_use`,
`post_tool_failure`, `approval_requested`, `subagent_start`, `subagent_stop`.

```toml
[[hooks.pre_tool_use]]
command = "echo about to run a tool"
```

## `[web]` — web search / fetch

Configures the `web_search` and `fetch_url` tools (provider keys, enable flags).

## `[memory]` — persistent project memory

```toml
[memory]
file_path = ".dcode-ai/memory.json"
```

## `[ui]` — terminal UI

```toml
[ui]
theme             = "..."     # or use /theme
editor            = "vim"
mouse_capture     = true
code_line_numbers = true
scroll_speed      = 3
hide_tips         = false
```
