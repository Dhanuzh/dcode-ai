# Performance benchmark plan

Date: 2026-06-03

## Goal

Add repeatable performance checks for the hot paths that affect terminal UX and
automation speed.

## Current state

- `dcode-ai-runtime` already has a Criterion benchmark for token counting and
  context estimation.
- `scripts/bench/hyperfine_baseline.sh` measures process-level CLI startup and
  common JSON/human commands.
- TUI transcript rendering and event-log replay do not have dedicated benchmark
  coverage yet.

## Scope

- Add CLI Criterion benchmarks for:
  - transcript rendering over many user/assistant/tool blocks;
  - replaying a large event log into TUI state.
- Keep benchmarks out of production behavior.
- Add docs explaining how to run Criterion and hyperfine baselines.

## Out of scope

- Optimizing measured slow paths before we have baseline numbers.
- Adding CI performance thresholds.
