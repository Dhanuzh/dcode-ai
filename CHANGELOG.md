# Changelog

## Unreleased

### Changed
- **Ctrl+C works like Claude Code**: it interrupts a running turn, else
  clears what you typed (↑ brings it back), else shows "Press Ctrl-C again
  to exit" in the footer; a second press within 2 s exits. Ctrl+D on an
  empty prompt does the same.

## 1.0.20 — 2026-10-26

### New
- **Questions look like Claude Code's**: when the agent asks you something,
  the question takes the prompt's place with a title chip, numbered options
  and a one-line description under each, the recommended one marked and
  selected. Press a number to pick, or just start typing to answer in your
  own words ("Type something."). Questions with several valid answers let
  you tick more than one (Space) and Submit. Esc still lets you answer in
  the prompt.
- **`/btw <question>`**: ask a quick side question about the work, even
  while the agent is busy. The answer appears in a panel above the prompt
  and is never added to the conversation.
- **Sub-agent merge prompt**: when a sub-agent finishes with changes on its
  own branch, you're asked to merge them into your branch, keep the branch
  for later, or discard it (`d` shows the diff). Its work is committed on
  its branch first, so nothing uncommitted is lost (this also fixes
  `dcode-ai worktrees merge`).
- **`/memory tidy`** removes empty notes and exact or near-duplicate
  memories, after showing you the list.
- **`/terminal-setup`** makes Shift+Enter insert a newline: it adds the
  binding to VS Code / Cursor / Windsurf (also from WSL), Windows Terminal or
  Alacritty. In kitty, Ghostty, foot and WezTerm Shift+Enter now works as is,
  and `\` then Enter adds a new line in any terminal.

### Fixes
- **Slow resume of long sessions**: tool results were saved whole in the
  session log (one search result was 23 MB) and replayed on every resume.
  They are now saved capped like the model sees them, old logs are trimmed
  while loading, and only the newest 200 messages are printed (`/transcript`
  has the rest). Syntax highlighting loads in the background at startup.
- **Repeating replies are stopped**: if the model starts outputting the same
  text over and over, dcode-ai stops the reply and keeps one copy, instead
  of burning tokens.

## 1.0.19 — 2026-10-26

### New
- **`/mcp` panel with sign-in**: `/mcp` lists servers with their status
  (connected · tools, needs sign-in, failed, disabled). Enter on a server:
  Authenticate, Reconnect, Sign out, Disable. Authenticate signs in right in
  the session, like Claude Code: the browser opens, the panel shows the URL
  (`c` copies it), and if the browser can't reach localhost after you
  approve (WSL), paste the address-bar URL into the panel. The server
  connects as soon as you're signed in, with no restart.
- **More plugins**: `/plugins` now also adds Anthropic's community
  (`claude-community`, ~2,280) and knowledge-work marketplaces next to the
  official one, about 2,700 plugins. Official ones are listed first.
- **`/copy link`** copies the newest link in the conversation whole (long
  wrapped URLs can't be mouse-copied cleanly).

### Fixes
- **After `/resume` (or `/project`), answers went nowhere**: approving a
  command, answering a question and Esc all still pointed at the previous
  session, so a resumed session waited five minutes on every approval and
  then reported it as denied, questions stayed on "Waiting input…", and Esc
  did not stop the turn. Approvals, questions, Esc, mid-turn messages and
  sub-agent launches now follow the session you resumed.
- A cancelled run no longer prints "run cancelled" twice.

## 1.0.18 — 2026-10-23

### New
- **Send a message while dcode-ai works**: press Enter during a turn and the
  agent reads it at its next step (after the current tool calls, or before
  it would stop) and answers in the same turn. Tab (or Alt+Enter) queues it
  for after the turn instead. Waiting messages are listed above the prompt
  (`· next step` / `· after this turn`); ↑ takes them back to edit, and Esc
  puts them back in the prompt.

### Changed
- **`/resume` picker** redrawn Claude-style in place of the prompt: full
  width, a search line, and two rows per session (title, then
  `3h ago · model · branch · tokens`). The current session and empty
  sessions are left out.

## 1.0.17 — 2026-10-23

### New
- **sudo in commands asks through a popup**: in the TUI, `! sudo …` and the
  agent's own sudo commands (after you approve them) ask for the password in
  the masked popup instead of printing sudo's prompt over the screen. Commands
  run without a terminal and with `SUDO_ASKPASS` pointing at dcode-ai's
  helper; the password reaches only sudo and is wiped after use. Outside the
  TUI nothing changes.

### Changed
- In the TUI, agent and `!` commands no longer inherit the terminal, so a
  tool that wants to prompt there (ssh, gpg) fails fast instead of drawing
  over the screen and hanging.

## 1.0.16 — 2026-10-23

### New
- **sudo password popup for `/update`**: when dcode-ai lives in a system
  folder (`/usr/local/bin`), the verified download is kept and a masked
  popup asks for your sudo password (three tries). The password goes straight
  to `sudo -S` on stdin, is never shown, logged, stored or sent to the model,
  and is wiped from memory after use. `dcode-ai update` in a terminal lets
  sudo ask there itself.

### Fixes
- An update that stopped half way no longer leaves `dcode-ai-update-*`
  folders in the temp directory.

## 1.0.15 — 2026-10-23

### Fixes
- The "new version available" hint never appeared in the TUI (the check only
  ran in `--plain`). It now shows once per version, as a small line right
  under the welcome box; a version found by the background check during the
  session is announced once in the transcript. Checks run every 6 hours
  instead of 20.

## 1.0.14 — 2026-10-22

### New
- **`/goal <objective> [--budget N]`**: dcode-ai keeps working across turns
  until the objective is done. The agent closes the goal with `update_goal`
  only after verifying it; a goal also stops at its token budget, after 50
  automatic turns, or on Esc. `/goal pause | resume | clear`; the footer
  shows `◎ goal · turn N`. (Design after Codex's goals.)
- **Background sub-agents**: in the TUI, sub-agents run in the background by
  default — the main agent gets an id at once and a `<task-notification>`
  with the report when the agent finishes. New `agent_output` (progress,
  optional wait) and `agent_kill` tools. A sub-agent's risky actions now ask
  you in the main session instead of being refused. (Design after Alter
  Zero's agent tools.)
- **Fallback models**: `[model] fallback_models = [...]` (up to three) are
  tried in order when the model is overloaded, rate-limited or unavailable;
  the transcript notes the switch.

### Fixes
- A sub-agent asking for a sub-agent of its own waited ten minutes for a
  reply that never came; sub-agents no longer get that tool.
- Approval prompts without tool details no longer show "0 fields".

## 1.0.13 — 2026-10-22

### New
- **`/update`**: install the latest release from inside a session; the
  welcome screen's "update available" line points to it. Downloads retry
  when a freshly published release isn't reachable yet.
- **Installer**: when `/usr/local/bin` needs sudo it offers `~/.local/bin`
  (the default), so `dcode-ai update` never needs sudo; it warns when that
  folder isn't on your PATH or an older copy would still be used.

### Changed
- Pasting an image no longer freezes the screen: the clipboard is read in the
  background and the footer shows "⧗ pasting image…".
- The empty prompt is just `>` (no "Try …" text).
- A remembered permission choice is applied quietly; the footer shows the
  mode.

## 1.0.12 — 2026-10-22

### Fixes
- **Claude backend loops**: when Claude wrote its native function-call markup
  (`<invoke name="…">`) instead of dcode-ai's `<tool_call>`, the calls were
  printed as text and the model repeated them for minutes. They now run;
  a call repeated word for word in one reply runs once and the third repeat
  ends the reply.
- **MCP**: a slow server no longer delays startup (dcode-ai waits at most 3 s;
  servers keep connecting in the background and join at the next turn) and
  is no longer dropped after 15 s. Servers connect in parallel. A server that
  fails says so in the transcript, with `dcode-ai mcp login <name>` for
  sign-in errors.

### New
- **`dcode-ai update`** (`--check`, `--force`): downloads the latest release
  for your platform, verifies its checksum and replaces the binary.
- Faster start: the Claude sign-in check is cached for 10 minutes (startup
  ~60 ms instead of ~400 ms); logging out clears it.
- Auto mode says when its model check approved a command.
- `--plain`: Tab opens a completion menu (commands with descriptions, skills,
  `@` files).
- `/permissions forget` and `/permissions folders` for remembered startup
  choices.
- `/setup` picker shows roles and languages side by side, no scrolling.
- Sub-agents: a worktree left unchanged is removed with its branch; one with
  changes is reported with the merge command.
- Releases: notes come from the CHANGELOG, and a Homebrew formula
  (`dcode-ai.rb`) is attached; `install.sh` no longer falls back to a
  deleted version.

## 1.0.11 — 2026-10-22

### Fixes
- The welcome box shows again when the folder's permission choice is
  remembered (the note used to hide it). The startup tip mentions auto mode.

## 1.0.10 — 2026-10-22

### Changed
- The startup permission question is asked once per folder: the answer
  (Manual, Accept edits or Auto) is remembered in `~/.dcode-ai/folders.json`
  and applied silently next time. A new folder still asks. Picking a mode in
  `/permissions mode` updates the remembered choice.

## 1.0.9 — 2026-10-21

### New
- **Smarter auto mode**: commands that are neither read-only nor ordinary dev
  tools get a quick model check (safe → runs, otherwise you are asked);
  risky actions still always ask.
- **`/security-review`**: a security reviewer over the pending change, or the
  branch against its base when nothing is uncommitted.
- **`/pr-comments [n]`**: fetches the pull request's reviews and inline
  comments with `gh` and summarizes what reviewers ask for.
- **`/permissions` rule editor**: lists every rule with its source;
  `allow|ask|deny|remove <rule>` edits them live and in the config
  (`--user` for all projects). The mode picker is `/permissions mode`.
- **`/hooks add|remove`**: edit hooks for all eleven events without
  opening a config file.
- **Auto memory**: the agent remembers corrections, preferences and
  non-obvious project facts by itself; notes about you are kept in
  `~/.dcode-ai/memory.json` and recalled in every project. `/memory auto
  on|off`, `/memory forget <id>`.
- **`/setup` picker**: tick roles and languages with Space, Enter installs.
  Packs live in `packs/packs.toml` and update with `dcode-ai setup --update`.

### Fixes
- Sub-agents now really work in their own worktree: their shell ran in your
  checkout before. They also follow your permission mode instead of
  bypassing it.
- Tool calls that write or run commands in one batch run in order (a read
  after a write sees it); a batch mixing a denied call and others no longer
  waits until cancelled.
- `read_file` says "File not found" instead of "outside the workspace".

### Faster
- `dcode-ai sessions`, `status`, `logs`, `plugin`, `mcp`, `setup` and other
  commands that don't call a model start in ~60 ms instead of ~500 ms.
- Fewer dependencies (removed genai, mcpr, globset, cli-prompts and an old
  crossterm); the binary no longer compiles shared modules twice.

## 1.0.8 — 2026-10-21

### New
- **Auto mode**, like Claude Code's: Shift+Tab cycles default → accept edits
  → plan → auto (`⏵⏵ auto mode on`). Reads, edits and ordinary commands run
  without asking; risky actions (deleting files, `git push`, `reset --hard`,
  publishing, deploying, `sudo`, cloud CLIs, `curl | sh`) still ask, and
  known-destructive commands are refused. Also in the startup chooser and as
  `--permission-mode auto`.
- **Starter packs** (`dcode-ai setup`, `/setup`): pick your roles (Backend,
  Frontend / UI-UX, DevOps, Automation, Data / ML, Security) and languages
  (detected from the repo) and dcode-ai installs the plugins that fit from
  the official marketplace, lists missing language tools with their install
  command, and asks before adding any MCP server. `--remove`, `--save
  team.toml` / `--from team.toml`, and your own packs in
  `~/.dcode-ai/packs/` or `.dcode-ai/packs/`. Entirely optional — never
  offered at startup.
- **`/second-opinion`**: a model from a different family (GPT/Gemini via
  Copilot for a Claude session, Claude otherwise) reviews your uncommitted
  changes; your agent fixes each finding or explains why it is not a
  problem. `[session] cross_review = true` runs it after every turn that
  changed files; `[session] second_opinion_model` picks the reviewer.
- **Editors (ACP)**: with an editor that offers them, files are read and
  written through the editor (unsaved changes included) and shell commands run
  in editor terminals shown in the tool call.
- **`/add-dir <path>`** (and `--add-dir`, `[permissions]
  additional_directories`): let dcode-ai read and edit another folder
  besides the workspace, like Claude Code's additional directories.
- **MCP OAuth sign-in**: `dcode-ai mcp login <name>` signs in to
  OAuth-protected remote MCP servers such as Atlassian (PKCE, dynamic client
  registration, automatic refresh); `mcp logout`; `mcp list` shows the
  sign-in state. Unset `${VAR}`s in MCP headers now fail with a clear error.
- **`/agents new <description>`** writes a sub-agent file for you (name,
  description, tools, instructions), validated before it is saved; `--user`
  saves it for every project.
- **Startup permission chooser**: Manual (ask before edits and commands),
  Accept edits, or Auto (every tool, this session only), replacing the yes/no
  "approve all tools?" popup.

## 1.0.7 — 2026-10-20

### New
- `/vim`: vim keys in the prompt like Claude Code: Esc → NORMAL (`h l w b
  0 $ x dd dw cw cc D C u p`), `i a A I o O` → INSERT; the footer shows
  `-- INSERT --` / `-- NORMAL --`. Saved in `[ui] vim_mode`.
- `/compact [focus]` now really compacts, like Claude Code: the model
  summarizes the whole conversation (giving most detail to what you ask it
  to focus on) and the session continues from that summary.

### Fixes
- Copilot: a Claude model id Copilot does not offer (e.g. `--model sonnet`,
  which names the Anthropic API's newest Sonnet) now uses Copilot's newest
  model of the same family instead of failing with `model_not_supported`.
- The startup "approve all tools?" popup: Esc now dismisses it, and typing
  dismisses it too, with your keystrokes going to the prompt (a first message
  typed quickly was swallowed).
- Requests without tools no longer send `"tools": null`, which GitHub Copilot
  rejected, so compaction summaries on Copilot fell back to a plain extract.
- Dashboard (Ctrl+\\): the mouse wheel scrolls the transcript and clicks on
  tabs, hunks and agents work in real terminals. The dashboard now turns on
  mouse capture (the inline view still leaves the mouse to the terminal).

## 1.0.6 — 2026-10-16

### Fixes
- Claude: protocol examples no longer break or cut a turn. `<tool_call>`
  inside a ``` code block, a placeholder like `<tool_call>{json}</tool_call>`,
  and a backtick-quoted tag split across stream chunks are all plain text.
- A tool call Claude writes but that cannot be parsed (or is never closed)
  is returned to it as a failed `invalid_tool_call` result so it can retry,
  instead of failing the turn.

## 1.0.5 — 2026-10-15

### New (Claude Code parity)
- **MCP tool search**: with more than 15 MCP tools (`[mcp]
  tool_search_threshold`), their schemas are loaded on demand through
  `tool_search` instead of being sent with every request.
- `/release-notes` shows what's new; `/todos` shows the agent's task list.
- Scripting flags like Claude Code: `--append-system-prompt <text>`,
  `--allowed-tools` and `--disallowed-tools` (Claude names such as `Read`,
  `Edit`, `Bash(git:*)`, or dcode-ai rules such as `execute_bash:cargo *`).

## 1.0.4 — 2026-10-15

### New (Claude Code parity)
- **Ctrl+B** moves a running shell command to the background; it keeps
  running, shows in the footer and `/bashes`, and the agent is notified
  when it finishes. Running commands show `(ctrl+b to run in background)`.
- **Thinking keywords**: `think hard`, `think harder` / `megathink` and
  `ultrathink` raise reasoning effort for that message (highlighted as you
  type).
- **Output styles**: `/output-style` with Default, Explanatory (★ Insight
  notes) and Learning (`TODO(human)`), plus custom `output-styles/*.md`.
- **Session cleanup**: sessions untouched for 30 days are deleted at startup
  (`[session] cleanup_period_days`, 0 = keep); `dcode-ai sessions --prune
  <DAYS>`.

## 1.0.3 — 2026-10-15

### Background shells (Claude Code style)
- `run_background` jobs show in the footer (`· 1 shell`), turns end with
  `✻ Crunched for 1m 9s · 1 shell still running`, and a finished job prints
  `● Background command "…" completed (exit code 0)` and wakes the agent with
  its exit code and last output.
- `/bashes` lists background jobs; `/bashes kill <id>` stops one.

### Changes
- `bypass-permissions` runs shell commands without asking (destructive
  commands are still refused).

## 1.0.2 — 2026-10-15

### Fixes
- Claude: writing about the `<tool_call>` protocol (in backticks, or a bare
  tag followed by prose) no longer fails the turn with "unterminated tool
  call"; a quoted `<tool_result>` no longer cuts the reply short.

## 1.0.1 — 2026-10-15

### Sessions
- `/resume` opens the session picker; `/resume <id>` jumps to a session.
- Switching sessions no longer hangs on MCP servers that ignore an unknown
  `shutdown` request (stdio servers are closed the MCP way, 3s cap each).
- A session that stopped mid-turn reopens idle, not stuck on "Thinking…".
- `-c` picks the most recent session; the picker and `-c` skip sub-agent
  sessions; sessions whose process is gone show as `ended`.
- No approve-all popup when reopening a conversation.

### Provider and model
- `/model`, `/provider`, F2 and sign-in remember your choice globally, and
  startup keeps it instead of switching to the last signed-in provider.

### Fixes
- Windows: file tools no longer reject workspace paths with 8.3 short names.
- Hooks that exit without reading their input are not reported as failed.

## 1.0.0 — 2026-10-15

First stable release: the Claude + GitHub Copilot edition. Changes since 0.0.43:

### Providers and sign-in
- Only two providers remain: **Claude** (your own signed-in `claude` binary by
  default, or Console OAuth via `ant`) and **GitHub Copilot** (device login).
  All other providers and API-key setup were removed; old configs still load.
- Copilot: native Messages API for Claude models, Responses API for codex
  models, image input on vision models.

### Terminal UI (Claude Code look and feel)
- `>` / `●` / `⎿` transcript grammar, Claude themes with a live `/theme` picker.
- Prompt input drawn with rules above and below (pink in `!` shell mode);
  approvals, `/rewind` and plan → build prompts replace the composer.
- `[Image #N]` and `[Pasted text #N +L lines]` chips, deleted as one unit.
- Shift+Tab mode cycle, Esc Esc rewind (code and conversation), `?` shortcuts,
  `/context`, `/effort`, `--plain` mode for screen readers.
- Ctrl+\ full-screen dashboard: sessions/agents rail, transcript, and a right
  rail with context, changes (per-hunk review and revert) and agents tabs
  (Alt+1/2/3, or Ctrl+D then ←/→).

### Extensibility
- Claude Code formats: `.mcp.json`, `commands/**/*.md`, `agents/*.md`, hooks,
  plugins and marketplaces with a `/plugins` browser, `/reload`.
- `dcode-ai mcp serve`, status line command, cost budget.
- Language servers chosen per project (`.lsp.json`, plugin `lspServers`).

### Automation and editors
- `dcode-ai ci` + GitHub Action (JSON result, step summary, exit codes).
- `dcode-ai acp`: Agent Client Protocol server for editors such as Zed, with
  `session/load` and image prompts.

### Safety
- `policy.toml` allow/ask/deny rules, `dcode-ai policy show|test`.
- macOS Seatbelt sandbox for shell commands (Linux uses Landlock).
- `/verify` read-only verifier, optional `verify_after_edits`.

### Tools
- `inspect_binary`: read-only static inspection of ELF/PE/Mach-O files
  (sections, symbols, imports, linked libraries, strings, cargo-auditable
  crates, Go build info).
