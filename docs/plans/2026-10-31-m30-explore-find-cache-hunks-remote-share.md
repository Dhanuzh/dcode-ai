# M30 — Explore agents, browser find, cache report, hunks, remote, sharing, verified security findings (v1.0.30)

Status: implemented

## 1. Cheap explore sub-agents

`spawn_subagent {explore: true}`: read-only research (search, read,
summarize). The child runs in plan mode (read-only tools), without a
worktree, on the utility model (`[model] compact_model`, else Haiku on
Claude). An agent definition's own model still wins.

## 2. Browser `find`

`browser {action: "find", query}`: elements and text lines on the page that
contain the query (case-insensitive), with their refs, instead of the whole
page. Searches up to 1,000 elements and the full text, not only what fits a
snapshot.

## 3. Prompt cache report

`/usage` lists why the provider had to re-read the conversation without
its cache:
- Claude Code backend: process restarts by reason (system prompt or model
  changed, tool schema changed, history rewritten, process ended).
- Every backend: "cold" requests (cache writes but no cache reads) and the
  tokens they wrote.

## 4. Per-hunk approval for every edit tool

`h` in an approval already chooses hunks, but only for `write_file`; for
`edit_file` a partial choice applied the whole edit. Now:
- hunks for `write_file` (content) and `edit_file` (old_text → new_text,
  `replace_all`), paths resolved against the workspace;
- a partial choice is sent as the final file content, run as `write_file`
  (same workspace checks), and the tool result tells the model which part
  the user kept;
- tools where hunks can't be computed don't offer `h`.

## 5. Terminal UI tests in a pseudo-terminal

An integration test (`crates/cli/tests/tui_pty.rs`) starts the real
`dcode-ai` binary in a PTY with a temporary HOME and a dummy API config
(nothing is submitted, so nothing reaches a model), answers the terminal's
cursor-position queries, and checks the screen: welcome box, typing,
a multi-line paste staying in the composer, Shift+Tab cycling modes,
`Ctrl+\` dashboard, quitting with Ctrl+C. Runs in CI on Linux and macOS.

## 6. Attach from another machine

- `dcode-ai remote <host> [--dir path] [--keep] [-- args]` runs
  `ssh -t -- host 'cd dir && dcode-ai args'`; `--keep` wraps it in
  `tmux new-session -A -s dcode-ai-<dir>` so it survives disconnects and the
  same command re-attaches. `remote host -- sessions` lists sessions there.
- `dcode-ai attach <id> --ssh host [--dir path]` follows a server session.
Nothing listens on the network; SSH carries everything. A host starting
with `-` is refused (ssh would read it as an option).

## 7. Share a session with a teammate

`dcode-ai session export <id> [-o file]` writes one `.dcode-session` file
(state + event log) with secrets masked; `dcode-ai session import <file>`
adds it to this workspace's sessions (new id, workspace path rewritten) so
`/resume` finds it.

## 8. From Shannon (AGPL — ideas only, no code)

Shannon's rule is "no exploit, no report": a finding counts only once it
is demonstrated. For dcode-ai's defensive review of the user's own code:
- `/security-review` asks the reviewer to verify each finding against the
  code (the exact path from input to sink) and to label it **verified** or
  **unverified**; unverified ones are listed separately, not mixed in.
- `dcode-ai ci --sarif <file>` writes findings as SARIF 2.1.0 for GitHub
  code scanning, and `--fail-on <severity>` fails the job only for
  verified findings at or above that severity.
Live exploitation of running systems is out of scope.
