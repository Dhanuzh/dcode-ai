# Sessions, browser, video, TUI, reliability, talk extras — round 3

Requested 2026-11-17 after v1.0.74. Builds on what exists: `/undo`,
`/redo`, `/rewind` (checkpoints of code and conversation), `[session]
max_cost_usd` (a hard stop), the browser's persistent profile, `/export`.

## Agent and sessions
1. **Checkpoints** — `/checkpoint [name]` lists/takes a named snapshot of
   the working tree (a git stash-like commit object, not touching the
   branch); `/checkpoint restore <n>`. Each turn's automatic snapshot (the
   one `/rewind` uses) is listed too.
2. **Resume across machines** — `dcode-ai session export <id> [file]`
   writes one bundle (state + event log + memory, secrets masked);
   `dcode-ai session import <file>` adds it to this project; `/resume`
   continues it.
3. **Cost guardrails** — `/budget <usd>` for this session (and
   `max_cost_usd` as the default): at 80 % a warning; at 100 % the agent
   pauses and asks — continue (+50 %), raise to …, or stop — instead of a
   hard stop.
4. **Compaction you can see** — `/pin <fact>` keeps facts that every
   compaction carries over verbatim; after compacting, a note lists what
   was dropped (turns, tool outputs, files) and `/pins` shows the pins.

## Browser and recording
5. **Flows as smoke tests** — `/browser test save <name>` saves the
   recorded steps with a screenshot per step as the baseline;
   `/browser test run [name|all]` replays them and compares each
   screenshot (grey thumbnail difference); a report with the changed steps.
6. **Sign-in reuse** — `/browser remember [site]` saves that site's cookies
   (session cookies too) to `~/.dcode-ai/sites/<host>.json` (0600); they
   are put back whenever the browser opens that site. `/browser forget`.
7. **Multi-tab recordings** — when a click opens a new tab during a
   recording, the recorder follows it (screencast and stills switch to the
   new tab).

## Video output
8. **YouTube chapters and description** — `description.txt`: title,
   summary, chapter timestamps (`0:00 Intro`), ready to paste.
9. **Brand kit** — `~/.dcode-ai/brand.json` (or `.dcode-ai/brand.json`):
   logo (corner watermark and title card), colours (background gradient,
   caption pill), font, intro/outro text cards. `/video brand` shows/sets.
10. **Batch** — `/video render all` re-renders every recording.
11. **How-to guide** — `/video doc [id]`: `guide.md` and `guide.html`
    (self-contained) from the stills and narration, one section per step.

## TUI
12. **Notify when long jobs finish** — renders, CI watches and background
    sub-agents send the desktop notification + sound.
13. **Command palette** — Ctrl+P: one search over slash commands, recent
    prompts and project files.
14. **Inline images** — kitty graphics protocol when the terminal is
    kitty/WezTerm/Ghostty (sixel where supported), half-blocks elsewhere.

## Reliability and performance
15. **Hardware encoding** — ffmpeg's NVENC / QuickSync / VideoToolbox /
    AMF when listed and working (a one-frame probe), else libx264.
16. **Crash reports** — `/bugreport` (and a panic hook) writes one zip:
    logs, session state, config with secrets masked, version, OS.
17. **Fuzz tests** — proptest on the IPC frame reader and the
    `<tool_call>` text protocol parser.

## Talk extras (round 4)
18. **Wake word** — `/talk wake`: listens in the background for "hey
    dcode" (Whisper on short windows, VAD-gated), then takes the request.
19. **Multilingual** — `/talk` keeps Whisper's detected language and asks
    the agent to answer in it; the spoken reply uses a voice for it.
20. **Daily recap card** — `/recap`: an image (and a short video) of
    today: sessions, commits, tests, files changed.
21. **Achievements** — streaks, tests fixed, bugs closed; `/stats` shows
    them, a quiet toast when one is earned.
22. **Voice persona** — `/talk persona calm|brief|cheerful`: the tone of
    spoken replies.
