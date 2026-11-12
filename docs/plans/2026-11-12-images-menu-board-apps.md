# Plan — scrolling fix, smaller images, findable commands, board power features, app control (2026-11-12)

## 1. Mouse scrolling in the normal view (bug)
v1.0.57 left the mouse to the terminal in the normal (inline) view — unless
`[ui] mouse_capture = true`. Older versions wrote `mouse_capture = true`
into existing configs, so for those users the wheel is still captured, and
in the inline view the app has no history to scroll (it lives in the
terminal's scrollback). Fix: the inline view **never** captures the mouse;
`mouse_capture` only matters where the app owns the screen (dashboard,
overlays, which always capture). The wheel in the normal view scrolls the
terminal; text selects natively.

## 2. Smaller images
Images go to the model at full size (`image_attach.rs`, browser
screenshots). Claude bills by pixel area and downsizes anything over ~1568px
on the long edge itself, so sending more costs upload time and tokens for
nothing. Before an image joins a request: scale it so the long edge is at
most 1568px (and ≤ ~1.15 megapixels), re-encode PNG (JPEG q85 for photos
over 1 MB). Done once, where an attachment is built, for pasted images,
`@`-attached image files, `[Image #N]`, and browser screenshots.

## 3. Findable slash commands
The `/` menu lists 124 commands flat. Group them: **Recent** (last used,
newest first, from `~/.dcode-ai/recent-commands`), then **Session**,
**Context & cost**, **Board & Jira**, **Team**, **Agents & tools**,
**Settings**, **Help** — headings in the list, each command with its
one-line description. Typing filters as today (headings hide when a group
has no match).

## 4. Board power features
- `/` searches cards (key, title, description, assignee, labels); Esc
  clears.
- `f` cycles filters: everyone → mine (the Jira filter's person or the git
  user) → unassigned → by label (cycles labels on the board).
- `l` toggles swimlanes: cards grouped by parent/epic (Jira) or milestone
  (personal), one band per group.
- WIP limits per column: `/board wip <column> <n>` (0 = none); a column
  over its limit shows `3/2` in the error colour.

## 5. Long replies in the normal view
While a reply streams taller than the inline pane, a dim line at its
bottom: `… Ctrl+O shows the whole reply`.

## 6. App control, part A (from `2026-11-11-app-control.md`)
- `/app open <name|path>`: known apps (VS Code, Slack, Discord, Postman,
  Notion — Electron; Teams, Outlook (new) — WebView2 via
  `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`) or any exe; started with a
  debug port (9340+), attached over CDP. A running single-instance app is
  reported first (it ignores the flag).
- Named connections: the browser module keeps `name → page`; the
  `browser` tool takes `target` (default `browser`); `/app list`,
  `/app close <name>`.
- Part B (Windows UI Automation helper) follows in its own release.
