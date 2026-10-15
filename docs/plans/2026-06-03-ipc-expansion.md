# IPC expansion plan

Date: 2026-06-03

## Goal

Make `attach`, `status`, and `cancel` easier to automate by giving JSON callers
stable error payloads and by documenting the NDJSON event stream schema used by
IPC, event logs, and `--stream ndjson`.

## Current state

- Runtime IPC uses Unix sockets and newline-delimited JSON.
- `EventEnvelope` already carries `schema_version`, `id`, `ts`, and tagged
  `AgentEvent`.
- `attach --json` prints event envelopes, but error cases bubble as human
  `anyhow` messages.
- `status --json` and `cancel --json` return machine-readable success payloads,
  but missing/corrupt session failures are not machine-readable.

## Scope

- Add a stable command error shape:
  `{ "error": { "code": "...", "message": "...", "session_id": "..." } }`.
- Use `session_not_found` for missing `attach`, `status`, and `cancel` session
  files.
- Keep command exits nonzero on JSON errors.
- Document the NDJSON envelope and IPC commands.

## Out of scope

- Windows IPC replacement.
- Query-style IPC request/response redesign.
- Changing the event envelope schema version.
