# Video recording, voice, talk and reliability — round 2 (v1.0.74)

Follows `2026-11-17-video-quality-and-workflow.md`. The JIVA tour recording
showed tiny footage and markers on the wrong page; v1.0.74 first fixed those
(standard 1440×810 viewport at 1920×1080, a still before each action).

## Recording

1. **Re-record one step** — `/video redo [id] <step>`. The agent replays the
   browser steps before it (`browser` `replay`), records only that step
   (`record_start` with `splice: {id, step}`), and `record_stop` splices the
   new frames, events and privacy boxes into the old recording in place of
   the step's window `[still before step n, still before step n+1)`. Later
   times shift by the length difference (events, frames, lines, zooms,
   segments, inserts, privacy, focus).
2. **Auto-frame the content** — after each action a JS scan measures the
   page's main content (text and image rects, area-weighted 3–97 %
   horizontal extent, ignoring fixed bars) → `focus.json`. The camera's
   resting framing fits that box (scale 1–1.5) instead of the whole page.
   `autoframe: false` in edl.json turns it off.
3. **Whole-screen recording** — `/record --screen`: ffmpeg captures the
   desktop (gdigrab on Windows, avfoundation on macOS, x11grab on Linux);
   on WSL a PowerShell loop captures the Windows desktop. Frames go to
   `raw/` like any recording; the agent narrates by time.
4. **Webcam picture-in-picture** — `/record --webcam`: ffmpeg records the
   camera beside the recording (v4l2 / avfoundation / dshow) to
   `webcam.mp4`; any other clip works via `webcam` in edl.json. The render
   puts it in a rounded corner bubble, following the edit's cuts and speed.
5. **Thumbnails in the terminal** — `/video frames [id]` and
   `/video show [id] [contact|N]` draw images in the transcript with
   half-block characters (every terminal).
6. **Old recordings** — the render reports clicks with no still before
   them and points at `/video redo`.
7. **Live-mode markers** — a marker ends when the picture changes a lot
   (more than a third of it, compared on small thumbnails), i.e. when a
   click opens another page. Applies to both modes.

## Voice

8. **Voice picker** — `/video voices` lists installed and downloadable
   Piper voices and which one is in use; `/video setup voice <name>`
   downloads one; `/video voices use <name> [id]` sets the default (or one
   video's `draft_voice`); `/video voices try <name> [text]` plays a sample.

## Talk

9. **Push-to-talk** — `/talk ptt`: speaking stops only when you press
   Enter (no pause detection); Esc still cancels.

## Reliability

10. macOS CI join tests: diagnose from the CI logs and fix.
11. CI runs the live recording/render tests in release mode against a
    headless Chrome.
12. Option dropdown sync — a test checks every option in the `/` menu
    table appears in its command's handler.
13. Render progress — the TUI shows percent done and time left.

Not doable from here: talking into a real microphone, a covered visible
Chrome window.
