# Product hardening roadmap

Date: 2026-06-03

## Goal

Raise `dcode-ai` from a capable Rust-native terminal agent to a more reliable
daily-driver CLI by hardening MiniMax/default-provider behavior, making failure
paths explicit, and improving automation surfaces without changing the single
binary product shape.

## Constraints

- Keep the shipped surface Rust-native and terminal-first.
- Treat MiniMax M2.5 / OpenCode Zen as the primary default provider.
- Fail loudly for empty provider completions; never turn an empty model response
  into a successful turn.
- Preserve crate boundaries: shared types in `common`, agent/provider logic in
  `core`, lifecycle/IPC/worktrees in `runtime`, UX in `cli`.
- Prefer machine-readable CLI behavior for automation surfaces.

## Phase 1: provider and MiniMax hardening

- Strengthen the agent-level empty-completion guard so any provider turn with no
  assistant text and no tool calls fails with a stable, explicit error.
- Retry empty provider completions the same way regardless of whether the stream
  reported token usage; usage reporting is not a prerequisite for retry.
- Add regression tests for provider streams that end with only `Done`, only
  usage, or reasoning-only content.
- Label MiniMax streams distinctly instead of letting OpenAI-compatible errors
  appear as generic `openai` stream failures.
- Expand OpenAI-compatible stream parsing for MiniMax-like deltas where tool
  calls arrive as partial arguments or nonstandard reasoning keys.

## Phase 2: code intelligence

- Keep the current heuristic code map as a fallback.
- Introduce an optional LSP client boundary in `core` that can query symbols,
  definitions, references, and diagnostics.
- Expose LSP-backed results through existing read-only tool surfaces before
  changing prompts or TUI behavior.

## Phase 3: worktree management

- Add CLI commands for worktree listing, pruning, merging, and session-linked
  cleanup.
- Surface parent/child lineage, branch, and worktree paths in session status.
- Keep destructive cleanup explicit and testable.

## Phase 4: IPC and machine output

- Document the NDJSON event envelope and command request/response shapes.
- Stabilize machine-readable error fields for `attach`, `status`, and `cancel`.
- Add tests for event-log fallback and socket command failure cases.

## Phase 5: performance and compaction UX

- Benchmark transcript replay, TUI render helpers, token counting, and large
  event logs.
- Add `/compact --preview` so users can inspect preserved artifacts before the
  compaction rewrite happens.
- Reuse the existing preserved-artifacts block as the preview source.

## Phase 6: platform and release hardening

- Gate Unix-only PTY/tmux/Unix-socket code and design a Windows IPC backend
  using named pipes or loopback TCP.
- Improve installer diagnostics and add checksum verification once release
  artifacts publish checksums.
- Extend `/doctor` to report provider readiness, install source, binary version,
  runtime socket path, and platform limitations.

## First implementation slice

This slice starts with Phase 1 because silent provider success is the highest
risk and MiniMax is the default provider path:

1. Make empty completions fail after explicit retries even without usage chunks.
2. Add regression coverage in `core::agent`.
3. Distinguish MiniMax stream labels in provider errors.
4. Run targeted core tests and formatting.
