# M10 — Claude-style input box, image chips, `inspect_binary`

Date: 2026-10-15 · Branch: `feat/claude-copilot` · Status: ✅ done (live-tested in tmux; image sent to Copilot `claude-sonnet-5`) · Parent: [v1 master plan](2026-10-12-v1-master-plan.md)

Request: "I want image like this input box … I want everything like Claude",
"the ⏺ dot icon can't change", and "check REA (Reverse Engineer Anything) —
reverse-engineer Claude, I mean".

## Reference: what the installed Claude Code does
Read from the user's own `claude` binary with `strings` (UI strings and
layout props only; no code or credentials taken):
- Prompt input: `borderStyle:"round", borderLeft:false, borderRight:false`
  → a rule above and below, no sides; dim colour; `bashBorder` (pink) in
  `!` shell mode.
- Permission prompts: top rule only (`borderBottom:false`, no sides).
- Placeholders: `[Image #N]`, `[Pasted text #N +L lines]`; the composer
  edits them as one unit.
- Image paste key: `alt+v` on Windows/WSL, `ctrl+v` elsewhere.

## Changes
1. **Input box**: top/bottom rules (muted; pink for `!`, red in bypass mode);
   the cursor sits on the placeholder's first letter.
2. **Image chips**: pasting an image (clipboard, dropped/pasted path,
   `/image`) inserts `[Image #N]` at the cursor instead of a system line.
   Backspace deletes a chip whole; deleting it drops the image. Paste
   placeholders become `[Pasted text #N +L lines]`. Chips are accent-bold.
   The `[1 image(s)]` suffix is dropped when chips already name the images.
3. **Bullet**: `●` instead of `⏺` (`⏺` renders as a fixed-colour emoji in
   Windows Terminal, so the success/error colour never showed). A running
   tool's bullet blinks.
4. **Copilot images**: vision models (Claude, GPT-4o/4.1/5, Gemini, o3/o4)
   accept images; requests with images send `Copilot-Vision-Request: true`.
5. **`inspect_binary` tool** (REA-inspired, Rust-native via `object`, no
   Node/MCP): read-only static inspection of ELF/PE/Mach-O — `summary`
   (format, arch, entry, PIE/stack protector, build id, libraries,
   toolchain hints), `sections`, `symbols`, `imports`, `strings` (with
   offsets, filter), `deps` (cargo-auditable crate list from `.dep-v0`, Go
   1.18+ build info). Findings cite sections/offsets.

REA itself (Node MCP server) still works through `.mcp.json` for users who
want Ghidra/Hopper/IDA-backed analysis; it is not bundled.

## Verification
Unit: placeholder parsing/ranges, atomic backspace, inline staging and
drop-on-delete, Copilot vision caps, preview suffix, `inspect_binary`
(real executable, strings offsets, cargo-auditable JSON, Go build info).
Live (tmux): `describe [Image #1] please`, chip deletion, image → "Red".

## Follow-up batch (same day, "do other pending things also")
6. Dashboard right rail: Alt+1/2/3 select context/changes/agents; Ctrl+D
   focuses the rail and ←/→ (or Tab/Shift+Tab, h/l) switch tabs; hint in
   the rail's bottom border. Changes tab skips tracked `.dcode-ai/` files.
7. A prompt refused before reaching the agent (hook block, image check)
   still appears in the transcript above its error.
8. `inspect_binary` summary lists ELF `DT_NEEDED` libraries.
9. ACP: `session/load` (resume + replay user/assistant history),
   image prompt blocks (`promptCapabilities.image = true`); ACP session ids
   are the real dcode-ai session ids. Live-tested new → prompt → load.
   Still open: routing file reads/writes through the editor (`fs/*`) and
   editor terminals.
10. Version 0.1.0 + `CHANGELOG.md`.
