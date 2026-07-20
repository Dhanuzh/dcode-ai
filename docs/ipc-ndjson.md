# IPC and NDJSON reference

`dcode-ai` uses newline-delimited JSON for event logs and `attach --json`
output. Unix uses Unix domain sockets; Windows uses loopback TCP.

## Socket wire framing (v2)

Runtime IPC **sockets** now use length-prefixed frames instead of raw NDJSON:
each message is a 4-byte big-endian payload length followed by exactly that
many bytes of JSON (the same `EventEnvelope` / `AgentCommand` documented
below). Frames are capped at 16 MiB.

Compatibility:

- **Readers auto-detect per connection.** Because of the 16 MiB cap, a framed
  stream's first byte is always `0x00`, while NDJSON always starts with `{`.
  Both dcode-ai ends accept either format, so old NDJSON-writing peers keep
  working.
- **External consumers** that parse the socket as NDJSON lines can set
  `DCODE_AI_IPC_LEGACY=1` on the runtime to keep legacy newline-delimited
  writes.
- Event **log files** (`<sid>.events.jsonl`) and `attach --json` stdout remain
  NDJSON — framing applies to the socket transport only.

## Event envelope

Each line is one `EventEnvelope`:

```json
{"schema_version":1,"id":42,"ts":"2026-06-03T12:00:00Z","event":{"type":"TokensStreamed","delta":"hello"}}
```

Fields:

| Field | Type | Meaning |
| --- | --- | --- |
| `schema_version` | integer | Stable envelope schema. Current value: `1`. |
| `id` | integer | Monotonic event id within the session log. |
| `ts` | string or null | RFC 3339 UTC timestamp. |
| `event` | object | Tagged `AgentEvent`; its `type` field selects the payload. |

Common event `type` values include:

| Type | Purpose |
| --- | --- |
| `SessionStarted` | Session id, workspace, and model became active. |
| `MessageReceived` | User or assistant message committed. |
| `TokensStreamed` | Visible assistant text delta. |
| `ThinkingDelta` | Internal reasoning/thinking delta. |
| `ToolCallStarted` | Tool execution requested. |
| `ToolCallCompleted` | Tool execution result returned. |
| `ApprovalRequested` | Tool approval is waiting on a user or IPC command. |
| `ApprovalResolved` | Approval request was accepted or denied. |
| `CostUpdated` | Token/cost totals changed. |
| `SessionEnded` | Session finished, errored, or was cancelled. |
| `Error` | Runtime/provider/tool error. |

Consumers should ignore unknown event fields and unknown event types they do not
handle.

## IPC commands

Commands sent to a running session socket are also one JSON object per line:

```json
{"type":"SendMessage","content":"continue"}
{"type":"ApproveToolCall","call_id":"call-1"}
{"type":"DenyToolCall","call_id":"call-1"}
{"type":"Cancel"}
{"type":"Shutdown"}
```

Interactive question answers use:

```json
{"type":"AnswerQuestion","question_id":"q-1","selection":{"kind":"suggested"}}
```

## CLI automation

Useful machine-readable commands:

```bash
dcode-ai sessions --json
dcode-ai status <session-id> --json
dcode-ai attach <session-id> --json
dcode-ai cancel <session-id> --json
```

JSON error payloads use this shape and still exit nonzero:

```json
{"error":{"code":"session_not_found","message":"session missing: missing","session_id":"missing"}}
```
