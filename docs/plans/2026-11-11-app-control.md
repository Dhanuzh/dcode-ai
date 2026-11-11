# Plan — controlling other apps (2026-11-11)

Goal: let the agent use the apps people work in, through the most reliable
door each app has. Order of preference (more reliable, faster, cheaper first):

1. **MCP server** — already supported (`.mcp.json`, plugins).
2. **CLI** — already supported through bash (`gh`, `m365`, `az`, `sqlcmd`…).
3. **API without MCP** — the agent writes a small MCP server (later:
   `dcode-ai mcp from-openapi`).
4. **Files** — `.xlsx`, `.docx`, `.csv`, SQLite edited directly.
5. **Web app** — the `browser` tool (CDP), already shipped.
6. **Electron / WebView2 desktop app** — the browser tool attached to the
   app's debug port. **This plan, part A.**
7. **Native Windows app** — the accessibility tree (UI Automation).
   **This plan, part B.**
8. **Anything else** — screenshot + vision + mouse/keyboard. Later (part D).

What exists today: `crates/core/src/browser.rs` drives Chrome/Edge over CDP
(snapshot with numbered elements, click, type, press, scroll, find,
wait_for, steps/replay), finds Windows browsers from WSL, and
`/browser connect <port>` attaches to any CDP endpoint. WSL here runs with
`networkingMode=mirrored`, so Windows-side debug ports are reachable.

## A. Electron and WebView2 apps (small)

Slack, VS Code, Postman, Discord, Notion, Obsidian are Electron: started
with `--remote-debugging-port=<p>` they speak CDP. New Teams and new
Outlook are **WebView2**: they take the same flag through the environment
variable `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=<p>`.

- `/app open <name|path>`: a table of known apps (name → exe locations
  under `%LOCALAPPDATA%` / `Program Files`, Electron or WebView2) plus any
  `.exe` path. Starts it with debugging on a free port (9340+), waits for
  `/json/version`, attaches.
- Single-instance apps ignore flags when already running: detect the
  running process (`tasklist.exe` from WSL) and ask before closing it.
- **Several connections**: the browser module's single shared page becomes
  a map `name → Page` (`browser`, `slack`, `teams`…). The `browser` tool
  gets an optional `target` (default `browser`); `/app list` shows them.
  Snapshots, steps and replay work unchanged.

## B. Native Windows apps: the `app` tool (UI Automation)

Same shape as the browser tool, over the Windows accessibility tree
instead of the DOM — cheap on tokens, no screenshots.

### Where it runs
- A Windows helper speaking JSON lines on stdio: `dcode-ai.exe uia-host`,
  built in the Windows release (Rust, `uiautomation` crate on
  `windows-rs`; no extra install).
- Native Windows dcode-ai: runs it in-process.
- **From WSL**: spawns the Windows exe through interop. Found at
  `%LOCALAPPDATA%\dcode-ai\bin\dcode-ai.exe`; when missing or a different
  version, downloaded from the matching GitHub release and its `.sha256`
  checked (the self-update code already does this) — asking first.
- One long-lived helper per session (UIA calls are cross-process; startup
  is the slow part). Properties fetched in batch with a `CacheRequest`.

### Actions
| action | does |
|---|---|
| `windows` | top-level windows: number, title, process |
| `launch {app}` | start an app (Start-menu name or path) |
| `focus {window}` | bring a window forward |
| `snapshot {window}` | the window as text: numbered controls with role, name, value, state (≤200, off-screen and decorative pruned) |
| `find {query}` | only the controls mentioning it |
| `click {ref}` | Invoke / Toggle / SelectionItem pattern, else a click at its centre |
| `type {ref, text, submit?}` | ValuePattern.SetValue, else focused keystrokes |
| `press {keys}` | `Ctrl+S`, `Alt+F4` is refused (see safety) |
| `read {ref}` | full value / text of a control (Value / Text pattern) |
| `select {ref, item}` · `expand {ref}` · `scroll {ref, dir}` | lists, trees, combo boxes, menus |
| `wait_for {text, gone?}` | until it appears / disappears |

After `click`/`type`/`press`, like the browser: only what changed in the
window. Refs hold for a snapshot; behind them the UIA RuntimeId, so a ref
survives small re-renders.

## C. Safety
- `app` actions that change something (`click`, `type`, `press`, `select`,
  `launch`) go through approvals, even in auto mode: the first action in
  each app per session asks; `[permissions] app_allow = ["Excel",
  "Notepad"]` pre-approves apps. Read actions (`windows`, `snapshot`,
  `find`, `read`) don't ask.
- Never types into password fields (UIA `IsPassword`) — asks the user.
- Refuses to drive UAC, Windows Security, credential dialogs, password
  managers; `Alt+F4` and close/delete-style keys need explicit approval.
- The TUI shows what's being driven (`🖱 Excel`); Esc stops it.
- Every action goes to the session's event log.

## D. Later
- Vision fallback: screenshot → Claude → mouse/keyboard, for apps with no
  usable accessibility tree (Citrix, canvas UIs).
- `/connect <app>`: picks the door — known MCP server, else an installed
  CLI, else browser/app.
- `dcode-ai mcp from-openapi <spec>`.

## Milestones
1. **A** — `/app open`, named CDP connections, `target` on `browser`.
2. **B1** — `uia-host` read-only: `windows`, `snapshot`, `find`, `read`;
   WSL interop + helper provisioning.
3. **B2** — actions with approvals and the safety rules (C).
4. **B3** — change diffs, `wait_for`, steps/replay shared with the browser.

## Tests
- Snapshot formatting and ref handling against a fake tree (all platforms).
- `windows-latest` CI: the helper against Notepad / a WPF test window.
- WSL: helper discovery, version check and download with a fake release.

## Questions
- Which apps first? (decides the known-app table and test targets)
- OK to download the Windows helper from the release when it's missing?
