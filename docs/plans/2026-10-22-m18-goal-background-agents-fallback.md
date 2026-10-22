# M18 — /goal, background sub-agents, fallback models (v1.0.14)

Status: implemented

References studied: Codex (`codex-rs/ext/goal`: `create_goal` / `update_goal`
tools, continuation template, statuses Active · Paused · Blocked ·
BudgetLimited · Complete) and Alter Zero (`docs/agent-tools.md`: agent ids,
`run_in_background`, completion notices, `agentoutput` / `agentkill`, agents
never nest). Fallback models follow Claude Code's `fallbackModel`.

## 1. `/goal`

- `/goal <objective> [--budget <tokens>]` sets a goal for the session and
  starts work on it. `/goal` shows it; `/goal pause | resume | clear`.
- After every turn while the goal is active, dcode-ai sends a continuation
  message (adapted from Codex's template: keep the full objective, work from
  current evidence, completion audit before claiming done, blocked only after
  the same blocker three turns running).
- The model ends it with the `update_goal` tool (`complete` or `blocked`).
- Stops on: complete, blocked, token budget reached, 50 automatic turns,
  Esc / cancel (pauses), an error turn (pauses).
- The footer shows `◎ goal · turn N`.

## 2. Background sub-agents

- `spawn_subagent` gains `run_in_background` (default **true**, like Claude
  Code). A background launch returns at once: `agent <id> started in the
  background; you will get a <task-notification> when it finishes`.
- When it finishes, the parent gets a `<task-notification>` with the
  agent's final report (same path as background shells), and is woken when
  idle.
- New tools for the lead: `agent_output {agent_id, wait?}` (status, recent
  activity, final report; `wait` up to 600 s) and `agent_kill {agent_id}`.
- A background agent's approval prompts appear in the main session (the
  child registers them in the parent's pending-approval map and announces
  them on the parent's event stream) instead of being auto-denied.
- Sub-agents never get `spawn_subagent` (no nesting) — this also fixes a
  hang: a child that called it waited forever, since nothing received its
  requests.

## 3. Fallback models

- `[model] fallback_models = ["…", "…"]` (up to three). When a request fails
  after its retries with an overloaded / rate-limit / unavailable /
  unknown-model error, the same turn is retried on the next model; the
  transcript notes the switch. The session returns to its own model on the
  next turn.
- `/model` docs and `dcode-ai doctor` mention it.
