# Plan — idle compaction, inline mouse, board work, ticket activity, checkpoints (2026-11-11)

Target release: v1.0.57. Each item is independent; ship in this order.

## 1. Inline view: scroll and copy (bug)
**Cause.** The inline view prints finished blocks into the terminal's own
scrollback (`insert_before`), so scrolling and selecting belong to the
terminal. But mouse capture is switched on whenever an overlay (`/board`,
diff, transcript, office) closes or the dashboard is left, and never off
again. From then on the wheel and drags go to an in-app transcript pane the
inline view doesn't show: nothing scrolls, nothing can be selected.

**Fix.** Mouse capture follows the view: on in the dashboard and overlays,
off in the inline view, unless `[ui] mouse_capture = true` (opt-in for
in-app wheel/drag-copy). One helper `inline_mouse(capture)` used at every
place that returns to the inline view.

## 2. Idle compaction
After a turn, the provider keeps the prompt cached for ~5 minutes. Compacting
inside that window reads the long context at cache price; the next turn
then starts small instead of re-sending everything uncached.

- `Supervisor::idle_compact()` — runs the auto-compaction path (summarize
  the older history, keep the recent messages) when: auto-compaction is on
  and not tripped, `[memory.context] idle_compaction = true` (default),
  context ≥ `idle_compaction_min_tokens` (default 40_000), and there is
  something old enough to summarize. Emits `ContextCompaction` events
  (`"Compacting while idle…"`).
- REPL idle loop (wakes every 500 ms): due when the agent has been idle for
  `idle_compaction_after_secs` (default 240) and less than that + 120 s
  (past it, the cache is cold and compacting saves nothing), the TUI is
  idle (no approval/question/picker, composer empty, no running
  sub-agents). Once per idle period.
- Typing during it: commands queue and run after (a few seconds).

## 3. Resume picker: files touched
The picker already searches names, first prompts and conversation text.
Add the paths the session's tools wrote/edited (from tool calls in the
saved messages) to the searchable text, and show a `files:` hint in the
snippet when a path matched.

## 4. Jira board
### 4a. Ticket activity
During a pull, compare each ticket with what the board had: a new comment
by someone else (author ≠ me), or a status change the board didn't make →
`remote.activity` = "Mamatha commented" / "moved to Resolved", shown as a
`●` chip on the card and in its view, plus a desktop notification (the
TUI's notify path) for tickets assigned to me. Opening the card clears it.
Never on the first pull of a ticket.

### 4b. Worklog and attachments
- Details read `getJiraIssue fields:[attachment,timetracking]`
  (search and the evidence view leave attachments out).
- Card view lists attachments; `a` downloads them
  (`executeRead downloadJiraIssueAttachment` → short-lived URL) into
  `.dcode-ai/attachments/<KEY>/` and opens the folder/file.
- `w` in the card view logs time (`2h 30m [note]`) →
  `executeWrite addOrEditJiraIssueWorklog`. Also
  `/board jira log <KEY> <time> [note]`.

### 4c. Saved views (several projects / filters as tabs)
`JiraLink.views: Vec<{name, jql}>` + active view. `/board jira view add
<name> <JQL>`, `/board jira view <name>|default`, `/board jira view rm
<name>`; `v` in the board cycles views. A view replaces the pull JQL
(`project in (BSI, OPS) AND assignee = currentUser()` covers several
projects); new tickets still go to the linked project. Switching clears
synced tickets and resyncs.

### 4d. Work a ticket
`/board work <KEY> [worktree]`:
- Moves the card to In progress (its ticket follows).
- Default: switch to branch `<key>-<slug>` in this repo (refuses with a
  hint when the tree is dirty). `worktree`: the agent hands the work to a
  sub-agent in its own worktree.
- Prompt: the card in full (description, fields, links, comments), plus:
  implement, test, commit, push, `gh pr create` titled `<KEY>: <title>`,
  then `board_update` the card (note, branch) with `comment` = the PR
  link — a new `board_update` field that posts a comment on the ticket.

## 5. Checkpoints that survive restart and compaction
Today `/rewind` (Esc Esc) restores code and/or conversation, but its turn
snapshots live in memory (lost on restart/resume) and compaction breaks
the message positions (can't rewind the conversation past it — idle
compaction would make that common).

- Persist the undo/redo stacks under `<sessions>/<id>.checkpoints/`:
  `index.json` (entries: epoch, msg_pos, paths, blob hashes) + `blobs/<sha256>`
  file contents, deduplicated. Written after each turn and undo/redo;
  loaded on resume. Cap: newest 100 turns / 256 MB.
- Epochs: a compaction archives the full message list it replaces to
  `<id>.checkpoints/epoch-<n>.json` and bumps the epoch; turns keep
  `(epoch, msg_pos)` instead of being invalidated. Rewinding to a user
  message from before a compaction restores that epoch's messages up to
  it, and files as before.

## Out of scope
Threaded Jira replies (the API has none), attachment upload, worklog edit.
