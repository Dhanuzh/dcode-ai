# M31 — Context window and automatic compaction, reworked (v1.0.31)

Status: implemented

## What goes wrong today

Evidence from three real sessions on 1.0.28 (ZeO_Release_Jira_Dashboard,
Claude Code backend, Sonnet / Opus):

| session | tool calls | compaction events | notes |
|---|---|---|---|
| …6078223066 | 306 | **374** | compacting nearly every step; footer warned "90% /compact" anyway |
| …6123849498 | 346 | 16 | half of them manual `/compact` at 11k–42k tokens |
| …3433658200 | 184 | 15 | manual `/compact` at 14k–36k |

1.0.29/30 fix the loop (one estimator, re-arm on growth, burn alarm), but
the design underneath still differs from every other CLI in ways that
cost credit and confuse:

1. **Two different windows.** The footer measures against the model's
   window (200k). Compaction measures against a shrunken target
   (200k − output reserve − 40k overhead, floored at half = ~100k) and then
   fires at 70% of *that* (~70k). So it compacts at ~35% of the real
   window while the footer shows plenty left, or the footer shows "100%
   used" from the measured size while compaction thinks it's fine.
2. **Compacts early and often.** Other CLIs compact once, late, and free a
   lot. Every dcode-ai compaction on the Claude Code backend also restarts
   the `claude` process, which re-sends (re-caches) the whole prompt, so
   frequent small compactions cost more than they save.
3. **Estimates instead of measurements.** Decisions mix a local
   tokenizer estimate with the provider's count. The provider's count (what
   is actually billed and limited) is known after every request.
4. **No cheap step before the expensive one.** Old tool outputs (file
   reads, browser pages, command output) are most of a long session, but
   they are only trimmed at 100% of the target, after summarizing failed to
   help.
5. **Users compact by hand** because the gauge nags ("context N% used ·
   /compact") when auto-compaction would have handled it.

## How others do it

| | trigger | before summarizing | after compaction the history holds | other |
|---|---|---|---|---|
| **Claude Code** | context ≈ window − buffer (~167k of 200k; buffer ~33k reserved for the summary) | micro-compaction: older tool results replaced by a stub, the last ~5 kept | summary + re-read recently used files (token-capped) + project instructions re-injected | footer "Context left until auto-compact: N%"; stops after 3 failed attempts; `/compact <focus>`; PreCompact hook |
| **Codex** | model's `auto_compact_token_limit` (pre-turn / post-turn) | — | summary (prefixed, framed as a hand-off) + the **real user messages**, newest first, up to 20k tokens + fresh initial context | on context-overflow during compaction, drops the oldest item and retries |
| **Gemini CLI** | 50–70% of the model limit | large tool outputs truncated | `<state_snapshot>` (goal, knowledge, file state, plan) + the last 30% of history | a compression that makes the history bigger is rejected |
| **opencode** | on overflow (usage > window − output) | prune: keep the newest 40k tokens of tool output, clear older ones if that frees > 20k | summary checkpoint | retries overflow once per step |

Common ground: **one window, measured usage, compact late, prune tool
output first, keep the user's own words, refuse a compaction that doesn't
shrink, stop after repeated failures, and tell the user how far auto-compact
is.**

## Design

### 1. One window, measured usage

- `ContextBudget { window, output_reserve, buffer }` per model, from the
  model table (`[1m]` models keep 1M; others the 200k working window):
  - `window`: the model's real input window.
  - `output_reserve`: the max output tokens requested.
  - `buffer`: room for the summary request itself (15% of window, ≥ 20k).
- **Used** = the provider's prompt count for the last request (input +
  cache reads + cache writes). The local estimate is used only before the
  first response, and to price what a step adds since.
- The footer, `/context`, the dashboard rail and the triggers all read the
  same `ContextBudget` + used value.

### 2. Two stages, late

- **Auto-compact point** = `window − output_reserve − buffer` (≈ 80–85% of
  the window: 200k Sonnet → ~139k usable with 64k output; capped so it is
  never below 60% of the window).
- At **Prune point = auto-compact point − 10% of window**, run the cheap
  stage once:
  - **Prune** (no model call): replace old tool outputs with a one-line
    stub (`[read_file src/a.rs: 412 lines — cleared; read it again if
    needed]`), keeping the newest 40k tokens of tool output and every
    output the model is still acting on (current turn). Browser pages and
    command output first, file reads after. Done only if it frees ≥ 20k.
- At the auto-compact point, the full stage:
  - **Summarize** with the utility model (Haiku / `compact_model`).
  - New history (Codex + Claude Code): system prompt · summary framed as a
    hand-off ("summary of the earlier conversation; continue the task") ·
    the user's own recent messages (newest first, ≤ 20k tokens) · the
    current plan / todo list · the list of files changed so far and the
    last file read (paths, not content) · the current turn's tail (the
    unanswered tool call results, so the turn can continue).
  - Target after compaction: ≤ 35% of the window.
- Pre-turn, mid-turn (between steps) and on a context-overflow error use
  the same function; mid-turn at most once per turn (as now).

### 3. Guards

- **Refuse growth** (Gemini): if the compacted history isn't at least 30%
  smaller (estimate), keep the old one and count a failure.
- **Circuit breaker** (Claude Code): after 3 consecutive failed or
  ineffective compactions, stop auto-compacting for the session and say so
  ("auto-compact paused — /compact to try again").
- **Overflow retry** (Codex/opencode): on a context-limit error, prune +
  compact once; if the summary request itself overflows, drop the oldest
  non-system message and retry, up to 3 times.
- The burn alarm and the one-summary invariant from 1.0.29/30 stay.

### 4. Claude Code backend

- Prune and compaction both rewrite history, so the `claude` process
  restarts once per stage (the conversation is re-cached). Doing them late
  and rarely is what keeps this cheap; `/usage` already shows restarts by
  reason.
- Its prompt includes thinking it keeps; measured usage covers it, so
  triggers on measured usage handle it without guessing.

### 5. Telling the user

- Footer: `N% context` normally; from 70%: `N% · auto-compact at M%`
  (no "/compact" nag); while compacting: the working line says
  "Compacting conversation…".
- After compaction one transcript line:
  `⎿ Compacted: 148k → 41k tokens (summary + your last 6 messages + plan)`.
- `/context` shows the auto-compact point and the prune point on its grid.
- `/compact [focus]` stays; `/compact --preview` shows what would be kept.

### 6. Config

```toml
[memory.context]
auto_compact = true          # replaces enable_auto_summarize (alias kept)
auto_compact_at = 0          # 0 = automatic (window − output − buffer); else a % of the window
prune_tool_outputs = true
keep_user_messages_tokens = 20000
```
`auto_summarize_threshold`, `context_window_target`, `max_retained_messages`
keep working as overrides with a deprecation note in `/doctor`.

## As built

- `ContextBudget` (runtime `context_manager.rs`): window, reply reserve
  (≤ 32k), summary buffer (7.5%, ≥ 13k); compact point clamped to 60–90%;
  prune point 10% lower; `hard_limit` for last-resort trimming.
- Usage: `AgentLoop::context_used()` = last measured prompt + estimate of
  what was added since; local estimate (history + tool list) before the
  first request or right after a rewrite.
- Clearing old tool output turned out to be expensive when done often:
  every rewrite of old history makes the provider re-read the whole prompt
  without its cache (and restarts the `claude` process). The first build
  pruned at the prune point and did it ~14 times in the test session. Now
  it runs only at the compaction point (instead of a summary when that is
  enough), at most once between compactions, or pre-turn when the session
  was idle ≥ 5 minutes (cache already cold), like Claude Code's
  micro-compaction.
- Summary layout in `apply_summary`: hand-off line, summary, the user's
  recent messages verbatim (≤ 20k tokens), files changed; recent tail 15%
  of the window.
- Guards: `compaction_worked` (≥ 30% smaller), shared failure counter
  (`MAX_COMPACTION_FAILURES` = 3), prune before the overflow retry.
- Post-turn compaction removed: the next turn's pre-turn check covers it.
- Test session (400 steps, ~2k tokens each, 200k window, 15k fixed, growing
  provider-side extra): 5 compactions (~153k → ~49k), 5 clearings, no
  failures, no pause.

## Tests (release gate)

- Simulation on the real supervisor with a scripted provider reporting
  realistic usage (200k window, 15k fixed, Claude-side extra growing with
  thinking): over 400 steps compaction runs ≤ 10 times, ≥ 20 steps apart,
  clearing at most once between compactions and before the first one, the user's last
  messages survive, at most one summary, no burn-alarm pause.
- Unit: budget math per model (200k, 1M, Copilot models from `/models`),
  prune keeps the newest 40k of tool output and the current turn, refuse a
  compaction that doesn't shrink, circuit breaker after 3, overflow retry
  drops oldest.
- PTY test: footer shows `auto-compact at` past 70%.

## Rollout

1. Budget + measured usage + footer/`/context` (no behaviour change yet).
2. Prune stage.
3. New compaction layout (summary + user messages + plan + files).
4. Guards (refuse growth, circuit breaker, overflow retry).
5. Remove the old target/threshold path behind the new config; simulation
   gate in CI; docs and CHANGELOG.

## Sources

- Codex `codex-rs/core/src/compact.rs` (COMPACT_USER_MESSAGE_MAX_TOKENS,
  summary prefix, overflow retry).
- Gemini CLI `chatCompressionService.ts` (via analyses):
  https://lin-guanguo.github.io/llm-memory-research/gemini-cli.research/
- opencode compaction: https://opencode.ai/v2/docs/compaction,
  https://lin-guanguo.github.io/llm-memory-research/opencode.research/
- Claude Code: https://claudelog.com/faqs/what-is-auto-compact-buffer/,
  https://barazany.dev/blog/claude-codes-compaction-engine,
  https://morphllm.com/claude-code-compact
