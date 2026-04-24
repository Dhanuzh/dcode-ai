# AGENTS.md-backed instructions and skills

## Goal

Treat the repo root `AGENTS.md` as both:
- an extended system-prompt instruction source that can steer agent behavior
- a lightweight skill manifest for reusable slash skills

## Why

- Teams already maintain `AGENTS.md` as durable project guidance.
- `dcode-ai` already discovers skills from `.dcode-ai/skills/` and home-level skill directories.
- Reusing `AGENTS.md` lowers setup friction for repo-local skills and makes skill discovery visible in version control.
- Project guidance in `AGENTS.md` should shape how the model reasons even when no explicit skill is invoked.

## Design

- The full repo-root `AGENTS.md` is loaded into the layered system prompt as an additional instruction block.
- `AGENTS.md` extends the built-in prompt; it does not replace built-in policy, `.dcode-airc`, or local instructions.
- Prompt order should stay stable: built-in -> permission mode -> `AGENTS.md` -> `.dcode-airc` -> local instructions -> skill summaries -> orchestration.
- Each root-level `## Heading` in `AGENTS.md` is parsed into one discovered skill.
- Optional directive bullets at the top of a section configure:
  - `model=<alias|inherit>`
  - `permission_mode=<plan|accept-edits|dont-ask|bypass-permissions|inherit>`
  - `context=<inline|fork>`
- `AGENTS.md` skills are loaded before filesystem skills and win on command conflicts.
- All discovery surfaces should show the source so users can tell `AGENTS.md` skills from directory-based skills.

## User-facing surfaces

- Harness/system prompt layering
- `dcode-ai skills` and `dcode-ai skills --json`
- Harness skill summary in the system prompt
- TUI slash palette and REPL slash execution

## Documentation updates

- Document `AGENTS.md` as an instruction source in `README.md`.
- Document `AGENTS.md` as a skill source in `README.md`.
- Document the default skill directories alongside the new manifest source.
- Show that `dcode-ai skills` includes source metadata for discovered skills.

## Current status

- `crates/core/src/skills.rs` parses `AGENTS.md` sections into `Skill` entries.
- `crates/cli/src/tui/app.rs` includes discovered skills in the slash palette.
- Remaining follow-up after this pass is to layer `AGENTS.md` into the system prompt path and keep docs/tests aligned.
