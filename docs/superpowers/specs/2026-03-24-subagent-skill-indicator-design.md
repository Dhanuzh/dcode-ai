# Subagent Skill Indicator Design

**Issue:** [#34](https://github.com/madebyaris/dcode-ai/issues/34) (sub-project 4 of 4)
**Date:** 2026-03-24
**Status:** Approved

## Summary

Show which skill a subagent is running — in both the TUI sidebar and the transcript. Uses existing `ChildSessionActivity` events with `phase: "skill"` emitted from `InvokeSkillTool`.

## Motivation

When subagents invoke skills, there's no visible indicator of which skill is active. Users need to see this for debugging and understanding what their agents are doing.

## Design

### Event Emission

When `InvokeSkillTool::execute()` successfully loads a skill, emit a `ChildSessionActivity` event:
- `child_session_id`: current session ID
- `phase`: `"skill"`
- `detail`: the skill command name (e.g., `"brainstorming"`)

This requires adding two fields to `InvokeSkillTool`:
- `event_tx: mpsc::Sender<AgentEvent>` — the event channel (same one used by `AskQuestionTool`)
- `session_id: String` — current session ID (needed for `child_session_id` field)

### Sidebar Display

Add `skill: Option<String>` field to `SubagentRow` in `crates/cli/src/tui/state.rs`.

In `apply_event` for `ChildSessionActivity`: when `phase == "skill"`, set `row.skill = Some(detail.clone())`.

In the sidebar rendering: when `row.skill` is `Some(name)`, append `[name]` after the task text, styled with `theme::WARN` or similar.

### Transcript Display

Already handled — `ChildSessionActivity` events are rendered as `"↳ {short_id}… · {phase} · {detail}"` in `state.rs:667-668`. A skill invocation will appear as:
```
↳ sub-abc… · skill · brainstorming
```

No transcript rendering changes needed.

### Registration Update

`InvokeSkillTool::new()` signature changes to accept `event_tx` and `session_id`. The registration in `supervisor.rs` passes these (both are already available at the registration site — `event_tx` is used by `AskQuestionTool`, `session_id` is generated at line ~185).

### What stays the same

- Event pipeline — uses existing `ChildSessionActivity`, no new event types
- `ChildSessionSpawned` / `ChildSessionCompleted` — unchanged
- Transcript rendering — existing `System` block formatting handles it
- All other subagent fields — unchanged

### Testing

- Unit test: `SubagentRow.skill` populated when `ChildSessionActivity` arrives with `phase == "skill"`
- Manual: invoke skill in subagent, verify sidebar shows skill label and transcript shows activity line

## Files Affected

- Modify: `crates/core/src/tools/invoke_skill.rs` — add `event_tx` and `session_id` fields, emit activity event on success
- Modify: `crates/runtime/src/supervisor.rs` — pass `event_tx` and `session_id` to `InvokeSkillTool::new()`
- Modify: `crates/cli/src/tui/state.rs` — add `skill` field to `SubagentRow`, populate on activity event
- Modify: `crates/cli/src/tui/app.rs` — render skill label in sidebar (if sidebar renders subagent rows)
