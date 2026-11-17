# Plan — video quality, narration and workflow (2026-11-17)

**Status:** built in v1.0.72 (see "Not in this release").

Follow-up to `2026-11-16-narrated-videos.md`, from a real dashboard demo
that came out as one still image and showed real ticket ids and a name.

## Picture
- **Settle after every action** while recording: page loaded, then two
  screenshots a quarter second apart identical (≤ 4 s) — no spinners or
  half-drawn tables.
- **Stills mode** (`/record --stills`, `record_start {mode: "stills"}`):
  no live recording; one settled screenshot per step (vidni's way), a slow
  camera drift over each still and a short dissolve between them. Immune to
  covered/background windows.
- **Highlight**: the element clicked or typed into gets an outline and the
  rest of the frame dims slightly while the camera is on it
  (`highlight: true`).
- **Privacy blur**: at the start and after every action the recorder finds
  emails, ticket ids (`ABC-123`), tokens/keys, password and email fields,
  plus your own `redact` strings, and stores their boxes
  (`privacy.json`); the renderer pixelates them (`privacy: true`). Manual
  boxes: `blur: [{from, to, x, y, w, h}]`.

## Narration
- **`/video setup voice`**: downloads Piper and an English voice
  (`en_US-lessac-medium`) into `~/.dcode-ai/` — used automatically.
- **`respell: [{text, say}]`**: how the draft voice says words
  ("UX" → "you ex"); captions keep the written form.
- **Takes played back** right after recording (`/video voice`), with the
  Whisper check; a take that sounds wrong is offered again.
- **Translations**: `/video translate [id] <language>` — the agent writes
  `translations.<lang>` (same line ids); `video_render {lang}` renders
  `video.<lang>.mp4` + `captions.<lang>.srt` with the draft voice
  (`voices.<lang>` picks it).

## Workflow
- **`/video preview`**: a fast low-resolution draft (`preview.mp4`).
- **Review notes**: `/video note [id] <line> <text>`, `/video notes`,
  `/video apply` (the agent applies them and takes them out) — `notes.md`.
- **Contact sheet** after each render (`contact.png`, 12 frames), path in
  the transcript; `/video open contact`.

## Scenes
- `inserts` kinds `code` (title = file, items = lines) and `terminal`
  (items = lines; `$ ` lines are commands) drawn in a bundled monospace
  font (Adwaita Mono, OFL).

## Speed and cost
- Frames drawn on all cores (rayon), written in order.
- GIF: unchanged frames merged (longer delay), 8 fps.
- `record_stop` returns a compact event summary and the draft's script,
  not the whole edit list (it is in `edl.json`).

## Not in this release
- Re-recording one step and splicing it in (the timeline keeps
  recording order; needs reorderable segments).
- Whole-screen recording and a webcam picture-in-picture (platform
  capture; can't be tested here).
- Thumbnails drawn inside the terminal (the contact sheet is the stand-in).
