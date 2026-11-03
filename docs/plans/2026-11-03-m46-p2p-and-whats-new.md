# M46 — Peer-to-peer invites, "what's new", no token pause (v1.0.51)

Status: implemented

## 1. Direct peer-to-peer invites (iroh)

Goal: someone on another network joins with the same line, without
Tailscale, a relay of yours, or open ports.

- The host binds an iroh endpoint per `/invite` (fresh key each time,
  ALPN `dcode-ai/invite/1`, n0's address lookup and relays) and adds
  `p2p-<id>` (base32, 52 characters) to the invite's address list:
  `dcode-ai join 192.168.0.9,p2p-…:8788/<key>`.
- `join` tries the direct addresses first, then dials the id (25 s), opens
  one bidirectional QUIC stream and runs the unchanged `DCAI1` + Noise
  `NNpsk0` handshake inside it. Reconnects reuse one client endpoint.
- The host code takes any stream (TCP halves or QUIC streams are boxed
  `AsyncRead`/`AsyncWrite`), so knocks, roles, rejoin, calls and chat work
  the same over every transport. Knocks say "(a direct link)".
- Security: QUIC is authenticated with the host's key (from the invite),
  the Noise layer with the invite key. iroh's relays and DNS lookup see IP
  addresses and ciphertext only.
- Off: `/invite lan`, `DCODE_AI_P2P=0`. Old guests ignore the `p2p-` entry
  (they fail it and use the others).
- Tests: in-process host and guest over iroh on localhost with an
  in-memory address book (no public network); an ignored test dials over
  the public network (1.3 s round trip here).
- cargo-deny: allowed `Unlicense` (wasm-only transitive crates) and the
  `paste` unmaintained advisory (compile-time macro, via netlink).

## 2. "Updated to vX · what's new"

- `~/.dcode-ai/last-version` records the version that ran. The first start
  of a newer one shows "✓ Updated to v1.0.51 (from v1.0.49) · what's new"
  with the headline of each changelog item since the old version (at most
  6), then records it. Nothing on a first install.

## 3. No token pause

- The `turn_token_alarm` pause (5M tokens) is removed: the setting is read
  but ignored. The same-tool-call loop guard stays.
