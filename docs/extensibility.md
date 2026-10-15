# Extending dcode-ai

dcode-ai reads Claude Code's open file formats, so existing plugins, MCP
configs, skills, commands, agents and hooks work without changes.

- [Plugins and marketplaces](#plugins-and-marketplaces)
- [MCP servers](#mcp-servers)
- [Skills](#skills)
- [Custom commands](#custom-commands)
- [Custom agents](#custom-agents)
- [Hooks](#hooks)
- [Output styles](#output-styles)
- [Status line](#status-line)
- [Language servers](#language-servers)
- [Model routing](#model-routing)
- [Editors (ACP)](#editors-acp)
- [CI](#ci)

## Plugins and marketplaces

A plugin bundles commands, skills, agents, MCP servers, hooks and language
servers in the Claude Code layout:

```text
my-plugin/
├── .claude-plugin/plugin.json   # {"name": "my-plugin", "version": "1.0.0", "description": "…"}
├── commands/**/*.md             # → /my-plugin:<command>
├── skills/<skill>/SKILL.md      # → /my-plugin:<skill>
├── agents/*.md                  # → my-plugin:<agent>
├── .mcp.json                    # ${CLAUDE_PLUGIN_ROOT} expands to the plugin dir
└── hooks/hooks.json
```

**`/plugins`** opens the browser:

| Tab | Keys |
| --- | --- |
| Discover | type to search · `Space` install / uninstall · `Enter` details |
| Installed | `Space` enable / disable · `u` update · `x` `x` uninstall |
| Marketplaces | `a` add · `u` update · `x` `x` remove |
| Errors | problems loading plugins |

The official marketplace (`anthropics/claude-plugins-official`) is added the
first time it opens. Changes apply to the running session immediately; after
editing `.mcp.json` or hooks by hand, run `/reload`.

```bash
dcode-ai plugin marketplace add anthropics/claude-plugins-official   # owner/repo, git URL or dir
dcode-ai plugin search review
dcode-ai plugin install code-review@claude-plugins-official
dcode-ai plugin install ./my-plugin
dcode-ai plugin list
dcode-ai plugin disable my-plugin        # enable · update [name] · remove
```

Plugins live in `~/.dcode-ai/plugins/`, marketplaces in `~/.dcode-ai/marketplaces/`.
Plugins distributed through npm are not supported (dcode-ai has no Node runtime).

## MCP servers

Servers come from `[[mcp.servers]]` in config and from `.mcp.json` files:

| Scope | File |
| --- | --- |
| local (personal) | `.dcode-ai/mcp.json` |
| project (commit it) | `.mcp.json` |
| user | `~/.dcode-ai/mcp.json` |

On a name clash TOML wins, then local > project > user > plugins. `${VAR}` and
`${VAR:-default}` are expanded. Tools appear as `mcp__<server>__<tool>`.

```bash
dcode-ai mcp add files -- npx -y @modelcontextprotocol/server-filesystem .
dcode-ai mcp add docs --scope project --transport http \
  -H 'Authorization: Bearer ${DOCS_TOKEN}' https://example.com/mcp
dcode-ai mcp list                # name · source · transport
dcode-ai mcp get docs            # secrets masked
dcode-ai mcp remove docs
```

In the TUI, `/mcp` lists servers and their health; `/mcp test <name>` connects
and lists its tools.

**Many tools:** when servers bring more than 15 tools
(`[mcp] tool_search_threshold`, `0` = never), their schemas are not sent with
every request. The model sees their names and loads the ones it needs with
`tool_search` (keywords, or `select:<name>`), like Claude Code's deferred
tools. This keeps big servers from filling the context.

**dcode-ai as a server:** `dcode-ai mcp serve` exposes dcode-ai's read and
search tools over stdio (`--allow-writes` adds edit tools), for example
`claude mcp add dcode -- dcode-ai mcp serve`.

## Skills

Skills are directories with a `SKILL.md`, found in:

- `.dcode-ai/skills/` and `.claude/skills/` in the workspace
- `~/.dcode-ai/skills/`
- `AGENTS.md` sections and installed plugins

They load on demand through the `invoke_skill` tool or as `/skill-name`.
`dcode-ai skills list|add|remove|update` manages them.

## Custom commands

A Markdown file becomes a slash command: `.dcode-ai/commands/<name>.md` (or
`.claude/commands/`, workspace or user level) runs as `/name`.

```markdown
---
description: Review a file for bugs
model: sonnet
allowed-tools: Bash(git diff:*)
---
Review $ARGUMENTS for correctness bugs. Current diff:
!`git diff`
```

- `$ARGUMENTS` is everything typed after the command; `$1`…`$9` are single words.
- `` !`cmd` `` lines run before sending (when `allowed-tools` permits them or the
  command is read-only) and their output is inlined.
- Tools in `allowed-tools` are pre-approved for that command's turn only.
- Sub-directories namespace commands: `commands/git/log.md` → `/git:log`.

## Custom agents

Sub-agents defined as Markdown in `.dcode-ai/agents/` or `.claude/agents/`
(workspace or user), or in a plugin's `agents/`:

```markdown
---
name: test-runner
description: Runs the test suite and fixes failures. Use after code changes.
tools: Read, Grep, Bash, Edit      # optional; Claude Code or dcode-ai names
model: sonnet                      # optional; omit or `inherit` for the parent's model
---
You are a test specialist. Run the tests, read failures, fix them …
```

The main agent sees them in `spawn_subagent`; the child runs with only the
listed tools and the body as its instructions. `/agents` lists definitions and
past sub-agent sessions.

## Hooks

```toml
[[hooks.user_prompt_submit]]           # before a prompt reaches the model
command = "./scripts/check-prompt.sh"  # receives {session_id, prompt} on stdin
blocking = true                        # non-zero exit stops the turn; stderr is shown

[[hooks.pre_tool_use]]
matcher = "execute_bash"
command = "./scripts/audit.sh"

[[hooks.stop]]                         # after every turn
command = "notify-send dcode-ai done"
```

Events: `session_start`, `session_end`, `user_prompt_submit`, `pre_tool_use`,
`post_tool_use`, `post_tool_failure`, `approval_requested`, `pre_compact`,
`subagent_start`, `subagent_stop`, `stop`. Plugin `hooks/hooks.json` files map
onto these.

## Output styles

`/output-style` switches how replies are written: **Default**, **Explanatory**
(adds `★ Insight` notes about choices and patterns) or **Learning** (hands
small pieces of code to you with `TODO(human)`). Custom styles are Markdown
files in `output-styles/` under `.dcode-ai/` or `.claude/` (project or home):

```markdown
---
name: Reviewer
description: Findings first, one line each
---
Answer like a senior code reviewer …
```

The choice is saved in `[ui] output_style`.

## Status line

```toml
[ui]
statusline_command = "~/.dcode-ai/statusline.sh"
```

The command gets session JSON on stdin (model, cwd, permission mode, tokens,
cost) every second; its first output line replaces the right side of the footer.

## Language servers

`code_intel` (definitions, references, symbols, diagnostics) picks the server
for the project's dominant language, preferring ones that are installed:
`rust-analyzer`, `typescript-language-server`, `pyright-langserver`, `gopls`,
`clangd`. Add more in `.lsp.json`, `~/.dcode-ai/lsp.json` or a plugin's
`lspServers`:

```json
{ "lspServers": {
    "ruby": { "command": "ruby-lsp", "args": ["--stdio"],
              "extensionToLanguage": { ".rb": "ruby" } } } }
```

Without a server it falls back to a fast local index.

## Model routing

```toml
[model]
compact_model = "haiku"      # conversation summaries
subagent_model = "sonnet"    # sub-agents without their own `model:`

[session]
max_cost_usd = 5.0           # stop at $5 estimated spend (0 = off)
```

## Editors (ACP)

`dcode-ai acp` speaks the [Agent Client Protocol](https://agentclientprotocol.com)
over stdio: streamed replies and thinking, tool calls with results, the plan,
approvals as the editor's permission prompts, reopening saved sessions
(`session/load`) and image prompts. In Zed's `settings.json`:

```json
"agent_servers": {
  "dcode-ai": { "command": "dcode-ai", "args": ["acp"] }
}
```

dcode-ai reads and writes files and runs commands itself; it does not yet use
the editor's unsaved buffers or terminals.

## CI

```bash
dcode-ai ci --prompt "Review this PR for bugs"                     # read-only
dcode-ai ci --prompt-file task.md --allow-writes --max-cost 2      # edits; --allow-shell adds commands
```

Prints a JSON result (`status`, `output`, `changed_files`, tokens, cost), appends
a Markdown summary to `$GITHUB_STEP_SUMMARY`, and exits `0` completed, `1`
failed, `2` budget reached. As a GitHub Action:

```yaml
- uses: Dhanuzh/dcode-ai@main
  with:
    prompt: "Fix the failing test in tests/auth.rs"
    copilot-token: ${{ secrets.COPILOT_GITHUB_TOKEN }}
    allow-writes: true
    max-cost: "1.50"
```
