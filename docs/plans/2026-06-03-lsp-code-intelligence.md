# LSP code intelligence plan

Date: 2026-06-03

## Goal

Move `code_intel` from a single heuristic symbol lookup toward an LSP-capable
boundary that can support symbols, definitions, references, and diagnostics.

## Current state

- `query_symbols` uses local ripgrep over Rust definition patterns.
- `LanguageServerCodeIntel` needs a real `rust-analyzer` JSON-RPC transport.
- The tool surface exposes only `query_symbols`.

## Scope

- Add provider-agnostic code-intelligence records for locations and diagnostics.
- Extend the `CodeIntel` trait with:
  - `goto_definition`;
  - `find_references`;
  - `diagnostics`.
- Wire `LanguageServerCodeIntel` to a real `rust-analyzer` JSON-RPC transport
  for workspace symbols, definitions, references, and publish diagnostics.
- Use `WorkspaceCodeIntel` to prefer LSP and fall back to fast local lookup when
  `rust-analyzer` is unavailable.
- Enhance `FastLocalCodeIntel` as the fallback:
  - definitions reuse symbol lookup;
  - references search literal identifiers;
  - diagnostics parse common Rust compiler error lines from text files/logs.

## Out of scope

- Long-lived `rust-analyzer` process pooling.
- TUI rendering changes for code-intel results.
