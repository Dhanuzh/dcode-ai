# Plan — narrated tutorial videos, the vidni way (2026-11-16)

**Status:** built in v1.0.69.

**Idea.** [vidni](https://github.com/dhilipsiva/vidni) (MIT/Apache, Rust) makes
narrated tutorial videos from one script: a draft voice first, then your own
recorded voice, many cuts (16:9 and 9:16 shorts), captions, chapters,
thumbnails and an upload folder per platform, with a person signing off.
dcode-ai already records and edits the screen (`/record`, v1.0.67); this
adds vidni's narration and publishing model on top of the same edit list.

## The script lives in the edit list (`edl.json`)
- `lines[]` — `{id, at, text, section, only}`: what is said, from recording
  second `at`. **IDs are permanent**: takes are filed under them.
  `section` makes chapters; `only: ["short"]` keeps a line to some cuts.
- `inserts[]` — `{at, seconds, kind, title, items}`: cards placed into the
  video (`section`, `bullets`, `quote`) or a `hold` on the current frame.
- `cuts` — `["long"]`, `["long", "short"]`: 16:9 master and a 9:16 short.
- `voice` — `auto` (your take when recorded, else the draft voice),
  `draft`, `takes`, `none`. `draft_voice` — `sapi[:name]`, `say[:name]`,
  `piper[:model.onnx]`, `espeak`, or empty for the first one available.
- `clean_master` — also write the long cut without burned-in captions
  (for YouTube with uploaded captions); shorts are always captioned.
- `platforms` — upload folders made by `/video publish`.

## Timing: the video waits for the voice
Each line's audio (take or draft) is measured; when the next line would
start before this one ends, a hold is added just before the next action.
Narration ducks the background music. Captions follow the spoken lines.

## Voices
- **Draft voice**, cached per line (re-drafted only when its text or voice
  changes): Piper when installed (`~/.dcode-ai/bin/piper` + a model in
  `~/.dcode-ai/voices/`), else the system voice — Windows SAPI (also from
  WSL), macOS `say`, `espeak-ng`.
- **Your voice**: `/video voice [id] [line]` prompts line by line, records
  with the microphone (`/talk`'s), trims silence, saves
  `voice/<line>.take.wav`, and checks it with Whisper.
- **`/video hear`**: Whisper listens to every take and flags one that
  doesn't sound like its line (advice only).

## Outputs (`.dcode-ai/videos/<id>/`)
`video.mp4` (captioned) · `video-clean.mp4` · `short.mp4` (9:16, captions
clear of the app overlays) · `video.gif` · `captions.srt/.vtt` ·
`short.srt` · `chapters.txt` · `thumbnail.png` · `short-thumbnail.png` ·
`short-review.png` (the edges the apps cover, shaded).

`/video publish` → `out/<platform>/<cut>/` with video, captions, title,
description (with chapters) and thumbnail, for YouTube, YouTube Shorts,
Instagram Reels, WhatsApp Status, with length checks.

## Sign-off (the person's)
`/video approve` records the rendered files' sha256 in `approved.json`;
`/video status` shows takes recorded, drafts, renders, and whether anything
changed since approval. Agents edit the script and render; recording takes
and approving have no tool — only a person does them.

## Not taken from vidni
Byte-exact rebuild checks, the egui app, the TOML project file (our edit
list is JSON the agent already writes), uploading.
