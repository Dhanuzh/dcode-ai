# M29 — Credit guards, honest numbers, release safety (v1.0.29)

Status: implemented

The 1.0.27 compaction loop showed three gaps: nothing stops a runaway turn,
the numbers shown don't match what a plan user pays, and nothing ran a long
session before release. A look at the affected session also showed where
tokens go: 758 browser calls, each returning a full page snapshot
(≈3–4k tokens), and a Claude-side prompt ~2.5× dcode-ai's own count
(Claude keeps its earlier thinking and counts with its own tokenizer).
MCP tools were already deferred behind `tool_search` (threshold 15), so
they were not the cause.

## 1. Lean fixed prompt + measurement

- MCP tools: defer from 6 tools instead of 15 (each schema is sent with
  every request until activated).
- `/context` splits the prompt into system prompt, tool definitions,
  skills, summary, conversation, and "provider extra" (what the provider
  counted above dcode-ai's estimate: thinking it keeps, tokenizer, its own
  framing).

## 2. Browser results by difference

- Stable element refs: an element keeps its number while the page lives
  (`window.__dcodeNext` counter; refs survive re-snapshots). Before this,
  refs were renumbered every snapshot.
- After click / type+submit / press / scroll / back / wait_for on the same
  page, the result lists only what changed (new and removed elements, new
  text lines) instead of the whole page. Navigation to another URL, or an
  explicit `snapshot`, gives the full page.

## 3. Cap big tool output

Tool outputs over 16k characters (was 24k) keep head and tail in the conversation;
the full text is written to `.dcode-ai/tool-output/<call>.txt` and the
result says how to read the rest with `read_file` ranges.

## 4. Burn alarm

A turn stops and tells the user when:
- it has processed more tokens than `[session] turn_token_alarm`
  (default 5M; 0 = off), or
- the same tool call (name + input) repeats 4 times in a row.
The user can send "continue" to go on.

## 5. Plan-aware numbers

Claude through the `claude` binary and Copilot don't bill per token: show
tokens and hide dollar estimates there (footer, dashboard, `/cost`,
`/usage`, turn summary). Dollars stay for the API backend.

## 6. `/context` breakdown

See 1.

## 7. Long-session release gate

- Supervisor-level test: a scripted provider runs many tool steps with a
  large reported prompt; assert compactions are rare, one summary at most,
  conversation kept, no runaway.
- The release workflow runs the workspace tests before building.

## 8. Windows in CI

Already in place: `ci.yml` runs clippy and tests on `windows-latest` and
`macos-latest`, and the release workflow runs the tests before building.

## Found while building the gate (7)

The long-session test showed compaction twice per turn: the mid-turn
check counted chars/4 while the supervisor used the BPE tokenizer, and the
re-arm compared against the measured size (which always carries the fixed
part). Now one estimator (passed to the agent's `Compactor`), re-arm on
conversation growth of a fifth of the window, no post-turn compaction
after a mid-turn one, and retention at a fifth of the window.

## 9. `/stats` across sessions

Each finished turn appends `{date, workspace, model, tokens}` to
`~/.dcode-ai/usage.jsonl`; `/stats` shows tokens per day (last 14 days)
and per project.

## 10. Browser in `/rewind`

The page URL is recorded with each turn. Rewinding code + conversation
also opens the page that was open then (when the browser is connected).
Actions already sent to a site (a submitted form) can't be undone; the
note says so.
