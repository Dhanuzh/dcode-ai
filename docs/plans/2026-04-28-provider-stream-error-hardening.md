# Provider Stream Error Hardening

## Goal

Make provider streaming failures fail loudly instead of being rendered as assistant text.

## Problem

- OpenAI-compatible and Anthropic-compatible stream readers currently turn byte-stream read errors into `TextDelta` chunks.
- The agent loop can then treat a provider transport failure as successful assistant output.
- This is unsafe for automation and hides the real failure from users.

## Plan

- Add a `StreamChunk::Error` variant to represent provider stream failures.
- Emit `StreamChunk::Error` from provider stream readers on transport errors.
- Teach `AgentLoop` to emit an `AgentEvent::Error` and return `ProviderError::RequestFailed`.
- Add a focused agent-loop test proving stream errors do not become successful assistant text.

## Validation

- `cargo test -p dcode-ai-core provider_stream_error`
- `cargo test -p dcode-ai-core openai_provider_streams_text_tool_and_usage`
- `cargo test -p dcode-ai-core anthropic_provider_streams_text_tool_and_usage`
