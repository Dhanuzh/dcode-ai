# M15 — Starter packs (`dcode-ai setup`)

Status: implemented (v1.0.8)

## Goal

A new developer picks what they do (roles) and the languages they use, and
dcode-ai installs the plugins and MCP servers that fit, and says which
language tools are missing. Entirely optional.

## Shape

- **Packs** are TOML. Built-in packs ship in the binary
  (`crates/common/src/packs/builtin.toml`); extra packs are read from
  `~/.dcode-ai/packs/*.toml` and `<repo>/.dcode-ai/packs/*.toml`
  (a later file with the same id replaces an earlier one). A team can
  publish its own.
- A pack has `id`, `name`, `kind` (`role` | `language`), `description`,
  optional `detect` (marker files or `*.ext` globs at the repo root, for
  languages), and:
  - `plugins = ["name@marketplace", …]`
  - `[[pack.mcp]]` servers (`name`, `command`+`args` or `url`, `note`)
  - `[[pack.tools]]` language tools (`command`, `install`, `why`)
  - `notes`
- Roles: Backend, Frontend / UI-UX, DevOps, Automation, Data / ML,
  Security. Languages: Rust, TypeScript/JavaScript, Python, Go, Java,
  Kotlin, C#, C/C++, PHP, Ruby, Swift, Lua.

## Commands

```
dcode-ai setup                       # wizard: roles, languages (detected ones preselected), scope
dcode-ai setup backend rust          # install these packs
dcode-ai setup --list                # packs and what each adds
dcode-ai setup --detect              # languages found in this repo
dcode-ai setup --remove <pack>       # undo a pack (keeps items another pack still needs)
dcode-ai setup --save team.toml      # write the installed pack ids for a team
dcode-ai setup --from team.toml      # install a team file
  --scope project|user               # where MCP servers go (default project)
  --yes                              # no questions (MCP servers still need --yes to be added)
  --dry-run
```

`/setup` in a session prints the same list and installs packs given as
arguments (`/setup backend rust`); plugins load on the next session.

## Rules

- Idempotent: an installed plugin or an existing MCP server of the same name
  is skipped.
- The official marketplace is added on demand when a pack needs it.
- Third-party MCP servers are asked about one by one (`--yes` accepts all).
- Language tools are never installed by dcode-ai: missing ones are listed
  with their install command.
- What each pack added is recorded in `~/.dcode-ai/setup.json` so
  `--remove` only removes what setup added and nothing another pack uses.
- Never offered at startup: setup runs only when the user asks.
