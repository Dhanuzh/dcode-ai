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
default = "anthropic"   # anthropic | copilot

[provider.anthropic]
backend       = "claude-code"       # claude-code (your Claude account, default) | api
base_url      = "https://api.anthropic.com"
model         = "claude-opus-5-5"   # aliases: opus, sonnet, haiku, fable
effort        = "high"              # low | medium | high | xhigh | max
oauth_profile = ""                  # `ant` profile; empty = active profile
temperature   = 1.0                 # only sent to models that still accept it

[provider.copilot]                  # GitHub Copilot — `dcode-ai login copilot`
model          = "gpt-4.1"          # any id from /models (e.g. claude-sonnet-5)
api_key_env    = "COPILOT_GITHUB_TOKEN"  # optional GitHub token for CI
# editor_version / editor_plugin_version / integration_id / user_agent:
# client identity headers sent to the Copilot API (override if needed)
```

Older configs that name removed providers (`openai`, `openrouter`, `opencodezen`,
`antigravity`, …) still load: those sections are ignored and the default falls back
to Claude.

**`backend = "claude-code"` (default)** uses your Claude account through your own
signed-in Claude Code binary (`claude auth login`). dcode-ai runs `claude -p` as a
model only — built-in tools, MCP, settings, skills and CLAUDE.md loading are all
off and its system prompt is replaced — and describes dcode-ai's own tools in the
system prompt. Claude calls them with `<tool_call>{json}</tool_call>` blocks, so
approvals, sandbox, undo and the freshness guard all apply. One `claude` process
per session receives only new messages each turn; history rewrites (compaction,
rewind) restart it with a transcript, as does a reply that invents
`<tool_result>` blocks (cut at that point). Usage counts against your Claude
plan's limits. `ANTHROPIC_API_KEY`/`ANTHROPIC_AUTH_TOKEN` are removed from the
child's environment so it always bills the account.

**`backend = "api"`**: Anthropic sign-in is OAuth through Anthropic's official
[`ant` CLI](https://github.com/anthropics/anthropic-cli/releases): run
`dcode-ai login anthropic` (or `/login anthropic` in the TUI), which drives
`ant auth login`. dcode-ai then asks `ant auth print-credentials --access-token`
for short-lived tokens (refreshed automatically) and sends them as
`Authorization: Bearer` with `anthropic-beta: oauth-2025-04-20`. Usage is billed
to the Anthropic Console org/workspace you pick at login. Claude.ai Pro/Max
subscription login is not supported — Anthropic's terms do not allow third-party
apps to use it.

Credential precedence: `ANTHROPIC_AUTH_TOKEN` (CI / workload identity) → `ant`
OAuth profile → legacy `sk-ant-*` key (only when `ant` is not installed).

Request shape follows the model: current models (Opus/Sonnet/Haiku 5.x,
Fable 5.x, Opus 4.7/4.8) get adaptive thinking with summarized reasoning,
`output_config.effort`, no sampling parameters, and a 64K `max_tokens` floor;
thinking blocks are replayed with their signatures. Prompt caching is automatic.

## `[model]` — generation behavior

```toml
[model]
default_model   = "claude-sonnet-4-6"
max_tokens      = 8192
enable_thinking = true        # stream extended-thinking tokens
thinking_budget = 4096
aliases         = { fast = "claude-haiku-4-5-20251001" }   # /model fast
```

Aliases and configured defaults are user-owned configuration. Available model
IDs are not bundled into dcode-ai; use `/models` or `dcode-ai models --json` to
read the active provider's live catalog.

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
cleanup_period_days    = 30   # delete sessions untouched this long, at startup (0 = keep)
max_cost_usd           = 0.0  # stop the agent at this estimated spend (0 = off)
turn_token_alarm       = 5000000  # pause a turn after this many tokens (0 = off)
verify_after_edits     = false
```

`dcode-ai sessions --prune <DAYS>` deletes old sessions on demand.

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
tool_search_threshold = 6    # more MCP tools than this → loaded on demand via tool_search (0 = never)
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

### `[[web.webhooks]]` — event webhooks

Each entry POSTs selected runtime events as JSON (the same `EventEnvelope`
shape as the session event log / SSE stream) to an external URL. Emitted from
service sessions (`dcode-ai serve`, `dcode-ai web`, spawned runs).

```toml
[[web.webhooks]]
url = "https://example.com/dcode-hook"
# Which events to send. Available:
#   SessionCompleted     — session ended (any reason)
#   ToolApprovalRequired — a tool call is waiting for approval
#   TurnCompleted        — the agent returned to idle after a turn
#   Error                — a turn failed
enabled_events = ["SessionCompleted", "ToolApprovalRequired"]
# Optional shared secret. When set, each request carries
#   X-Dcode-Signature: sha256=<hex>
# — an HMAC-SHA256 of the exact request body, keyed with this secret
# (GitHub-webhook style). Verify it server-side before trusting the payload.
secret_header = "my-shared-secret"
```

Delivery is fire-and-forget: failures are ignored and never block the agent.

## `[memory]` — persistent project memory

```toml
[memory]
file_path = ".dcode-ai/memory.json"
```

## `[ui]` — terminal UI

```toml
[ui]
theme             = "..."     # or use /theme
output_style      = "default" # or use /output-style (explanatory, learning, custom)
editor            = "vim"
mouse_capture     = true
code_line_numbers = true
scroll_speed      = 3
hide_tips         = false
```
