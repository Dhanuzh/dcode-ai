# MiniMax native image input

## Goal

First-class image attachments in the agent message pipeline, **MiniMax-first** via the existing Anthropic-compatible `/v1/messages` path. The full-screen TUI supports **Ctrl+V** (clipboard image) when the OS/clipboard allows it, plus **`/image paste`** and **`/image <path>`** as reliable fallbacks.

## Non-goals (this iteration)

- Line-mode REPL clipboard parity (TUI-only for direct paste).
- MCP `understand_image` as a silent fallback when the model does not support native images (we **fail loudly** instead).

## Design

- **`Message.content`** is either legacy plain text (`serde` string) or a JSON array of parts: `text` + `image` (path relative to workspace, MIME type).
- Attachments are stored under **`<workspace>/.dcode-ai/sessions/<session_id>/attachments/`** and referenced by relative path in serialized sessions.
- **MiniMax vision**: same contract as [MiniMax-Coding-Plan-MCP](https://github.com/MiniMax-AI/MiniMax-Coding-Plan-MCP) `understand_image`: `POST {api_origin}/v1/coding_plan/vlm` with `prompt` + `image_url` (base64 data URL). dcode-ai calls this from Rust (no MCP), then replaces the user multimodal message with text (VLM output + user text) before `/v1/messages`.
- **Other providers**: user turns with images use native multimodal JSON where implemented (Anthropic-style blocks; OpenAI-style `image_url` data URLs for OpenAI/OpenRouter when the model is treated as vision-capable).
- **Capability gating**: if the user attaches images and the active provider/model is not considered vision-capable, the turn errors before calling the API.

## Key files

- [`crates/common/src/message.rs`](../../crates/common/src/message.rs) — `MessageContent`, `ContentPart`, `ImageAttachment`
- [`crates/common/src/model_caps.rs`](../../crates/common/src/model_caps.rs) — `model_accepts_native_images`
- [`crates/core/src/provider/anthropic_compat.rs`](../../crates/core/src/provider/anthropic_compat.rs) — multimodal user serialization + base64 from workspace files
- [`crates/core/src/provider/openai_compat.rs`](../../crates/core/src/provider/openai_compat.rs) — vision `content` arrays
- [`crates/cli/src/tui/app.rs`](../../crates/cli/src/tui/app.rs) — Ctrl+V, composer attachment indicator
- [`crates/cli/src/repl.rs`](../../crates/cli/src/repl.rs) — `/image` handling and `run_turn_with_images`

## References

- MiniMax Coding Plan MCP (optional tooling, not the primary chat path): [MiniMax-AI/MiniMax-Coding-Plan-MCP](https://github.com/MiniMax-AI/MiniMax-Coding-Plan-MCP)
