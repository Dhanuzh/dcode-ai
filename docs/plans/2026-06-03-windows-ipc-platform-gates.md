# Windows IPC and platform gates plan

Date: 2026-06-03

## Goal

Make the detached-session IPC layer compile behind platform-specific transports:
Unix domain sockets on Unix and loopback TCP on Windows.

## Scope

- Replace hard-coded Unix socket construction with a shared runtime IPC directory.
- Keep the existing `socket_path` session metadata field for compatibility, but
  store a platform endpoint path (`.sock` on Unix, `.tcp` metadata path on
  Windows).
- Gate Unix socket listener/client code behind `cfg(unix)`.
- Add Windows loopback TCP listener/client code behind `cfg(windows)`.
- Update `/doctor` and docs to report the platform IPC directory.

## Out of scope

- Native Windows named pipes.
- Full Windows PTY parity.
- Cross-compiled CI in this local pass.
