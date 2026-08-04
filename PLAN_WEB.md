# dcode web — implementation plan

Status: **draft, pre-implementation.** Three decisions in [Open decisions](#open-decisions)
should be settled before P1 starts.

Goal: a hosted, ChatGPT-style web app where a user signs in with Google, chats
with the dcode agent, and — optionally — points it at a folder on their own
machine. Installing the CLI must be **optional**, not required.

---

## 1. The constraint that shapes the design

A browser can open a local folder with no install, via the
[File System Access API](https://developer.mozilla.org/docs/Web/API/File_System_API)
(`showDirectoryPicker()`). It grants **file read/write inside the chosen
directory** — and nothing else.

It cannot run `bash`, `git`, `cargo`, `npm`, a test suite, or any process. That
is an OS sandbox boundary, not a missing browser feature; no amount of
engineering removes it. It is also **Chromium-only** (Chrome, Edge, Opera —
not Firefox, not Safari) and desktop-only.

So "no install + full agent" is not achievable. "No install + a genuinely
useful agent" is. The product is therefore tiered rather than all-or-nothing.

## 2. Capability tiers

| | **Tier 0 — Chat** | **Tier 1 — Browser folder** | **Tier 2 — CLI linked** |
| --- | --- | --- | --- |
| Setup | Google login | + pick a folder | + install `dcode-ai` |
| Browser support | all | Chromium only | all |
| Chat + history | yes | yes | yes |
| Read project files | — | yes | yes |
| Create / edit files | — | yes | yes |
| Search across files | — | yes | yes |
| **Run commands, tests, git** | — | — | yes |
| Sub-agents, worktrees | — | — | yes |

Tier 1 covers "read and refactor my code", which is most of the day-to-day
value. Tier 2 becomes an upgrade path rather than a barrier to entry.

## 3. Architecture

The agent loop runs **on the server**. Whatever the user has connected —
a browser tab or a linked CLI — acts as a **remote tool executor**.

```
Browser (React · Cloudflare Pages)
  ├─ chat UI, streaming
  └─ File System Access API ──► user's local folder
        ▲   implements: read_file, write_file, list_dir, search_files
        │   WebSocket
        ▼
Rust API (Axum · Railway)
  ├─ Google OAuth · session cookies
  ├─ AgentLoop                 ← reused from dcode-ai-core
  ├─ RemoteToolExecutor        → dispatches each tool call to browser or CLI
  └─ Neon Postgres             (users, conversations, messages, devices)
        ▲
        │   WebSocket, dialled outbound by the user's machine
        ▼
`dcode-ai --link` ──► full toolset: bash, git, build, sub-agents
```

### Why this shape

Three pieces already exist in this repo and carry most of the weight:

- **`crates/common/src/event.rs`** — `AgentCommand` (`SendMessage`,
  `ApproveToolCall`, `DenyToolCall`, `AnswerQuestion`, `Cancel`, `Shutdown`)
  and `AgentEvent` / `EventEnvelope`. Already `serde`-serializable.
- **`crates/runtime/src/ipc.rs`** — already speaks newline-delimited JSON of
  exactly those types over a socket. Going remote is a **transport swap**, not
  a new protocol.
- **`ToolExecutor`** in `dcode-ai-core` is an async trait. An implementation
  that forwards a `ToolCall` over a WebSocket and awaits the `ToolResult`
  satisfies it directly.

The agent therefore does not know or care where a tool ran. Tier 1 simply
registers a smaller toolset than Tier 2. **One agent, one protocol, two
backends** — no forked logic, no duplicated schema.

Because the laptop dials *out* in Tier 2, there is no port forwarding, no
firewall rule and no public IP — the same approach VS Code Tunnels uses.

## 4. Stack

| Layer | Choice | Notes |
| --- | --- | --- |
| Frontend | React + Vite + Tailwind v4 | Reuses the design system in `www/` |
| Static hosting | Cloudflare Pages | Already configured for `www/` |
| API | Axum (Rust) on **Railway** | Depends on `dcode-ai-common` + `dcode-ai-core` |
| Database | **Neon Postgres** via `sqlx` | Serverless, scales to zero |
| Auth | Google OAuth 2.0 + PKCE | Same flow as the existing Antigravity login |
| Session | HttpOnly + Secure cookie | Not `localStorage` — XSS-resistant |

Cloudflare Workers is deliberately **not** used for the API: it is a
WASM runtime without tokio, long-lived WebSockets or `sqlx`. Static assets stay
on Cloudflare; the stateful API lives on Railway.

## 5. Data model

```sql
users         (id, google_sub UNIQUE, email, name, avatar_url, created_at)
conversations (id, user_id, title, tier, created_at, updated_at)
messages      (id, conversation_id, role, content, tool_calls JSONB, created_at)
devices       (id, user_id, name, token_hash, last_seen_at, revoked_at)
user_keys     (user_id, provider, ciphertext, nonce)   -- BYO provider keys
```

`devices.token_hash` stores a hash, never the token. `user_keys` is encrypted
at rest with a server-held key (env var), decrypted only in memory per request.

## 6. Phases

### P1 — Chat that works (Tier 0)
Ships a real, usable product with no folder plumbing.

- Axum service + Railway deploy + Neon migrations (`sqlx migrate`)
- Google OAuth login/logout, session cookie, `users` upsert
- Conversation + message persistence
- Server-side agent turn with **SSE** streaming to the browser
- Chat SPA: sidebar of conversations, streaming transcript, markdown +
  syntax highlighting, reuse `www/` tokens

**Done when:** a user signs in on the deployed site, chats, reloads, and sees
their history.

### P2 — Browser folder (Tier 1)
The differentiator.

- `showDirectoryPicker()` + persist the handle (IndexedDB) across reloads
- Browser-side tool implementations: `read_file`, `write_file`, `list_dir`,
  `search_files`
- `RemoteToolExecutor` in Axum: dispatch `ToolCall` → browser, await result,
  with timeout + cancellation
- Approval UI **before** any write; diff preview reusing the CLI's diff logic
- Capability negotiation on connect (browser advertises its toolset)
- Clear messaging for non-Chromium browsers

**Done when:** a user picks a folder in Chrome and the agent reads, edits and
searches real files with per-write approval.

### P3 — CLI link (Tier 2)
Mostly transport work; the protocol already exists.

- `dcode-ai --link` — dial Railway over WSS, authenticate, relay existing
  `AgentCommand`/`AgentEvent` frames
- Device pairing with a short-lived one-time code
- Device management UI (name, last seen, revoke)
- Full toolset advertised; bash/git/build enabled

**Done when:** a linked laptop runs tests from a browser prompt.

### P4 — Polish
Multi-device selection, shareable read-only conversations, mobile chat layout,
per-user rate limits, usage dashboard.

## 7. Security requirements

Tier 2 means *a website can execute commands on a developer's machine.* These
are non-negotiable, not nice-to-haves:

1. Pairing uses a **short-lived one-time code**, never a long-lived shared secret.
2. Device tokens are **per-device, revocable, and scoped to a single workspace path**.
3. Approval is enforced **daemon-side**. The server/browser may *render* a
   prompt, but the executor must independently require it — never trust the
   client to have asked.
4. Linked sessions default to a restrictive permission mode. **Never Bypass.**
5. Path traversal is rejected at the executor: every resolved path must remain
   inside the granted root.
6. Tokens are stored hashed; provider keys encrypted at rest.

## 8. Open decisions

1. **Who pays for inference?** In Tier 0/1 the *server* calls the model.
   - *(a)* **BYO key** — user supplies a provider key, encrypted in `user_keys`.
     Zero cost and no billing system. **Recommended for P1.**
   - *(b)* Platform keys — you pay; requires billing, quotas and abuse controls.

2. **Persist file contents?** Agent context pulls file bodies into the
   conversation. Storing them means holding customers' source code.
   **Recommendation: do not persist file bodies** — keep them in the live
   session; store only paths and diffs in `messages`.

3. **Non-Chromium browsers in Tier 1.** No File System Access API.
   Options: a read-only "upload folder" fallback, or an explicit message
   pointing at Chrome or the CLI. **Recommendation: the honest message.**

## 9. Non-goals

- Running builds/tests in the cloud (no cloud containers — that's a different,
  much more expensive product)
- Hosting user repositories
- Replacing the TUI; the web app is an additional surface, not a successor
