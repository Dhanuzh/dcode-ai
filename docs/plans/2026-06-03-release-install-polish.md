# Release and install polish plan

Date: 2026-06-03

## Goal

Make release installs easier to verify and make `/doctor` more useful for
debugging real installations.

## Current state

- `install.sh` downloads release tarballs but does not verify checksums.
- The release workflow uploads only archives.
- `/doctor` reports providers, skills, MCP count, and memory path, but not the
  running binary path/version or runtime socket directory.

## Scope

- Generate SHA-256 checksum files for release archives in CI.
- Teach `install.sh` to download and verify `<archive>.sha256` when available.
- Keep install working when old releases do not have checksum files, but print a
  clear warning.
- Extend `/doctor --json` and human output with binary path, package version,
  runtime socket directory, platform, and checksum tool availability.

## Out of scope

- Sigstore or detached signature verification.
- Changing release artifact names.
- Network calls from `/doctor`.
