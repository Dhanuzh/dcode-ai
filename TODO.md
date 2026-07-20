# dcode-ai Improvement Roadmap

## 1. Onboarding & UX (First 60 Seconds)
- [ ] Auto-detect existing local provider configs (`~/.config`, env vars).
- [ ] Create a "Welcome" onboarding flow for first-time users.
- [ ] Streamline API key entry with validation feedback in the TUI.

## 2. Context & Intelligence
- [ ] Implement semantic context compaction (summarize stale conversation into "Memory Notes").
- [ ] Cache file-mention summaries for large files to reduce redundant reads.
- [ ] Build a workspace-native semantic search tool (Tantivy/SQLite-based).
- [ ] Add git-aware context tool (summary of current branch, diffs, open PRs).

## 3. Reliability & Testing
- [ ] Expand test coverage (86/145 files currently have zero tests).
- [x] Implement property-based testing (proptest) for `ApprovalPolicy` and `wildcard_matches`. (7 properties in `approval.rs`: no-panic on arbitrary unicode, `*` totality, contains-equivalence, prefix/infix acceptance, suggested-pattern self-match, `check()` no-panic across all permission modes.)
- [ ] Build a headless integration test harness for end-to-end agent behavior.

## 4. IPC & Runtime Stability
- [x] Add heartbeat/ping-pong to Unix-socket IPC (CLI/Runtime). (Server sends an empty frame every 15s; clients treat 60s of silence as a dead runtime.)
- [x] Implement length-prefix framing for IPC messages to prevent desync. (4-byte BE prefix, 16 MiB cap, legacy NDJSON auto-detect + `DCODE_AI_IPC_LEGACY=1` escape hatch — see `docs/ipc-ndjson.md`.)
- [x] Improve IPC backpressure handling for large tool outputs. (A lagged broadcast consumer now gets an explicit "N events skipped" notice and stays connected, instead of the connection silently dying; combined with the 24k tool-output cap + 16 MiB frames.)
- [x] Add explicit "Runtime Disconnected" UI alerts. (Client injects a synthetic `Error` envelope on heartbeat stall or unexpected EOF-without-SessionEnded, so attach and the web UI render an explicit alert.)

## 5. Architectural Maintenance
- [ ] Complete monolith splitting for `app.rs` and `repl.rs` (as per `MONOLITH_SPLITTING_PLAN.md`).
- [ ] Finalize transition to structured errors using `thiserror`.
- [ ] Integrate `cargo udeps` into CI.
- [ ] Add `cargo-audit` for dependency vulnerability scanning.
- [ ] Audit `autoresearch` crate: integrate or archive.

## 6. Deployment & Packaging
- [ ] Create a Homebrew tap for macOS installation (`brew install dcode-ai/tap/dcode-ai`).
- [ ] Automate release builds and artifact creation with GitHub Actions.
- [ ] Generate DEB/RPM packages for Linux distributions.
- [ ] Investigate publishing to crates.io.
