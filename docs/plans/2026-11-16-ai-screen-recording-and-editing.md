# Plan — AI screen recording with automatic editing (2026-11-16)

**Status:** phase 1 built in v1.0.67 — browser/app recording, event track,
automatic edit (speed ramps, zooms, cursor, ripples, captions, title/end
cards), generated background music, MP4 via ffmpeg (`/video setup`) + GIF +
SRT, `/record`, `/video edit|list|open|render|setup`. Decisions: captions now
(no voiceover yet), background music, plan from the event track only (no
frames to the model), ffmpeg for MP4 instead of a compiled-in encoder.

**Goal.** Ask for a video ("record a demo of signing up and creating a
project"); the agent does the task on screen while recording, then edits the
recording itself — cuts the waiting, zooms in on what it clicks, adds a
cursor, captions, title and end card — and hands back a finished MP4 (and a
GIF). Re-edit in plain words ("under 45 seconds, captions bigger"). No video
skills, no editor, nothing uploaded.

## What makes it good: the agent knows what happened when

A person editing a screen recording has to watch it to find the clicks and
the dead time. Here the recorder writes an **event track** next to the
frames: every action the agent took (click at x,y on "Create project", typed
"Acme", page load started/finished, waited for text, scrolled), with
timestamps, plus the agent's own one-line description of each step. The
editor plans from that track — precise cuts and zooms without guessing — and
uses a few sampled frames only to check.

## How it works

```
/record <goal>
   │
   ├─ 1. Record   agent drives the browser / desktop app (browser_task machinery);
   │              frames + event track → .dcode-ai/videos/<id>/raw/
   ├─ 2. Plan     the model turns the event track (+ ~12 sampled frames) into an
   │              edit list (EDL, JSON): keep/cut ranges, speed ramps, zoom
   │              targets, captions, title/end text, blur boxes
   ├─ 3. Render   Rust compositor applies the EDL frame by frame → MP4 + GIF + SRT
   └─ 4. Review   path + thumbnail strip in the TUI; /video edit "<what to change>"
                  → the model edits the EDL, render again (no re-recording)
```

### 1. Recording
- **Browser and Electron/WebView2 apps (phase 1):** the Chrome DevTools
  `Page.startScreencast` stream we already speak to for `browser` — JPEG
  frames with timestamps, no extra software, works on all platforms
  including WSL (the browser/app runs on Windows).
- **Whole screen (phase 2):** `xcap` (Windows, macOS, Linux X11/Wayland
  portal) at 10–15 fps; on WSL a small Windows-side capture through the
  same helper path `/app` uses.
- Event track from the agent's own tool calls (`browser` actions,
  `browser_task` steps) — exact coordinates, element names, text typed
  (passwords never written: masked at the source).
- A visible "● REC" in the status line, terminal title and herdr while
  recording; `/record stop` or Esc ends it.

### 2. Automatic edit plan (EDL)
The model writes a JSON edit list; rules it follows (and the renderer
enforces):
- **Dead time:** loading/waiting over 1.5 s → speed ramp (4–8×) or cut with
  a short cross-fade; idle at start/end trimmed.
- **Auto-zoom:** zoom 1.5–2× toward each click/typing target, ease in
  before the action, hold, ease out (Screen-Studio style); never zoom on
  fast sequences (< 0.8 s apart).
- **Cursor:** a smooth synthetic cursor along the recorded path, click
  ripples, key-press badges ("⌘K", "Enter") for shortcuts.
- **Captions:** one short line per step from the agent's step
  descriptions; burned in and as `.srt`.
- **Title and end card:** from the goal ("Creating a project in Acme");
  end card with the result.
- **Privacy:** blur boxes over password fields, emails, tokens (from the
  DOM and secret-pattern matching on visible text).
- **Length target:** default "as short as it stays clear"; `--max 60s`.

### 3. Renderer (Rust)
- Decode frames (`image`), crop/zoom with easing, draw cursor/ripples/
  badges, text with `ab_glyph` and a bundled open font, blur boxes.
- Encode **MP4 (H.264)**: built in, via `openh264` (Cisco's encoder,
  compiled in) + a pure-Rust MP4 muxer — no ffmpeg needed. If `ffmpeg` is
  installed it is used for speed and for audio.
- Also exports: a GIF (short, for PRs/Slack), `captions.srt`,
  `chapters.json`, and the EDL itself.

### 4. Review and re-edit
- Result in the transcript: path, length, size, a strip of thumbnails
  (the dashboard already draws images); `o` opens it in the system player.
- `/video edit <instructions>` — "cut the login part", "slower at the
  end", "add a caption: settings live here", "vertical for LinkedIn". The
  model changes the EDL; only rendering runs again.
- `/video list`, `/video open <id>`.

## Commands and tools
| Command | What it does |
|---|---|
| `/record <goal>` | The agent does it on screen, records, edits, delivers the video |
| `/record start` · `/record stop` | Record what happens (you or the agent), then auto-edit (phase 2: whole screen) |
| `/video edit [id] <instructions>` | Re-edit the last (or given) video |
| `/video list` · `/video open [id]` | Find and play them |

Agent tools: `record_start {target?}`, `record_stop`, `video_edit {id,
instructions}` — so "make a demo video of the new feature" works from a
normal prompt, and `browser_task` can record its run.

Config: `[video] fps = 15`, `max_minutes = 10`, `format = "mp4"`,
`gif = true`, `frames_to_model = 12` (0 = plan from the event track only).

## Phases
1. **Browser/app recording + auto-edit + MP4/GIF** (`/record <goal>`,
   `/video edit`). Most of the value; no new capture code per platform.
2. **Whole-screen recording** (`/record start|stop`), including your own
   actions (cursor from the capture, no keylogging), WSL support.
3. **Voiceover and polish:** narration written by the model and spoken by
   the system voice (`say` / Windows speech / espeak, rendered to audio),
   background music bed, captions in another language, 9:16 and 1:1
   exports, chapter titles.

## Tests
EDL validation (cuts in range, zooms inside the frame); renderer golden
frames (zoom, cursor, caption placement) on small synthetic videos; event
track from a scripted fake browser; encode → decode round trip of a 2-second
clip; password-field blur always applied; `/video edit` changes only the EDL.

## Risks
- **Encoder:** `openh264` is C code compiled into the binary (like `ring`
  already is). Pure-Rust alternatives are AV1 (`rav1e`, slow) or GIF only.
- **Model cost:** planning reads the event track (small); sampled frames
  are optional and capped.
- **Whole-screen capture on Wayland/WSL** varies by system — phase 2.

## Questions
1. Start with phase 1 (browser and desktop apps the agent drives), or do you
   need whole-screen recording of your own work first?
2. Built-in H.264 (openh264 compiled in) is fine, or require `ffmpeg`?
3. Voiceover in the first version, or captions only first?
4. May the editor send ~12 downscaled frames to the model to check its
   plan, or plan from the event track only?
