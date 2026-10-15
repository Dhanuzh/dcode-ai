#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
BIN="${BIN:-$ROOT_DIR/target/release/dcode-ai}"
OUT_DIR="${OUT_DIR:-$ROOT_DIR/docs/benchmarks}"
RUNS="${RUNS:-25}"
WARMUP="${WARMUP:-3}"
DRY_RUN="${1:-}"

mkdir -p "$OUT_DIR"

if [[ ! -x "$BIN" ]]; then
  echo "Building release binary: $BIN"
  cargo build --release -p dcode-ai-cli --manifest-path "$ROOT_DIR/Cargo.toml"
fi

STAMP="$(date +%Y%m%d-%H%M%S)"
OUT_JSON="$OUT_DIR/hyperfine-baseline-$STAMP.json"

CMDS=(
  "$BIN --version"
  "$BIN --help"
  "$BIN config --json"
  "$BIN sessions --json --limit 20"
  "$BIN sessions --limit 20"
)
NAMES=(
  "version"
  "help"
  "config-json"
  "sessions-json"
  "sessions-human"
)

if [[ "$DRY_RUN" == "--dry-run" ]]; then
  echo "Dry run only. Commands:"
  for i in "${!CMDS[@]}"; do
    echo "  [${NAMES[$i]}] ${CMDS[$i]}"
  done
  exit 0
fi

if ! command -v hyperfine >/dev/null 2>&1; then
  echo "hyperfine not found. Install it first (cargo install hyperfine or package manager)."
  exit 1
fi

ARGS=(--warmup "$WARMUP" --runs "$RUNS" --export-json "$OUT_JSON")
for i in "${!CMDS[@]}"; do
  ARGS+=(--command-name "${NAMES[$i]}" "${CMDS[$i]}")
done

hyperfine "${ARGS[@]}"
echo "Saved benchmark JSON: $OUT_JSON"
