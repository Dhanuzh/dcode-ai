# Code health (backlog round 5, items 23–28) — 2026-11-26

Asked by the user: "Code health … 23–28, do it."

## 24. `crates/web-api`
Not in git and not a workspace member: the folder only holds a local,
git-ignored `.env` (may contain secrets). Deleting it is the user's call —
asked, not done.

## 28. Panics in the metric parser
- The 18 `Regex::new(…).unwrap()` become a `DEFAULT_PATTERNS` table compiled
  with `filter_map` (a bad pattern is skipped, never a panic); a test keeps
  every pattern valid.
- `experiment.rs`: the two `expect("stdout captured")` return an error.

## 26. Dependency diet
- Removed 11 direct dependencies nothing uses: `two-face`, `ansi-to-tui`,
  `unicode-segmentation` (cli); `ignore`, `walkdir` (runtime);
  `dcode-ai-common`, `serde_json`, `toml`, `thiserror`, `futures-util`,
  `async-trait` (autoresearch). `cargo-husky` stays (installs the hook).
- Duplicate versions are almost all transitive (iroh, candle, boa, syntect).
  Ours: `reqwest` 0.12 vs iroh's 0.13 — moving changes TLS for every
  request (0.13's rustls needs a crypto provider installed at start and
  uses the OS certificate store instead of bundled roots). Worth doing as
  its own change with a release note, not inside a cleanup. `rand` 0.8:
  `phf` keeps 0.8 in the tree anyway, nothing gained.

## 25. Smaller binary
- `speech` Cargo feature (default on): Whisper/candle moved to
  `voice/whisper.rs`; `--no-default-features` builds without it and
  `/talk` says speech isn't in the build. Voice calls still work.
- Measured (release, Linux x86_64): 55.3 MB with speech, 53.0 MB without
  — candle is only 2.3 MB. The bulk is elsewhere (iroh, boa, rustls,
  syntect, the browser and video code); a `p2p` feature is the next
  candidate.
- Not done: `opt-level = "s"` would slow Whisper on CPU (the user's
  hands-free mode); iroh (`/invite` p2p) is wired through invite/people —
  a `p2p` feature is a larger follow-up.

## 23. Split the biggest files
Test modules are most of the biggest files (jira_sync: 3,384 of 4,608
lines). Move each into its own file (`#[cfg(test)] #[path] mod tests;`);
split `main.rs` by job (session commands, info commands). Note: Rust
compiles a crate as one unit, so this helps reading and review more than
rebuild time.

## 27. Startup time
Measured in a PTY (time to the composer): 4.4 s (release). The log showed
the first frame waiting for every MCP server (`loaded 316 MCP tool(s)`
just before it): startup waited up to `MCP_STARTUP_WAIT` (3 s) so fast
servers' tools were in the first turn.
Now startup doesn't wait; the first turn waits what's left of the 3 s
(`mcp_wait_until`) — by then the user has usually typed for longer.
After: 1.5–1.7 s on a *debug* build.

## Result of 23
- Tests out of: `core/agent.rs` (→ `agent_tests.rs`), `core/video/render.rs`,
  `runtime/supervisor/mod.rs` (→ `tests.rs`), `cli/repl.rs`,
  `cli/tui/state/mod.rs`.
- `jira_sync.rs` (4,608) → `jira_sync/` mod.rs 1,201 + parse, fields,
  transitions, report, fake, tests.
- `main.rs` (3,862) → 2,509 + `cmd/sessions.rs`, `cmd/worktrees.rs`,
  `cmd/info.rs`.
