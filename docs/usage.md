# Using dcode-ai

Everything you do day to day: the terminal UI, commands, sessions, sub-agents,
permissions, tools and automation. For plugins, MCP and hooks see
[extensibility](extensibility.md); for config keys see [configuration](configuration.md).

- [Keys](#keys)
- [Slash commands](#slash-commands)
- [Command line](#command-line)
- [Sessions](#sessions)
- [Sub-agents](#sub-agents)
- [Permissions](#permissions)
- [Tools](#tools)
- [Automation](#automation)
- [Instructions and memory](#instructions-and-memory)

## Keys

Press `?` on an empty prompt (or `F1`) for the full list.

### Essentials

| Key | Action |
| --- | --- |
| `Enter` / `Shift+Enter` | Send / new line |
| `Shift+Tab` | Cycle mode: default → ⏵⏵ accept edits → ⏸ plan |
| `Esc` | Interrupt the running turn |
| `Esc` `Esc` | Rewind: pick a message, then restore code and conversation, conversation only, or code only |
| `1` / `2` / `3` | Answer a permission prompt: yes · yes, don't ask again · no |
| `Tab` | Complete `@file` or `/command`; otherwise cycle the agent profile |
| `F2` / `Shift+F2` | Next / previous model |
| `Ctrl+P` | Command palette |
| `/vim` | Vim keys in the prompt: Esc → NORMAL (`h l w b 0 $ x dd dw cw cc D C u p`), `i a A I o O` → INSERT |
| `Ctrl+B` | Move the running shell command to the background |
| `Ctrl+T` | Show or hide thinking |
| `Ctrl+O` | Fold or unfold tool output |
| `Ctrl+F` · `n` / `N` | Search the transcript |
| `Ctrl+R` | Search prompt history |
| `Alt+Enter` | Queue a follow-up while the agent is busy |

### In the prompt

| Input | Effect |
| --- | --- |
| `@path/to/file` | Mention a file (completes from the workspace) |
| `Ctrl+V` (`Alt+V` on Windows/WSL) | Paste an image as `[Image #N]`; dropping or pasting an image path works too |
| `!command` | Run a shell command (the input rule turns pink) |
| `# note` | Save a memory note for future sessions |
| `/command` | Slash commands, including plugin commands `/plugin:command` |

Thinking keywords raise reasoning effort for that message only, and are
highlighted as you type: `think hard` (high), `think harder` / `megathink`
(xhigh), `ultrathink` (max).

`[Image #N]` and `[Pasted text #N +L lines]` chips delete as one unit with
Backspace; deleting an image chip removes that image from the message.

### Dashboard

`Ctrl+\` (or `/dashboard`) switches to a full-screen view with sessions and
sub-agents on the left, the transcript in the middle and a tabbed rail on the
right.

| Key | Action |
| --- | --- |
| `Alt+1` / `Alt+2` / `Alt+3` | Context · changes · agents tab |
| `Ctrl+D` | Focus the rail (again to return to the prompt) |
| `←` / `→` (rail focused) | Previous / next tab |
| `j` / `k` (changes) | Select a hunk |
| `r` `r` (changes) | Revert the selected hunk |
| `g` (changes) | Refresh |

Tabs, hunks, transcript items and sub-agents are also clickable.

## Slash commands

Type `/` to see them all with descriptions. The most used:

| Command | Purpose |
| --- | --- |
| `/model`, `/models`, `/provider`, `/effort` | Choose model, provider and reasoning effort |
| `/theme` | Theme picker with live preview |
| `/output-style` | How replies are written: Default, Explanatory (★ Insight notes), Learning (`TODO(human)`), or custom `output-styles/*.md` |
| `/permissions` | List rules; `allow`/`ask`/`deny`/`remove <rule>` to edit them; `mode` for the mode picker |
| `/context`, `/cost`, `/usage`, `/stats` | Context window grid, spend and token use |
| `/compact [focus]` | Replace the conversation with a model-written summary to free context (`--preview` shows what is kept) |
| `/rewind`, `/undo`, `/redo` | Step back through code and conversation |
| `/new`, `/resume`, `/sessions`, `/fork`, `/rename` | Manage sessions (`/resume <id>` jumps straight in) |
| `/plan`, `/review`, `/fix`, `/test` | Switch agent profile |
| `/verify` | Read-only review of the uncommitted diff |
| `/second-opinion` | The same review by a model from a different family |
| `/security-review` | Security review of the pending change (or the branch against its base) |
| `/pr-comments [n]` | Fetch the PR's review comments with `gh` and summarize them by file |
| `/agents`, `/skills`, `/plugins`, `/mcp`, `/reload` | Extensions |
| `/hooks` | List hooks; `/hooks add <event> [--matcher <tool>] <command>`, `/hooks remove <event> <n>` |
| `/memory` | Your notes and the project's; `/memory auto on\|off`, `/memory forget <id>` |
| `/init` | Create project instructions |
| `/todos` | The agent's current task list |
| `/release-notes` | What's new in this version |
| `/image`, `/web`, `/run` | Attach an image, fetch a page, run a command into context |
| `/diff`, `/commit` | Review and commit changes |
| `/bashes` | Background shells the agent started (`/bashes kill <id>`) |
| `/setup` | Starter-pack picker (or `/setup <id>…` to install directly) |
| `/doctor`, `/login`, `/logout`, `/auth` | Diagnostics and sign-in |

## Command line

```bash
dcode-ai                          # interactive TUI
dcode-ai -p "…"                   # one-shot prompt
dcode-ai -c                       # continue the most recent session
dcode-ai -r [id]                  # resume the last session, or one by id (or its last digits)
dcode-ai --plain                  # plain output for screen readers
dcode-ai --model opus             # pick a model for this run
dcode-ai -s                       # read-only safe mode
dcode-ai -p "…" --append-system-prompt "Answer in French"
dcode-ai -p "…" --allowed-tools "Read,Edit,Bash(git:*)" --disallowed-tools "WebFetch"
dcode-ai -p "…" --max-turns 20

dcode-ai spawn --prompt "…"       # detached session
dcode-ai attach <id>              # follow its live output
dcode-ai status                   # running sessions
dcode-ai sessions [--all]         # saved sessions (--all includes sub-agents)
dcode-ai logs <id>                # replay the event log
dcode-ai cancel <id>              # stop a running session

dcode-ai login anthropic|copilot  # sign in
dcode-ai models                   # models on the active provider
dcode-ai doctor                   # diagnose setup
dcode-ai setup                    # starter packs for your role and languages
dcode-ai policy show|test         # permission rules
dcode-ai worktrees list|prune|merge <id>
dcode-ai web                      # local browser chat for a live session
dcode-ai completion zsh           # shell completions
```

## Sessions

Every conversation is saved per workspace:

```text
<workspace>/.dcode-ai/sessions/
├── <session-id>.json             # state
└── <session-id>.events.jsonl     # append-only event log
<workspace>/.dcode-ai/worktrees/
└── <session-id>/                 # isolated git worktree (sub-agents)
```

Sessions untouched for 30 days are deleted at startup
(`[session] cleanup_period_days`, `0` keeps them); `dcode-ai sessions --prune
<DAYS>` does it on demand. `/resume` (or `dcode-ai -r`) picks one to continue,
`dcode-ai -c` continues the latest.

A session records its model, provider, token counts, estimated cost,
timestamps, parent/child lineage and orchestration metadata. Resuming keeps
the provider and model it ran on.

## Starter packs

Optional. `dcode-ai setup` asks what you work on (Backend, Frontend / UI-UX,
DevOps, Automation, Data / ML, Security) and which languages (the ones found
in the repo are preselected), then installs the plugins that fit from the
official marketplace. Language tools such as `gopls` are never installed for
you; missing ones are listed with their install command.

```bash
dcode-ai setup                    # wizard
dcode-ai setup backend rust       # install packs directly
dcode-ai setup --list             # packs and what each adds (● = installed)
dcode-ai setup --detect           # languages found in this repo
dcode-ai setup --remove frontend  # undo a pack (keeps plugins another pack uses)
dcode-ai setup --save team.toml   # share your packs; teammates run --from team.toml
dcode-ai setup --update           # fetch the latest packs from GitHub
```

In a session, `/setup` opens a checkbox picker (Space to tick, Enter to
install); `/setup <id>…` installs directly. Re-running is safe: installed
plugins are skipped.

Packs come from [`packs/packs.toml`](../packs/packs.toml) in this repository.
The binary has a copy; `dcode-ai setup --update` (and `setup` itself, weekly)
fetches the latest, so packs improve without a new release.

Make your own packs in `~/.dcode-ai/packs/*.toml` or
`<repo>/.dcode-ai/packs/*.toml` (an id that exists replaces the built-in):

```toml
[[pack]]
id = "acme"
name = "Acme backend"
kind = "role"                      # or "language" (with detect = ["go.mod", "*.go"])
description = "Our services"
plugins = ["code-review@claude-plugins-official"]

[[pack.mcp]]                       # asked about one by one (--yes accepts)
name = "acme-docs"
url = "https://docs.acme.dev/mcp"

[[pack.tools]]                     # checked on PATH, never installed
command = "acme"
install = "brew install acme/tap/acme"
```

MCP servers go to `.mcp.json` (`--scope user` for all projects). What each
pack added is recorded in `~/.dcode-ai/setup.json`.

## Extra working folders

By default dcode-ai reads and edits only inside the folder you start it in.
`/add-dir <path>` (or `--add-dir <path>`, repeatable) lets it work in another
folder too for this session — e.g. a sibling repo or a shared library. Keep
folders permanently with `[permissions] additional_directories = ["~/code/lib"]`.
The shell sandbox allows writes there as well.

## Sub-agents

The agent delegates with `spawn_subagent`. Each child:

- gets its own session id, linked to the parent in metadata;
- starts from a summary of the parent conversation;
- runs in an isolated worktree on branch `dcode-ai/<session-id>` — its files,
  shell and sandbox are all rooted there, never in your checkout;
- follows your permission mode: default, accept edits and auto become auto
  (it cannot ask you, so a risky call is refused and reported back); plan,
  don't-ask and bypass carry over;
- returns its result to the parent (merge with `dcode-ai worktrees merge <id>`).

Their live activity shows in place in the transcript and in the dashboard's
agents tab. Define specialised agents in `agents/*.md` — see
[custom agents](extensibility.md#custom-agents).

## Permissions

| Mode | Behaviour |
| --- | --- |
| `plan` | Read and research only — no writes or shell |
| `default` | Reads run; edits and unknown commands ask |
| `accept-edits` | Edits run; commands still ask |
| `auto` | Reads, edits and ordinary dev commands run; other commands get a quick model check (safe → runs, otherwise asks); risky actions (deleting, `git push`, `reset --hard`, publishing, deploying, `sudo`, cloud CLIs) always ask |
| `dont-ask` | Read-only, no prompts (headless) |
| `bypass-permissions` | Everything runs without prompts (destructive commands are still refused) — trusted environments only |

Shift+Tab cycles default → accept edits → plan → auto; the footer shows the
mode (`⏵⏵ auto mode on`). The startup chooser and `--permission-mode auto`
also pick it.

The startup chooser (Manual / Accept edits / Auto) appears once per folder;
the answer is remembered in `~/.dcode-ai/folders.json`. Pick again with
`/permissions mode`.

Rules: `/permissions` lists every allow, ask and deny rule with where it comes
from. `/permissions allow Bash(cargo test:*)` adds one for this session and
saves it to `.dcode-ai/config.local.toml` (`--user` saves it to
`~/.dcode-ai/config.toml`); `/permissions remove <rule>` deletes it.

Shell commands are parsed (quoting, pipelines) rather than matched as text:
read-only commands (`ls`, `cat`, `rg`, `git status`, …) run in every mode except
plan; destructive shapes (`sudo`, `rm -rf /`, `mkfs`, `curl | sh`, fork bombs)
are refused.

### Policy files

`~/.dcode-ai/policy.toml` and `.dcode-ai/policy.toml` add rules. `deny` wins,
then `ask`, then `allow`, then the mode's default:

```toml
deny  = ["execute_bash:git push --force*", "delete_path:*"]
ask   = ["execute_bash:git push*"]
allow = ["execute_bash:cargo test*"]
```

```bash
dcode-ai policy show                           # every rule and its source
dcode-ai policy test execute_bash "git push"   # ask: … rule `execute_bash:git push*`
```

### Sandbox

```toml
[permissions]
sandbox_bash = true
sandbox_writable_roots = ["/tmp/build-cache"]
```

Shell commands may read everywhere but write only inside the workspace, temp
directories and the listed roots — Landlock on Linux, Seatbelt (`sandbox-exec`)
on macOS.

### Second opinion

`/second-opinion [focus]` has a model from a **different family** review your
uncommitted changes: a Claude session gets GPT or Gemini (through Copilot), a
GPT session gets Claude. The reviewer runs the project's checks read-only;
anything it finds goes back to your agent, which must fix each point or say
why it is not a problem. Pick the reviewer with `[session]
second_opinion_model = "copilot:gpt-5"`; `[session] cross_review = true` does
this after every turn that changed files.

### Verifier

`/verify` runs a read-only reviewer over the uncommitted changes, including the
project's checks, and hands real problems back to the main agent.
`[session] verify_after_edits = true` does it after every turn that changed files.

## Tools

| Group | Tools |
| --- | --- |
| Files | `read_file`, `list_directory`, `write_file`, `edit_file`, `apply_patch`, `replace_match`, `create_directory`, `rename_path`, `move_path`, `copy_path`, `delete_path` |
| Search | `search_code` (ripgrep), `code_intel` (language server), `query_symbols`, `repo_map` |
| Shell | `execute_bash` (PTY, optional sandbox), `run_background` (see below), `run_validation` |
| Git | `git_status`, `git_diff` |
| Web | `web_search`, `fetch_url` |
| Binaries | `inspect_binary` — ELF / PE / Mach-O summary, sections, symbols, imports, strings, cargo-auditable crates, Go build info; never runs the file |
| Agent | `spawn_subagent`, `ask_question`, `update_plan`, `invoke_skill` |
| MCP | `mcp__<server>__<tool>` from configured servers; with many, `tool_search` loads them on demand |

### Background shells

The agent runs long commands (dev servers, test suites, builds) with
`run_background` and keeps working. While they run the footer shows
`· 1 shell`, and a turn ends with `✻ Crunched for 1m 9s · 1 shell still
running`. When a command finishes you see
`● Background command "cargo test" completed (exit code 0)` and the agent is
told automatically — exit code and last output lines — so it can react
without polling. A shell command already running in the foreground can be
sent to the background with `Ctrl+B`. `/bashes` lists the jobs with their latest output;
`/bashes kill <id>` stops one.

## Automation

### Output formats

```bash
dcode-ai -p "summarise README.md" --json            # one JSON result
dcode-ai -p "fix the build" --stream ndjson | jq .  # live event stream
```

`--stream` takes `off`, `human` (default) or `ndjson`.

### IPC

Detached sessions listen on a Unix socket (`$XDG_RUNTIME_DIR/dcode-ai/<id>.sock`,
or `/tmp/dcode-ai/`) — loopback TCP on Windows. Envelopes and commands are in
[IPC & NDJSON](ipc-ndjson.md).

### Orchestration metadata

```bash
DCODE_AI_ORCH_NAME=github-actions DCODE_AI_ORCH_RUN_ID=run-123 DCODE_AI_ORCH_TASK_ID=task-456 \
  dcode-ai -p "…"
```

### Exit codes

| Code | Meaning |
| --- | --- |
| `0` | Success |
| `1` | Internal failure |
| `10` | Configuration failure |
| `11` | Runtime, provider or tool failure |
| `13` | Headless run blocked on an approval |
| `130` | Cancelled |

`dcode-ai ci` uses its own codes — see [CI](extensibility.md#ci).

## Instructions and memory

Instructions are merged in this order:

1. built-in system prompt
2. `AGENTS.md` in the workspace root
3. `.dcode-airc` (project, path configurable)
4. `.dcode-ai/instructions.md` (personal, gitignored)
5. available skills
6. orchestration context from `DCODE_AI_ORCH_*`

Memory has two scopes: notes about you in `~/.dcode-ai/memory.json` (recalled
in every project) and notes about the project in `.dcode-ai/memory.json`. Add
them with `# note` in the prompt, `/memory <text>` or `dcode-ai memory add "…"`.

**Auto memory** (on by default, `[memory] auto = false` or `/memory auto off`
to stop): the agent saves corrections, your preferences and non-obvious
project facts on its own, one short sentence each, never secrets. `/memory`
shows both scopes; `/memory forget <id>` removes a note.
