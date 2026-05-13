# Startup One-Time Approval (2026-05-12)

## Problem

Users want a single approval decision at session start instead of repeated
tool approvals, while keeping current behavior when they deny.

## Goal

Add an optional startup prompt that asks once:
- Approve all tools for this session.
- If approved, allow all tools for the current session.
- If denied, keep existing permission flow unchanged.

## Plan

1. Extend permission config:
   - Add `permissions.startup_approve_all` (default `false`).
2. Add CLI overrides:
   - `--startup-approve-all`
   - `--no-startup-approve-all`
3. Add runtime hook method:
   - SessionRuntime method to add a session allow pattern.
4. Add startup prompt flow in CLI entrypoints for interactive starts:
   - Offer one-time approval before first turn/REPL loop.
   - On approve: add session allow `"*"`.
   - On deny/abort: no changes (current behavior).
5. Validate:
   - `cargo fmt --all -- --check`
   - `cargo test -p dcode-ai-core -p dcode-ai-runtime -p dcode-ai-cli`

