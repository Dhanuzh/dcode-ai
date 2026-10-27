# M26 — credit savings, crash-safe sessions, browser wait/replay, UI (v1.0.26)

Status: implemented

## Credit (measured on the user's project)
- **Skills list was 15k of a 20k-char system prompt**, sent every request.
  Now one line per skill (description ≤160 chars, settings only when not
  default) within a 6k budget, the rest by name: 20.2k → 10.8k chars
  (~2.4k tokens less per request).
- **Rarely used built-ins deferred** behind `tool_search` (`browser`,
  `notebook_edit`, `inspect_binary`, `copy/move/rename_path`,
  `query_symbols`); one `tool_search` covers built-ins and MCP (a second
  deferral no longer drops the first).
- **Compaction** starts at 70% (was 75%) of the 200k working window, on the
  cheap model (`compact_model`, else Haiku on Claude); the summary keeps a
  Files section.
- **`/usage`** splits fresh input / cache reads / cache writes / output,
  shows the cache hit rate and how many `claude` processes were started
  (restarts re-send the conversation). The turn line counts every processed
  token, cache included.

## Reliability
- **Crash-safe sessions**: `AgentLoop::checkpoint` saves the session after
  every tool step (sync, atomic temp + rename, so an older step can't land
  after a newer save).

## Browser
- `wait_for {text, gone?, timeout_ms?}` (title or body text).
- Every page-changing action is recorded (clicks/typing by `role "name"`,
  not by number); `steps` lists them, `/browser save [name]` writes
  `.dcode-ai/browser-steps/<name>.json`, `replay {file}` runs them again,
  finding each element anew.
- Screenshots are attached to the conversation for providers that take
  images (API backend, Copilot), so the model sees them.

## UI
- Approval box shows the edit as a line-numbered diff of the real file
  (diff-view renderer); falls back to the plain preview.
- `/resume` lists forks and sub-agent sessions indented under their parent.
- `[ui] working_style = "bar" | "dots" | "plain"`, `[ui] shimmer = false`.
- `/keymap` remaps apply in the full-screen diff view too (other prompts
  already went through the keymap).
