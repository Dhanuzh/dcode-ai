# M25 — Browser control (v1.0.24)

Status: implemented

Let the agent drive a real Chrome/Edge page (Antigravity-style), Rust-only,
over the Chrome DevTools Protocol (CDP).

## Shape

- **One tool, `browser`, with an `action`**: `navigate`, `snapshot`,
  `click`, `type`, `press`, `scroll`, `back`, `screenshot`, `console`,
  `eval`. One schema keeps the per-request tool text small.
- **Snapshot instead of pixels**: the page as text — title, URL, numbered
  interactive elements (`[3] button "Sign in"`, `[5] input type=email`)
  and the visible text (capped). `click` / `type` take the `ref` number.
  Navigation and clicks return the new snapshot, saving a round trip.
  `screenshot` saves a PNG under `.dcode-ai/screenshots/` for models or
  people that want to look.
- **Real input**: clicks are CDP mouse events at the element's centre after
  scrolling it into view; typing focuses the element and uses
  `Input.insertText`; keys use `Input.dispatchKeyEvent`.

## Browser

- `browser` launches Chrome (else Edge, Chromium, Brave) with its own
  profile (`~/.dcode-ai/browser-profile`; on WSL the Windows browser and a
  Windows-side profile under `%LOCALAPPDATA%\dcode-ai`) and
  `--remote-debugging-port=9333`, visible, so the user sees what happens.
  Logins made in that window persist for later sessions.
- `/browser connect <port>` attaches to a browser the user started with
  `--remote-debugging-port` (their own profile). `/browser` shows status,
  `/browser open [url]` launches, `/browser close` disconnects.
- `DCODE_AI_BROWSER` picks the executable; WSL works with mirrored
  networking (the Windows browser's port is on localhost).

## Safety

- Approval tiers by action: `snapshot`, `screenshot`, `console` read-only;
  `navigate`, `click`, `type`, `press`, `scroll`, `back` like file edits
  (ask in default mode, allowed in accept-edits / auto); `eval` asks unless
  bypass or an allow rule.
- `type` into a password field is refused: the user signs in themselves.
- Page text, console output and eval results are tool output: secrets are
  redacted at the usual choke point.
