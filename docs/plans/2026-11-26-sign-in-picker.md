# Sign-in picker + Linux glibc (2026-11-26)

Asked by the user: v1.0.94 on their Linux box failed with
"`GLIBC_2.43' not found" right after install; "login you need to enhance
it" → chose **first-run sign-in picker**.

## glibc
`ubuntu-latest` moved to a glibc 2.43 image, so the Linux binaries only ran
on the newest distros. Release builds now run on `ubuntu-22.04` /
`ubuntu-22.04-arm` (glibc 2.35: Ubuntu 22.04+, Debian 12+). `install.sh`
checks glibc first and says what to do instead of installing a binary that
can't start.

## Picker
The picker already opened on first run when nothing was signed in. Make it
the one place to sign in:

- `dcode-ai login` with no provider opens it (`dcode-ai login copilot`
  still works as before).
- Each row shows its state, checked when the picker opens and after each
  attempt: `✓ signed in`, `needs Claude Code (claude) — not installed`,
  `no Ollama / LM Studio running`, or `ready`.
- The cursor starts on the first row that can work right now (signed in,
  else possible to sign in) — on a fresh machine without `claude`, that is
  Copilot, not a Claude row that can only fail.
- `1`–`3` pick a row directly; Enter on a signed-in row just uses it.
- A row that can't work says how to fix it (install link / `ollama pull`)
  under the list instead of a bare "Login failed".
