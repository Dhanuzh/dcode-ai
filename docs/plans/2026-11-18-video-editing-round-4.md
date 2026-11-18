# Video: recording and editing, round 4 (v1.0.80)

Requested 2026-11-18 after a ~6-minute screen demo needed every cut and
zoom placed by hand.

## Recording
1. **Smoother WSL screen capture** — the PowerShell loop was paced at 5
   shots a second and re-created its bitmap every frame. Reuse one bitmap
   and graphics object, encode at JPEG 75, aim for ~12 a second (measured
   on the real desktop).
2. **The cap is announced** — recordings stop taking frames at the cap
   (now 30 min, was 10). Browser actions and `record_note` say how much is
   left once under 2 minutes; `record_stop` says when the cap cut the take.
3. **Pause and resume** — `record_pause` / `record_resume` (tools) and the
   recorder drop what happens in between: frames, events, private and
   content boxes. Everything is kept on the raw clock and the paused
   stretches are cut out at stop; a webcam clip is kept in step.

## Editing
4. **Automatic cuts for screen recordings** — at stop, frames are compared
   (small grey thumbnails); stretches with no change become fast-forwards
   (1.2–6 s at 3×) or cuts (over 6 s, keeping 0.6 s each side), the same
   rules as browser recordings.
5. **Automatic zoom from screen changes** — a burst of change confined to a
   small part of the screen (under a third) gets a zoom on it.
6. **Text anchors** — narration lines and zooms may say `"anchor": "text"`;
   the time comes from when that text first shows on the page after the
   previous line (browser recordings keep each step's visible text in
   `text.json`). Unresolved anchors are reported.
7. **Edit report** — `/video check [id]` (and `video_render` with
   `check: true`): length per cut, narration longer than its footage, zooms
   that merge, lines in cut-away stretches, unresolved anchors, cap and
   pause notes — without rendering.
8. **Timeline view** — `/video timeline [id]`: a time bar in the terminal
   with segments (speed), zooms, narration lines and cards;
   `/video nudge [id] <line-id|zoom-N> <±seconds>` moves one.
