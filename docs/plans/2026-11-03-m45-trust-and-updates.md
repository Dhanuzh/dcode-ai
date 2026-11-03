# M45 — Guest roles, Alt+F preview, auto-update, dashboard keys (v1.0.50)

Status: implemented

From a Reddit question: "once a guest is approved, can their messages run
tools on the host? And what exactly does Alt+F send?"

## 1. `/invite` roles

- The knock prompt has three answers: **Let {name} in to watch and chat**,
  **Let {name} in to drive**, **No**.
- Watchers see everything live, chat with people (team chat) and join
  calls. The host refuses anything of theirs that would reach the agent
  (`say`, `todo`, `approve`, `deny`, `stop`) with a note. Their composer
  sends to team chat instead. Browser guests get the same (`/say` and
  `/todo` refused).
- Drivers' messages become prompts, under the host's permission mode;
  approvals stay with the host unless `--let-approve`.
- The role is in the `admitted` message (`"role": "drive"|"watch"`), kept
  on rejoin. Hosts before 1.0.50 didn't send it: guests treat that as drive.
- **Auto-mode guard**: letting someone drive while in auto / bypass /
  don't-ask switches the host to default mode, with a note, so the agent
  asks before acting on a guest's request. The knock prompt says so first.

## 2. Alt+F: masked, previewed

- `dcode-ai fix` sends only the command, its exit code and the folder: no
  output, no environment.
- Secrets in the command are masked first: `Authorization: Bearer …`,
  `-u user:pass`, `--password/--token/…` flags, mysql-style `-pPASS`,
  `NAME=value` with a secret-looking name, `user:pass@` in URLs, `?token=`,
  then the usual token patterns (`ghp_`, `sk-`, PEM, …).
- It prints exactly what will be sent and asks `Send it? [Y/n]`
  (`--yes` skips).

## 3. Auto-update

- Like Claude Code: when the startup check (cached, every 6 h) knows of a
  newer release, it's downloaded, checksum-verified and installed in the
  background; a note says to restart. A system folder that needs sudo
  only gets a note to run `/update`; a Homebrew copy a note to
  `brew upgrade`.
- Off with `[ui] auto_update = false` or `DCODE_AI_AUTO_UPDATE=0`. Never
  for a build from source (debug, or running from `target/`).

## 4. Dashboard keys

- The welcome box shows `Ctrl+\ dashboard view`.
- `Ctrl+D` focuses the tab that is showing (it used to jump to changes).
  Diff keys (`j/k`, `r r`, `g`, `Enter`) act only on the changes tab.
- Files tab: `←/→` switch tabs like everywhere else; `Enter`/`Space` (or
  `l`/`h`) open and close folders, `Enter` on a file adds `@file`.

## Tests

- Invite: a watcher's `say` is refused with a note, their chat reaches
  people, nothing reaches the agent; a driver still talks.
- Mask: bearer, `-u`, mysql `-p`, `--token`, `API_KEY=`, `access_token`,
  git URL password, `ghp_`; cargo's `-p` untouched. PTY: the preview shows,
  the token doesn't, Enter sends.
- Keys: Ctrl+D keeps the tab; ←/→ from files switch tabs; Enter on a
  non-changes tab doesn't open the diff.
