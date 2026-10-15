# M9 — Language servers, permission policy + sandbox, verifier

Date: 2026-10-14 · Branch: `feat/claude-copilot` · Status: ✅ done (verifier and policy live-tested; Seatbelt profile unit-tested, macOS not available here) · Parent: [v1 master plan](2026-10-12-v1-master-plan.md)

Remaining roadmap items after M8f ("doit").

## M5 — Language servers for code intelligence
- Today `code_intel` (definitions, references, symbols, diagnostics) always
  starts `rust-analyzer`, whatever the project is.
- Servers are now chosen by project: built-ins for Rust (`rust-analyzer`),
  TypeScript/JavaScript (`typescript-language-server --stdio`), Python
  (`pyright-langserver --stdio`), Go (`gopls`), C/C++ (`clangd`); plus
  `[[code_intel.lsp]]` in config and plugin `lspServers` (Claude Code
  format: `command`, `args`, `extensionToLanguage`). The language with the
  most files in the workspace wins; missing binaries fall back to the fast
  local (ripgrep-style) index as before.

## M3 — Permission policy file + macOS sandbox
- `policy.toml` (`~/.dcode-ai/policy.toml`, `.dcode-ai/policy.toml`): `allow`,
  `ask`, `deny` rule lists (`tool:pattern`, e.g. `execute_bash:git push*`)
  merged into permissions; deny wins.
- `dcode-ai policy test <tool> [input]` prints the decision and the rule
  that produced it.
- `sandbox_bash` on macOS wraps shell commands in `sandbox-exec` with a
  generated Seatbelt profile (write only under the workspace, temp dirs and
  configured roots; reads everywhere) — the Linux Landlock equivalent.

## M6 — Verifier
- `/verify` (and `[session] verify_after_edits = true`) runs a read-only
  verifier sub-agent over the uncommitted diff: it reads the changes, runs
  the project's checks through `run_validation`, and reports problems; the
  report is added to the conversation so the main agent can act on it.

## Verification
Unit tests for server selection, plugin `lspServers` parsing, policy
merging/precedence and `policy test`, Seatbelt profile generation, verifier
prompt/tool set. Live: code intel with a non-Rust workspace, `/verify`.
