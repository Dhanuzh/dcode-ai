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
| `Enter` / `Shift+Enter` | Send / new line (`\` then Enter or Ctrl+J also add a line; `/terminal-setup` makes Shift+Enter work in VS Code, Cursor, Windsurf, Windows Terminal and Alacritty) |
| `Shift+Tab` | Cycle mode: default → ⏵⏵ accept edits → ⏸ plan |
| `Esc` | Interrupt the running turn (while a shell command runs: stop just that command; Esc again stops the turn) |
| `Esc` `Esc` | Rewind: pick a message, then restore code and conversation, conversation only, or code only |
| `1` / `2` / `3` | Answer a permission prompt: yes · yes, don't ask again · no |
| `h` (edit prompt) | Choose which hunks of an edit to apply (Space toggles, Enter applies the chosen ones; none chosen = no) |
| `Tab` | Complete `@file` or `/command`; otherwise cycle the agent profile |
| `@` | Fuzzy file and folder search (`@gitint`, `@src/tui/`); `.gitignore` respected |
| `!` | Shell mode: the line runs as a shell command (pink `!` prompt) |
| `F2` / `Shift+F2` | Next / previous model |
| `Ctrl+P` | Command palette |
| `/vim` | Vim keys in the prompt: Esc → NORMAL (`h l w b 0 $ x dd dw cw cc D C u p`), `i a A I o O` → INSERT |
| `Ctrl+B` | Move the running shell command to the background |
| `Ctrl+T` | Show or hide thinking |
| `Ctrl+O` | Fold or unfold tool output |
| `Ctrl+F` | Find in the conversation: type to search, Enter/↓ next match, ↑ previous, Esc close |
| `Ctrl+R` | Search prompt history |
| `Enter` while the agent works | Send now: the agent reads the message at its next step (after the current tool calls) and answers it in the same turn |
| `Tab` / `Alt+Enter` while the agent works | Queue the message to send after the turn (commands like `/…` and `!…` always queue) |
| `↑` on an empty prompt | Take unsent messages back to edit; `Esc` also puts them back |

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
right. Drag over the transcript to select text (it is copied when you let
go), and drag a rail's inner edge to make it wider or narrower.

| Key | Action |
| --- | --- |
| `Alt+1` … `Alt+7` | Context · changes · agents · files · office · team · scratch tab (team: the shared to-dos and the people's chat; files: the project explorer, what the agent read or edited marked; scratch: the agent's working files) |
| `Ctrl+D` | Focus the tab that is showing (again to return to the prompt) |
| `←` / `→` (rail focused) | Previous / next tab |
| `j` / `k` (changes) | Select a hunk |
| `r` `r` (changes) | Revert the selected hunk |
| `g` (changes) | Refresh |
| `Enter` / `Space` (files) | Open or close a folder; `Enter` on a file adds `@file` to your message |
| `↑` / `↓`, `Enter`, `o` (scratch) | Pick a file (newest first, previewed below), add `@file`, open it in your viewer |
| `Ctrl+V` (team chat box) | Send the screenshot on your clipboard (text is pasted into the box) |

Tabs, hunks, transcript items and sub-agents are also clickable.

## Slash commands

Type `/` to see them all with descriptions. The most used:

| Command | Purpose |
| --- | --- |
| `/model`, `/models`, `/provider`, `/effort` | Choose model, provider and reasoning effort |
| `/theme` | Theme picker with live preview |
| `/output-style` | How replies are written: Default, Explanatory (★ Insight notes), Learning (`TODO(human)`), or custom `output-styles/*.md` |
| `/permissions` | List rules; `allow`/`ask`/`deny`/`remove <rule>` to edit them; `mode` for the mode picker |
| `/context`, `/cost`, `/usage`, `/stats` | `/context`: what fills the window (system prompt, skills, tools, MCP tools, summary, conversation, and what the provider counted beyond that); `/usage`: fresh input, cache reads / writes, output, cache hit rate, Claude process restarts and why (history rewritten, prompt or tools changed, process ended), and requests that missed the cache; `/stats`: tokens per day and per project over the last 14 days. Dollar estimates show only on the API backend: on a Claude plan or Copilot they would mislead |
| `/compact [focus]` | Replace the conversation with a model-written summary to free context (`--preview` shows what is kept). It also happens on its own near the end of the window — see *Context window and compaction* |
| `/rewind`, `/undo`, `/redo` | Step back through code and conversation (in git workspaces also files changed by shell commands) |
| `/diff` | Full-screen diff of every changed file (↑↓ file · Tab · `n`/`p` hunk · `r r` revert hunk · `q` close); `/diff <args>` prints a git diff |
| `/new`, `/resume`, `/sessions`, `/fork`, `/rename` | Manage sessions (`/resume <id>` jumps straight in; the picker searches titles and what was said; sessions name themselves after the first turn, bare `/rename` picks a new name) |
| `/export [file\|copy]`, `/share` | The conversation as Markdown with secrets masked: to a file, the clipboard, or a secret GitHub gist (`gh`) |
| `/browser` | The agent's web browser: `open [url]`, `connect <port>`, `close` (see *Browser control*) |
| `/project-config` | Shared `.dcode.toml`: `model` / `mode` save the current one for the team |
| `/plan`, `/review`, `/fix`, `/test` | Switch agent profile |
| `/verify` | Read-only review of the uncommitted diff |
| `/second-opinion` | The same review by a model from a different family |
| `/security-review` | Security review of the pending change (or the branch against its base) |
| `/pr-comments [n]` | Fetch the PR's review comments with `gh` and summarize them by file |
| `/agents`, `/skills`, `/plugins`, `/mcp`, `/reload` | Extensions |
| `/hooks` | List hooks; `/hooks add <event> [--matcher <tool>] <command>`, `/hooks remove <event> <n>` |
| `/memory` | Your notes and the project's; `/memory auto on\|off`, `/memory forget <id>`, `/memory tidy` (drop duplicates) |
| `/btw <question>` | Quick side question about the work, answered in a panel (also mid-turn) and never added to the conversation |
| `/terminal-setup` | Make Shift+Enter insert a newline in this terminal |
| `/init` | Create project instructions |
| `/todos` | The agent's current task list |
| `/release-notes` | What's new in this version |
| `/image`, `/web`, `/run` | Attach an image, fetch a page, run a command into context |
| `/diff`, `/commit` | Review and commit changes |
| `/bashes` | Background shells the agent started (`/bashes kill <id>`) |
| `/goal` | Keep working across turns until an objective is done (`/goal <objective> [--budget N]`, `pause`, `resume`, `clear`) |
| `/update` | Install the latest release from inside the session (`--check` to only look); asks for your sudo password in a masked popup when the binary is in a system folder |
| `/setup` | Starter-pack picker (or `/setup <id>…` to install directly) |
| `/doctor`, `/login`, `/logout`, `/auth` | Check sign-in, git, ripgrep, config, MCP and disk, and fix what can be fixed; sign-in |

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
dcode-ai attach <id> --ssh me@server --dir ~/app   # …of a session on another machine
dcode-ai remote me@server --dir ~/app --keep       # use dcode-ai on a server from here (tmux keeps it running)
dcode-ai session export <id> [-o f]  # one file to hand a teammate, secrets masked
dcode-ai session import <file>       # add it here; /resume finds it (with its handoff note)
dcode-ai join <id> [--name you]      # another terminal on a running session: watch it live and send messages
dcode-ai relay serve [--port 8790]   # a relay so people on other networks can join (/invite relay host:port)
eval "$(dcode-ai hook zsh)"          # in ~/.zshrc: "Alt+F to fix" after a failed command (also bash, fish, powershell)
                                     #   sends only the command (secrets masked), exit code and folder; shows it and asks first
dcode-ai --ask "message"             # start a session and send this first
dcode-ai sessions --tidy             # shrink every session log (streaming pieces, huge old tool results)
dcode-ai status                   # running sessions
dcode-ai sessions [--all]         # saved sessions (--all includes sub-agents)
dcode-ai logs <id>                # replay the event log
dcode-ai cancel <id>              # stop a running session

dcode-ai login anthropic|copilot  # sign in
dcode-ai models                   # models on the active provider
dcode-ai doctor                   # diagnose setup
dcode-ai update [--check]         # install the latest release over this binary
                                  # (new releases also install in the background; [ui] auto_update = false or DCODE_AI_AUTO_UPDATE=0 turns that off)
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

## Goals

`/goal <objective>` keeps dcode-ai working across turns until the objective
is done — for long jobs like "make the test suite pass" or "port module X":

```text
/goal make cargo test pass --budget 400000
/goal            # status: turns, tokens used
/goal pause | resume | clear
```

After each turn dcode-ai sends the agent a continuation message with the
objective and the budget used. The agent ends the goal itself with the
`update_goal` tool once it has *verified* the objective (or marks it blocked
when the same blocker stops it three turns running). A goal also stops at
its token budget, after 50 automatic turns, or when you press Esc. The
footer shows `◎ goal · turn N` while it runs. The goal is saved with the
session: after `/resume` it comes back paused, and `/goal resume` carries on.

## Task board

`/board` is a kanban for the project, like a small Jira: cards with keys
(`DCO-12`, from the project's name), priorities, blockers, milestones and
review gates, in **Backlog · To do · In progress · Review · Done**. It's
stored in `.dcode-ai/board/board.json` (git-ignored by its own
`.gitignore`; delete that file to share the board through git), and every
session of the project, sub-agents included, sees the same board.

- **Plan with the agent:** `/board plan add Stripe checkout` — it lays out
  milestones and cards (`board_add`), with what blocks what and a review
  gate on cards you should check, without starting the work.
- **The head agent:** `/board run [n]` — it takes the ready cards (in To do,
  every blocker done), starts up to `n` (default 3) sub-agents in their own
  worktrees in parallel, moves each card to In progress and then Review with
  a note and its branch, and continues with what became unblocked. It stops
  and tells you what waits for you: gated cards in Review, branches to
  merge, blocked cards. A card with a gate can't be moved to Done by the
  agent.
- **By hand**, in the board: `←/→/↑/↓` (or `h j k l`) move, `Shift+←/→` or
  `<` `>` move the selected card, `n` new card in this column, `e` rename,
  `p` priority, `b` blocked by (keys or numbers), `g` review gate on/off,
  `x` twice deletes, `Enter` shows the card with its notes, `r` runs the
  board, `q` closes. Click a card to select it. The agent's changes appear
  live.
- The agent's tools: `board_view`, `board_add`, `board_update` (they write
  only the board file, so they never ask). The agent's cards are personal
  unless you ask for Jira tickets.

### Personal and Jira

The board has two spaces, kept apart: **Personal** (your own tasks, this
computer only) and **Jira** (a Jira project's tickets). `t` (or Tab)
switches; `n` adds to the space you're in; `/board add jira <title>` makes a
ticket from the prompt.

Linking Jira, once per project:

```bash
dcode-ai mcp add atlassian --transport http https://mcp.atlassian.com/v1/mcp
dcode-ai mcp login atlassian          # signs in with your Atlassian account
```

then in a session `/board jira SHOP` (or `/board jira SHOP acme` to pick
the site `acme.atlassian.net`). dcode-ai calls the server's tools itself
(no model, no tokens):

- the project's open tickets and those changed in the last two weeks come
  in, with status, priority, assignee and description (Jira's status
  categories decide the column; a status named like "review" or "QA" goes
  to Review);
- a new card in the Jira space becomes a ticket (type Task, or the
  project's first type) and takes its key (`SHOP-42`). Fields the project
  requires (a "Task Type", planned dates, an estimate) are filled in: what
  you set with `/board jira field <name> = <value>`, else what your latest
  ticket of that type has, dates from today; the card's notes say what was
  used. With an assignee filter on your account the ticket is yours;
- moving a card moves its ticket (the workflow transition that lands in
  that column);
- renaming a card or changing its description updates the ticket;
- removing a Jira card only takes it off the board; the ticket stays.

It syncs every minute and right away after a change; `s` in the board or
`/board sync` syncs now, and the header shows when it last did (or what
went wrong). `/board jira` shows the link, `/board jira off` unlinks.

`/board jira SHOP me` links and filters in one go. `/board jira assignee <name or email>` keeps only that person's tickets
(`me` is whoever is signed in to the MCP server; `all` goes back to the
whole project). Changing it clears the tickets pulled before and syncs
again; cards still waiting to go to Jira stay.

With the mouse: click Personal / Jira to switch, click a card to select it
and again to see it in full, drag a card onto another column to move it,
scroll the column under the pointer.

## Scratch folder

Each session has a folder for what the agent makes along the way:
`.dcode-ai/scratch/<session-id>/` (git-ignored). The agent is told to put
test and command logs, screenshots, API responses and generated data there.
The dashboard's **scratch** tab (`Alt+7`) lists them newest first, live,
with the selected one previewed: the end of a log (errors in red), JSON
pretty-printed, a picture as a colour thumbnail. `Enter` adds `@file`, `o`
opens it in your viewer. Sub-agents' files show under `@<id>/`.

## Extra working folders

By default dcode-ai reads and edits only inside the folder you start it in.
`/add-dir <path>` (or `--add-dir <path>`, repeatable) lets it work in another
folder too for this session — e.g. a sibling repo or a shared library. Keep
folders permanently with `[permissions] additional_directories = ["~/code/lib"]`.
The shell sandbox allows writes there as well.

## Sub-agents

The agent delegates with `spawn_subagent`. For lookups it can use
`explore: true`: a read-only child (plan mode, no worktree) on the cheap
utility model (`[model] compact_model`, else Haiku on Claude). Each child:

- gets its own session id, linked to the parent in metadata;
- starts from a summary of the parent conversation;
- runs in an isolated worktree on branch `dcode-ai/<session-id>` — its files,
  shell and sandbox are all rooted there, never in your checkout;
- follows your permission mode: default, accept edits and auto become auto,
  and anything risky asks **you**, in the main session; plan, don't-ask and
  bypass carry over;
- in the TUI runs **in the background** by default: the main agent gets the
  agent's id at once, keeps working, and receives a `<task-notification>`
  with the agent's report when it finishes (`agent_output` shows progress,
  `agent_kill` stops it). One-shot runs (`-p`, CI, editors) wait for it;
- leaves no worktree behind when it changed nothing; otherwise its work is
  committed on its branch and, once the turn ends, you're asked: **merge**
  into your current branch, **keep** the branch for later (`dcode-ai
  worktrees merge <id>`), or **discard** it (`d` shows the diff). A merge
  that conflicts is aborted and the branch kept;
- cannot start sub-agents of its own.

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
the answer is remembered in `~/.dcode-ai/folders.json` and applied quietly
after that. Pick again with
`/permissions mode`, clear it with `/permissions forget`, and list remembered
folders with `/permissions folders`.

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
| Notebooks | `notebook_edit` (replace / insert / delete a cell); `read_file` also reads PDFs page by page and `.ipynb` as cells |
| MCP | `mcp__<server>__<tool>` from configured servers; with many, `tool_search` loads them on demand |

### sudo

In the TUI, a command that runs `sudo` — yours (`! sudo …`) or the agent's
(which asks your permission first, since `sudo` is a risky action) — asks for
your password in a masked popup instead of writing a prompt onto the screen.
dcode-ai starts those commands without a terminal and points `SUDO_ASKPASS`
at its own helper, which reaches the session over a private socket. The
password goes only to sudo; it is never shown, logged, stored or sent to the
model. If sudo already has a cached login, nothing is asked.

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

`AGENTS.md`, `CLAUDE.md` and `CLAUDE.local.md` in **subfolders** load the
first time the agent reads or edits something under them, and apply to work
in that folder (like Claude Code's nested `CLAUDE.md`).

Memory has two scopes: notes about you in `~/.dcode-ai/memory.json` (recalled
in every project) and notes about the project in `.dcode-ai/memory.json`. Add
them with `# note` in the prompt, `/memory <text>` or `dcode-ai memory add "…"`.

**Auto memory** (on by default, `[memory] auto = false` or `/memory auto off`
to stop): the agent saves corrections, your preferences and non-obvious
project facts on its own, one short sentence each, never secrets. `/memory`
shows both scopes; `/memory forget <id>` removes a note; `/memory tidy`
finds empty notes and exact or near duplicates (keeping the newest, fuller
copy) and asks before removing them.

## Browser control

The `browser` tool lets the agent use a real Chrome / Edge window: navigate,
read the page (a text outline with numbered elements), click, type, press
keys, scroll, go back, take a screenshot (`.dcode-ai/screenshots/`), read the
console and run JavaScript, work with tabs (`tabs`, `switch`, `new_tab`)
and `wait_for` text to appear or disappear. Every page-changing action is
recorded: `/browser save [name]` writes the steps to
`.dcode-ai/browser-steps/<name>.json` and the agent can `replay` them (each
element is found again by what it is, not its number). Screenshots are shown
to models that can see images (API backend, Copilot). When the tab it drives is closed or blank and you are on
another tab, it follows yours.

`find {query}` returns only the elements and text that mention the query,
which is much smaller than a snapshot. To keep requests small, a click, key press, scroll, `wait_for` or submitted
form on the same page returns only what changed (new elements, new text);
the agent asks for a full `snapshot` when it needs one. An element keeps its
number until the page is left. Rewinding code and conversation (`Esc Esc`)
also takes the browser back to the page that was open then; anything
already sent to a site stays sent.

Your everyday Chrome profile can't be driven: since Chrome 136 Chrome
ignores remote debugging on the default profile (protection against cookie
theft). Sign in once in dcode-ai's browser instead — the logins stay.

- The first use launches dcode-ai's own browser (separate profile in
  `~/.dcode-ai/browser-profile`, or `%LOCALAPPDATA%\dcode-ai` for the Windows
  browser from WSL) on debug port 9333. Sign in to sites there yourself;
  the logins stay for later sessions.
- To use your own browser profile, start Chrome with
  `--remote-debugging-port=9222` and run `/browser connect 9222`.
- `DCODE_AI_BROWSER` picks the executable. On WSL this needs
  `networkingMode=mirrored` (the Windows browser's port must be on
  localhost).
- Permissions: reading the page never asks; navigating, clicking and typing
  ask in default mode and run in accept-edits / auto mode; `eval` always
  asks. Password fields are never typed into.

## Seeing and sharing a session

| Command | What it does |
| --- | --- |
| `/office` | The Agent Office: every agent as an animated pixel-art character at a desk, acting out what it does (typing, reading, running commands, hand up for your approval), with a speech bubble; ←/→ picks one to see its activity. The people on the session are there too, under their names: you at the first desk, whoever joined (`dcode-ai join`) at a guest desk, whoever has the `/watch` page open standing with a mug; what someone says shows in a bubble. Also the dashboard's `office` tab (`Alt+5`), with who is here listed under the room |
| `/timeline` | The session as steps (your messages, tool calls, replies, compactions) with a scrubber bar; ←/→ to move, Enter on one of your messages rewinds there |
| `/files` | Files the agent read and edited this session (also the dashboard's `files` tab, `Alt+4`) |
| `/watch` | A live, read-only web page of this session (link copied). This machine only; `/watch lan [port]` shares it on your network (a fixed port such as `/watch lan 8787` lets you allow it in the firewall once; on WSL it prints the commands); `/watch stop` ends it. Each link has its own token, nothing goes through an outside service |
| `/invite` | Let someone on another computer join with one line: prints (and copies) `dcode-ai join <ip>:8788/<key> --name <their name>`. You're asked before each person gets in. `/invite web` also gives a browser link (no install; plain HTTP), `/invite <port>` another port, `/invite --let-approve` lets guests approve and stop too, `/invite stop` ends it |
| `/call` | A voice call with the people on this session (guests on `/invite` and `dcode-ai join` terminals type `/call` to join). In a call: `/call ptt` (hold Ctrl+Space to talk), `/call volume <name> <0-200>`, `/call echo on\|off`. `/mute` toggles your microphone, `/hangup` leaves. Never recorded or logged; headphones avoid echo. Needs PulseAudio/PipeWire on Linux (WSLg on Windows 11) |
| `/invite lan` | An invite for this network (and Tailscale) only, without the peer-to-peer link |
| `/invite relay <host:port>` | Invite through a relay you run (`dcode-ai relay serve` on a machine both sides can reach): people join from any network, no VPN. The relay only passes encrypted bytes |
| `/chat` | The chat window: the dashboard's team tab, with a box to type in (Enter sends, Esc back). `/chat <text>` sends a line directly |
| `/chat <text>` | Talk to the people on this session — not sent to the agent. `/chat` alone shows the recent chat; it's also in the dashboard's `team` tab (`Alt+6`) |
| `/board` | The project's task board, full screen (kanban: Backlog · To do · In progress · Review · Done). `/board add <title>`, `/board list`, `/board plan <goal>` (the agent lays out milestones and cards with blockers and review gates), `/board run [n]` (the head agent works the ready cards with up to `n` sub-agents in parallel, each in its own worktree; gated cards wait in Review for you). Kept in `.dcode-ai/board/board.json`, shared by the project's sessions |
| `/chat image <file>`, `/chat open [n]` | Send a picture to the people here (shrunk to 1280 px); open the n-th picture of the chat (the last by default) in your viewer |
| `/todo` | The shared to-do list: `/todo <text>` adds, `/todo done <n>` ticks off, `/todo` lists. Everyone on the session sees it, and the agent gets the open items with each prompt (not shown in the transcript) |
| `/people` | Who is with this session: you, guests on `/invite` (terminal or browser), `dcode-ai join` terminals, `/watch` viewers |
| `/kick <name>` | Remove an invited guest; their invite no longer lets them back in |
| `/handoff <note>` | Writes `<session>.dcode-session` (secrets masked) with your note, the open todo list and the changed files; the teammate imports it and says "continue" |
| `/sounds on\|off` | A bell when a turn of 10s+ finishes or dcode-ai needs an approval or an answer |
| `/speak` | Reads the last reply aloud (system voice; code blocks skipped) |

`dcode-ai join <id>` attaches a second terminal (or a teammate over SSH) to a
running session: it shows the session live; lines you type are queued as
messages; `/approve`, `/deny`, `/stop` and `/quit` work as you'd expect.
`--name` sets the name the others see (default: git `user.name`, else your
login). The session shows `⇢ Priya joined`, `⇢ Priya: <message>` and
`⇠ Priya left`, and the agent gets the message as `(from Priya) …`. Someone on
the `/watch` page can type a name at the top; while the page is open they
are in the office as watching. Names are display-only (letters, digits,
spaces and `-_.`, 24 characters at most); they are not a login.

### Joining from another computer: `/invite`

1. The host types `/invite`. It prints one line (and copies it):
   `dcode-ai join 192.168.1.9:8788/Qk3f… --name <your name>`.
2. The guest runs that line with their name. The host sees
   **"Priya (192.168.1.20) wants to join — let them in?"** and picks:
   - **to watch and chat**: they see everything live, chat with people
     and join calls; nothing they send reaches the agent;
   - **to drive**: they can also ask the agent to do things;
   - **No**.
   Letting someone drive while you're in auto mode switches you to default
   mode, so the agent asks you before acting on their requests.
3. The guest gets the same dcode-ai interface as the host, with the
   conversation so far, and types to send messages. `/office` and
   `/timeline` work (the host's desk says "(host)", theirs "(you)");
   commands that would change the host's session (`/model`, `/login`, …)
   say they aren't available; `/quit` leaves. `--plain` gives plain text
   instead. Guests don't need to be signed in.

- The connection is encrypted and authenticated with the invite's key
  (Noise `NNpsk0`); a wrong or old key can't even knock. Each `/invite`
  makes a new key; `/invite stop` ends it and disconnects guests.
- Drivers send messages only; approvals and stopping a turn stay with you
  unless you use `/invite --let-approve`.
- `/invite web` also prints a browser link on the same port, for people
  without dcode-ai: they type a name, press "Ask to join", and after your
  yes get a message box. It's plain HTTP, so others on the same network
  could read that page.
- WSL: allow the port once with the firewall commands `/invite` prints
  (the default port 8788 stays the same, so it's once).
- **Any network, nothing to set up**: the line also has a direct
  peer-to-peer id (`p2p-…`). `join` tries your network's addresses first,
  then dials that id: a direct, hole-punched QUIC link, or through iroh's
  public relays when the two networks can't be punched through. The link
  is authenticated with the host's key and the usual invite handshake runs
  inside it, so relays only ever pass encrypted bytes and never see the
  invite key. Each `/invite` gets a fresh id. `/invite lan` (or
  `DCODE_AI_P2P=0`) leaves it out.
- With Tailscale: run `dcode-ai setup tailscale` once on both
  computers (it asks before installing; you sign in through Tailscale).
  `/invite` then adds this machine's Tailscale address to the line, and
  `join` tries each address. Don't open the port on your router.
- If a guest's connection drops, they reconnect on their own (no knock;
  within 20 s nobody sees them leave). `/kick <name>` removes them for good.
- `/invite web` and `/watch lan` also print the link as a QR code.
- Guests use `/chat`, `/todo` and `/people` too; their header shows your
  project and name.
- Voice: `/call` (host, then each guest), `/mute`, `/hangup`. In a call:
  `/call` opens a menu (mute, push-to-talk, echo, volume per person, hang
  up); typed forms also work: `/call ptt` turns push-to-talk on (hold
  Ctrl+Space), `/call volume <name> <0-200>` sets someone's volume for you,
  `/call echo off` turns echo reduction off (with headphones).
- `dcode-ai join <id>` on the same machine gets the full interface and can
  join the call too (no knock; it uses the host's private local invite). Each
  person's voice goes to the host, who passes it to everyone else in the
  call; silence isn't sent. The browser page can't join calls (browsers
  allow the microphone only over HTTPS).

## Guards against runaway turns

- A turn pauses only when the same tool call repeats 4 times in a row (a
  loop). Send "continue" to go on. Long turns are never paused for their
  size.
- A tool output over 16,000 characters is shortened in the conversation
  (head and tail); the whole output is saved under `.dcode-ai/tool-output/`
  (the last 50) and the agent reads the part it needs.

## Context window and compaction

One window for everything: the model's (200k, or 1M for `…[1m]` models),
compared with the prompt size the provider reported for the last request
plus what was added since. The footer shows `N% until auto-compact` from
half full and `/context` shows where it happens.

- **Auto-compact point**: the window minus room for the reply (up to 32k)
  and for the summary request (7.5%, at least 13k), kept between 60% and
  90% — about 77% on a 200k window. `[memory.context] auto_compact_at = 70`
  sets it as a percentage instead.
- At that point old tool output is cleared first (file reads, pages and
  command output beyond the newest 40k tokens become a one-line stub; no
  model call). If that is enough, nothing else happens; it is done at most
  once between compactions, and also when the session was idle for five
  minutes (the provider's cache is gone then anyway).
- Otherwise the older history is summarized by the cheap model (Haiku, or
  `[model] compact_model`). What stays: the summary, framed as a hand-off;
  your own recent messages word for word (up to 20k tokens); the list of
  files changed; and the latest messages (15% of the window).
- A compaction that doesn't make the history at least 30% smaller is thrown
  away. After three of those in a row automatic compaction pauses until you
  run `/compact`.
- It runs before a turn and, at most once, in the middle of a long turn.
  Every rewrite of old history makes the provider read the whole prompt
  again without its cache (on the Claude Code backend it restarts the
  `claude` process), which is why it waits until it's needed.

## Look of the working line

```toml
[ui]
working_style = "bar"   # bar (default) · dots · plain (no animation)
shimmer = true          # sweep a highlight across the label
```

