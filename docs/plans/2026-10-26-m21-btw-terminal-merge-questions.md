# M21 — `/btw`, `/terminal-setup`, sub-agent merge prompt, loop guard, fast resume, `/memory tidy`, question UI (v1.0.20)

Status: implemented

References: Claude Code's `/btw`, `/terminal-setup`, AskUserQuestion
prompt; Codex's repetition handling.

## 1. Question UI (ask_question)

The question used a centred popup labelled "approval" plus a duplicate
`[0] [1]` list in the transcript. Now it reads like Claude Code's
AskUserQuestion and takes the composer's place (same as permission prompts):

```text
── ☐ Database ─────────────────────────────────────
 Which database should the service use?

 ❯ 1. Postgres (Recommended)
      Relational, strong consistency
   2. SQLite
      Single file, no server
   3. Type something.

 Enter select · ↑↓ move · 1-9 pick · Esc answer in chat
```

- Payload gains optional `header` (chip title), per-option `description`,
  and `multi_select` (Space toggles, Enter submits; answer
  `QuestionSelection::Options { option_ids }`). All `serde(default)` so old
  logs and IPC clients still parse.
- The recommended option stays in its own place with "(Recommended)"
  instead of being moved to the top.
- "Type something." edits inline in the box; Enter sends it as a custom
  answer.
- Transcript: while open, nothing is duplicated; once answered it shows
  `⏺ <question>` / `⎿ <answer label>`. The resolved line reads
  "Answered: Postgres", not the debug form.

## 2. Choice prompts (shared)

A small `ChoicePrompt { title, body, options, index, action }` drawn by
`prompt_box` and answered with `TuiCmd::Choice(action, index)`. Used by
the merge prompt and `/memory tidy`. Shown only when no approval or
question is pending and no turn is running.

## 3. Sub-agent merge prompt

When a worktree sub-agent finishes with work, the runtime emits
`SubagentChanges { child_session_id, task, branch, base_branch,
worktree_path, files, insertions, deletions }`. The TUI queues a prompt:

```text
── Sub-agent changes ──────────────────────────────
 "add retry to the uploader" · dcode-ai/session-… · 3 files +42 -7
   M src/upload.rs
   A src/retry.rs
 ❯ 1. Merge into main
   2. Keep the branch for later
   3. Discard the changes
 d view diff
```

Merge commits any uncommitted work in the worktree, then `git merge
--no-ff` into the branch checked out in the main repo (refuses when the main
tree has uncommitted changes to the same files; on conflict it aborts and
keeps the branch), then removes the worktree and branch. Discard removes
both. `dcode-ai worktrees merge` uses the same code, so uncommitted
sub-agent work is no longer lost.

## 4. Loop guard (repeated text)

While streaming, every ~256 new bytes check whether the tail of the reply
is one chunk (1–600 bytes) repeated back-to-back over ≥ 1,200 bytes and ≥ 4
times. If so stop the stream, keep the text up to the first copy, and warn
"The model started repeating itself, so dcode-ai stopped it."

## 5. Faster resume

Cause found in a real log: one `grep` tool result was 23 MB, saved whole in
`events.jsonl` and replayed on every resume.

- `ToolCallCompleted` events carry the same capped output the model sees
  (24k chars), so logs stay small.
- Replay skips streaming deltas by a substring check before JSON parsing,
  parses straight into `EventEnvelope`, caps oversized tool outputs in old
  logs, and holds the state lock once.
- Long sessions show the last 200 blocks; older ones sit behind
  "⋯ N earlier messages · ctrl+o shows the full transcript".

## 6. `/memory tidy`

Deterministic, no model call: drop empty notes, exact duplicates
(normalised text) and near duplicates (word-set Jaccard ≥ 0.8, or one
contained in another), keeping the newest / fuller note. Shows the plan as
a choice prompt (Remove N notes · Cancel) per scope (you, project).

## 7. `/btw <question>`

Asks a side question about the current work. One tool-less model call with
the conversation so far (snapshot taken at each turn start/end, so it works
while the agent is busy). The answer appears in a panel in the composer's
place (Esc/Enter dismiss, ↑↓ scroll) and is never added to the
conversation.

## 8. `/terminal-setup`

- `\` then Enter inserts a newline in every terminal (Claude Code's
  fallback); Ctrl+J still works.
- kitty, Ghostty, WezTerm, foot: dcode-ai turns on the kitty keyboard
  protocol so Shift+Enter arrives as itself.
- `/terminal-setup` detects the terminal: VS Code / Cursor / Windsurf
  (also from WSL, writing the Windows-side `keybindings.json`) gets a
  Shift+Enter binding that sends `\` + Enter; Alacritty gets a
  `[[keyboard.bindings]]` entry; others get a short note on what already
  works.
