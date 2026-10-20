# M14 — Real /compact, vim mode, startup popup, Copilot tool-less requests

Date: 2026-10-20 · Branch: `main` · Status: ✅ done (v1.0.7) · Parent: [v1 master plan](2026-10-12-v1-master-plan.md)

1. **`/compact [focus]`** (Claude Code parity). It used to save a summary
   note but keep the whole history. `Supervisor::compact_now` now has the
   model summarize the entire conversation (focus text gets most detail),
   replaces the history with that summary and saves the session; pre-compact
   hooks run with trigger `manual`.
2. **Copilot tool-less requests.** `"tools": null` was rejected by Copilot
   ("tools must be an array"), so every compaction summary there silently
   fell back to the extractive one. Both request builders omit `tools` when
   empty; summary errors are now logged.
3. **Startup approve-all popup.** Esc did nothing and typed text vanished
   into the popup (a fast first message was lost). Esc or typing now keeps
   the default approval flow, and typed keys reach the prompt.
4. **`/vim`.** NORMAL/INSERT modes in the composer with the common motions
   and operators; Esc still interrupts a running turn first. Footer shows
   the mode; `[ui] vim_mode` persists it.
5. **Supply chain.** zbus 5's split crates (via notify-rust) are flagged
   unmaintained with no fixed release; ignored in `deny.toml` with a note.

## Verification
Unit: vim editing sequences; tool-less body omits `tools`. Live (tmux,
Copilot `claude-sonnet-5`): code word survives `/compact keep the code word`
with a model-written summary; typing into the popup lands in the prompt;
vim `0wwdw`, `A`, mode indicator.
