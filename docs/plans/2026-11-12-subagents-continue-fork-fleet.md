# Plan — sub-agents: continue, richer agent files, fork, /fleet (2026-11-12)

From the CLI survey (Claude Code, Codex, Copilot CLI, Droid, Gemini CLI,
OpenCode, Kiro, Amp): four gaps in dcode-ai's sub-agents.

**Status:** built in v1.0.62 (0–4).

## 0. Fix: sub-agents see the conversation as it is now
`spawn_subagent_consumer` builds the parent summary once, from the messages
when the session started — a sub-agent spawned an hour in sees the first
minutes. The agent keeps a mirror of its messages, refreshed right before a
batch that spawns or messages an agent; each request carries that snapshot,
and the summary is built from it.

## 1. `send_message` — continue an agent
- Tool `send_message {agent_id, message}`.
- Running agent → the message goes to its steer inbox (read at its next
  step, like typing while the main agent works).
- Finished agent → its child session is resumed (`Supervisor::resume`), with
  the same tool limits, model and worktree (or the main checkout if the
  untouched worktree was removed), and runs the message as its next turn —
  history intact. Background or foreground like `spawn_subagent`; same
  budgets, approvals, notifications, merge offer.
- Only this session's agents: the registry, or a child session whose
  `parent_session_id` is this session (after a restart). A stopped agent is
  continued only on purpose (it was stopped by `agent_kill`/`x`: refused).
- Every agent (foreground too) is recorded in the registry with its id, agent
  name and kind, so it can be continued.

## 2. More agent-file settings (Claude Code names)
`disallowedTools`, `permissionMode`, `maxTurns`, `effort`, `skills`,
`isolation`, `background` in `agents/*.md` frontmatter:
- `disallowedTools`: removed after `tools` (Claude names mapped).
- `permissionMode`: plan / default / acceptEdits / auto / dontAsk /
  bypassPermissions — never more permissive than what the parent's mode
  gives a child (plan stays plan).
- `maxTurns`: the child's turn limit.
- `effort`: low…max for the child's model.
- `skills`: these skills' instructions are put in the child's prompt.
- `isolation: worktree` forces a worktree; `none` runs in the checkout.
- `background: true` always runs it in the background.

## 3. Fork
`spawn_subagent {fork: true}`: the child starts with a copy of the
conversation so far (not a summary) — for side work that needs the whole
context. The in-flight tool call is dropped from the copy; provider-specific
blocks are cleared. Own worktree by default.

## 4. `/fleet [n] <task>`
A head-agent preset like `/board run`, without cards: split the task into
work items with dependencies (shown with `update_plan`), start the ready ones
as background sub-agents in worktrees (up to n, default 3, max 8), merge each
finished branch into the current branch before starting what depended on it,
continue a sub-agent with `send_message` when its result needs a fix, and end
with what was done, merged, and left for the user.

## Tests
Agent-file parsing of the new fields; permission clamp; tool filtering;
fork history trimming; registry ownership check for `send_message`; steer
delivery to a running agent; consumer uses the fresh snapshot.
