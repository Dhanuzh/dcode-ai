# Orchestration Contract

This document defines the public subprocess contract for running `dcode-ai` under an external orchestrator.

The goal is to make `dcode-ai` usable as a headless worker without coupling the project to any one control plane.

## Supported Commands

These commands are the supported orchestration-facing surfaces:

| Command | Purpose | Machine-readable output |
|---|---|---|
| `dcode-ai run --prompt ... --stream off --json` | Run a foreground task and return a final result | JSON object |
| `dcode-ai run --prompt ... --stream ndjson` | Run a foreground task and stream live events | NDJSON `EventEnvelope` lines |
| `dcode-ai spawn --prompt ... --json` | Start a detached session | JSON object |
| `dcode-ai status <session_id> --json` | Read the current saved session snapshot | JSON object |
| `dcode-ai sessions --json` | List known sessions | JSON object |
| `dcode-ai attach <session_id>` | Stream live event envelopes from IPC or fall back to the event log | NDJSON `EventEnvelope` lines |
| `dcode-ai logs <session_id>` | Replay persisted event envelopes from disk | NDJSON `EventEnvelope` lines |
| `dcode-ai cancel <session_id> --json` | Stop a session and persist cancelled state | JSON object |

`dcode-ai serve` exists for long-lived IPC-driven sessions but is treated as an internal command rather than part of the public orchestration contract.

## Event Stream Shape

Machine event streams use the same envelope shape on stdout, in IPC, and in `.dcode-ai/sessions/<session-id>.events.jsonl`:

```json
{
  "id": 12,
  "ts": "2026-03-14T08:00:00Z",
  "event": {
    "type": "ToolCallStarted",
    "call_id": "call_123",
    "tool": "read_file",
    "input": {
      "path": "src/main.rs"
    }
  }
}
```

The `event` payload is the tagged `AgentEvent` enum from `crates/common/src/event.rs`.

Lifecycle-critical events:

- `SessionStarted`
- `MessageReceived`
- `ToolCallStarted`
- `ToolCallCompleted`
- `ApprovalRequested`
- `ApprovalResolved`
- `QuestionRequested` (interactive `ask_question` tool; includes `suggested_answer`)
- `QuestionResolved`
- `Checkpoint`
- `Response`
- `SessionEnded`
- `ChildSessionSpawned`
- `ChildSessionCompleted`

### Answering `QuestionRequested` over IPC

When the model uses the `ask_question` tool, the runtime emits `QuestionRequested` with a `question_id`. Send a newline-delimited JSON command on the session socket:

```json
{"type":"AnswerQuestion","question_id":"q-<call-id>","selection":{"kind":"suggested"}}
```

`selection.kind` may be `suggested`, `option` (with `option_id`), or `custom` (with `text`). In the interactive CLI, `/auto-answer` accepts the suggested answer for the active question.

## Session Snapshot Shape

`status --json`, `sessions --json`, and the final `run --json` output are built around the shared `SessionSnapshot` shape from `crates/common/src/session.rs`.

Important fields:

- `id`
- `status`
- `workspace`
- `model`
- `pid`
- `socket_path`
- `updated_at`
- `estimated_cost_usd`
- `total_input_tokens`
- `total_output_tokens`
- `orchestration`

The `orchestration` field is optional and only appears when the run was launched with `DCODE_AI_ORCH_*` metadata.

## Command Outputs

### `run --stream off --json`

Returns:

```json
{
  "session": {
    "id": "session-123",
    "status": "completed"
  },
  "output": "final assistant text",
  "end_reason": "completed"
}
```

### `spawn --json`

Returns:

```json
{
  "session_id": "session-123",
  "pid": 4242,
  "status_path": ".dcode-ai/sessions/session-123.json",
  "event_log_path": ".dcode-ai/sessions/session-123.events.jsonl",
  "spawn_log_path": ".dcode-ai/sessions/session-123.spawn.log",
  "socket_path": "/tmp/dcode-ai/session-123.sock",
  "permission_mode": "bypass-permissions",
  "safe_mode": false
}
```

### `sessions --json`

Returns:

```json
{
  "sessions": [
    {
      "id": "session-newer",
      "status": "running"
    }
  ],
  "unreadable": []
}
```

### `cancel --json`

Returns:

```json
{
  "session": {
    "id": "session-123",
    "status": "cancelled"
  },
  "cancelled": true
}
```

## Headless Permission Guidance

For orchestrated runs, prefer one of these modes:

- `--permission-mode dont-ask`: read-only headless execution
- `--permission-mode bypass-permissions`: fully autonomous execution

Avoid `default` and `accept-edits` for unattended subprocess runs unless the orchestrator is prepared for approval failures.

If a headless run reaches a tool call that would require user approval, `dcode-ai` exits with a dedicated approval-blocked exit code instead of waiting indefinitely.

## Exit Codes

These exit codes are intended to stay stable for orchestrators:

| Code | Meaning |
|---|---|
| `0` | Success |
| `1` | Unclassified/internal failure |
| `10` | Configuration failure |
| `11` | Runtime/provider/tool failure |
| `13` | Approval-blocked headless run |
| `130` | Cancelled run |

## Orchestration Metadata Environment Contract

`dcode-ai` reads optional orchestration metadata from these environment variables:

| Variable | Meaning |
|---|---|
| `DCODE_AI_ORCH_NAME` | Orchestrator name |
| `DCODE_AI_ORCH_RUN_ID` | Current external run identifier |
| `DCODE_AI_ORCH_TASK_ID` | Current task identifier |
| `DCODE_AI_ORCH_TASK_REF` | Human-readable task reference |
| `DCODE_AI_ORCH_PARENT_RUN_ID` | Parent external run identifier |
| `DCODE_AI_ORCH_CALLBACK_URL` | Callback or control endpoint hint |
| `DCODE_AI_ORCH_META_<KEY>` | Free-form metadata entries |

This metadata is persisted into session state and injected into the layered system prompt as coordination context. It does not create any implicit network behavior by itself.

## Wrapper Example

Example subprocess flow for a Paperclip-like orchestrator:

1. Export headless context:
   `DCODE_AI_ORCH_NAME=paperclip-wrapper`
   `DCODE_AI_ORCH_RUN_ID=<run-id>`
   `DCODE_AI_ORCH_TASK_ID=<task-id>`
2. Launch:
   `dcode-ai run --prompt "$PROMPT" --stream off --json --permission-mode bypass-permissions`
3. Parse the final JSON output and persisted `session.id`.
4. If live progress is needed, use:
   `dcode-ai run --prompt "$PROMPT" --stream ndjson --permission-mode bypass-permissions`
   or `dcode-ai attach <session_id>`.

## Compatibility Roadmap

This subprocess contract is the first compatibility layer.

Planned later layers:

- formal local IPC API over the existing Unix socket
- optional HTTP/SSE or A2A-style adapter on top of `runtime + common`
- orchestrator-specific wrappers only after the generic contract is stable
