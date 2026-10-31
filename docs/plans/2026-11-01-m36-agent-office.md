# M36 — Agent Office (v1.0.35)

Status: implemented (phases 1–3); watch-page office and footer sprite later

Every agent as an animated pixel-art character at a desk, in the terminal.

- `tui/office.rs`: `Office` (main agent + sub-agents) driven by events
  (busy state, tool calls, approvals, questions, compaction, sub-agent
  spawn/activity/finish; finished sub-agents leave after 8s). Art is drawn
  in code on a pixel canvas (wall with windows and a light sweep, plank
  floor, plant, water cooler; desk, chair, laptop whose logo glows in the
  activity's colour; six looks). Animation at ~8 fps from a shared clock:
  typing hands, head bob while thinking, raised waving hand for approval,
  eyes scanning while reading, papers while compacting, slumped with closed
  eyes when idle, arms up when done. Half blocks (`▀`) give two square
  pixels per cell; 2× scale when everyone fits.
- `/office` (full screen, `office_overlay.rs`): the room plus the selected
  agent's activity and recent log; ←/→/Tab/click to pick; Enter on an agent
  waiting for you returns to answer it.
- Dashboard tab `office` (`Alt+5`).
- Reviewed with real-terminal screenshots: the PTY test
  `office_screenshots` (ignored) dumps the screen's cells with colours and a
  small script turns them into PNGs.
