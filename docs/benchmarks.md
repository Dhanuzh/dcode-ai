# Benchmarks

Use these commands to capture local performance baselines before and after a
change.

## Criterion benches

Token counting and context budgeting:

```bash
cargo bench -p dcode-ai-runtime --bench token_count
```

TUI transcript rendering and event-log replay:

```bash
cargo bench -p dcode-ai-cli --bench tui_perf
```

Criterion writes reports under `target/criterion/`.

## CLI startup and command latency

The hyperfine script measures release-binary startup and common noninteractive
commands:

```bash
scripts/bench/hyperfine_baseline.sh
```

Dry-run the command list without executing:

```bash
scripts/bench/hyperfine_baseline.sh --dry-run
```

Override run count and output directory:

```bash
RUNS=50 WARMUP=5 OUT_DIR=docs/benchmarks scripts/bench/hyperfine_baseline.sh
```
