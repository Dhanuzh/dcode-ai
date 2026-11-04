# M47 — Task board, Scratch tab, images in team chat (v1.0.53)

Status: implemented

Ideas from Beluga (MIT; ideas only, written fresh in Rust).

## 1. The task board (Jira-style kanban) and the head agent

A board per project, kept in `<project>/.dcode-ai/board/board.json` (so every
session in the project sees the same board; `.dcode-ai/` is git-ignored).

### Model (`dcode_ai_common::board`)

```
Board { prefix: "DCO", next: 13, milestones: [Milestone], cards: [Card] }
Milestone { id, title }
Card {
  key: "DCO-12", title, description,
  status: backlog | todo | in_progress | review | done,
  priority: low | medium | high | urgent,
  assignee: Option<String>,        // a person, "agent", or a sub-agent session id
  blocked_by: [key],               // must be done first
  milestone: Option<id>,
  gate: bool,                      // review gate: stops for you before done
  branch: Option<String>,          // the worktree branch working on it
  notes: [Note { at, by, text }],  // what the agent did, links, results
  created, updated,
}
```

- The key prefix is the project folder's first letters (`dcode-ai` → `DCO`).
- A card is **ready** when it's `todo` and every blocker is `done`.
- Writes are atomic (temp file + rename) and re-read before each change, so
  the TUI and the agent never lose each other's edits.

### The agent's tools (runtime, always loaded)

- `board_view` — the board as text: columns, keys, blockers, ready cards.
- `board_add` — new cards (one or many), with priority, blockers, milestone,
  gate.
- `board_update` — status, assignee, blockers, priority, branch, a note.

### In the TUI: `/board`

A full-screen kanban: **Backlog · To do · In progress · Review · Done**.

- `←/→` column, `↑/↓` card, `Shift+←/→` (or `<`/`>`) moves the card.
- `n` new card (type a title), `e` edit title, `p` priority, `g` gate on/off,
  `b` blocked by (type keys), `x` delete (press twice), `Enter` details.
- Cards show key, title, priority mark, assignee, `⛓ DCO-3` blockers
  (red while open), `◆ gate`, and a milestone tag; blocked cards are dim.
- `/board add <title>`, `/board list` work without the overlay.

### Plans and the head agent

- `/board plan <goal>`: the agent splits the goal into milestones and cards
  with blockers and review gates (with `board_add`), then shows the board.
- `/board run [n]`: the agent becomes the head agent. It takes the ready
  cards (unblocked), starts up to `n` (default 3) sub-agents in their own
  worktrees in parallel, moves each card to In progress (with its branch),
  then Review when its sub-agent finishes, and continues with what became
  unblocked. A card with a gate stays in Review until you move it to Done
  (and merge its branch through the usual prompt); the head agent stops and
  tells you what waits for you.

## 2. Scratch tab

- Every session has a scratch folder,
  `<workspace>/.dcode-ai/scratch/<session-id>/` (git-ignored). The system
  prompt tells the agent to put working files there: test logs, screenshots,
  API responses (JSON), generated data — so you see what it sees.
- Dashboard tab **scratch** (`Alt+7`): the files, newest first, live; the
  selected one previewed: text/log tail, JSON pretty-printed, an image as a
  small colour thumbnail. `Enter` adds `@path`, `o` opens it in your viewer.

## 3. Images in team chat

- In the team tab (and the guest's), `Ctrl+V` with an image on the
  clipboard sends it; `/chat image <path>` too. Shrunk to at most 1280 px
  and 1.5 MB (PNG/JPEG).
- `AgentEvent::PersonChat` and `AgentCommand::Chat` get an optional
  `image { name, mime, w, h, data (base64) }`; it reaches guests over the
  invite like any chat line, and the browser page shows it.
- Drawn as a colour thumbnail (half blocks, 28×8 cells) under the line;
  `/chat open [n]` opens the n-th picture (the last by default) in your
  viewer. The transcript gets a one-line note.
- Browser guests see pictures; sending from the browser page is left for
  later.
- Images are people-only, like chat: the agent never sees them.

## Tests

- Board: keys, ready/blocked, atomic save + reload, moves; tools add/update
  and view; PTY: `/board` opens, `n` adds a card, `>` moves it.
- Scratch: listing newest first, previews (text tail, JSON, PNG thumbnail).
- Chat image: shrink + encode, event round trip, guest → host → event.
