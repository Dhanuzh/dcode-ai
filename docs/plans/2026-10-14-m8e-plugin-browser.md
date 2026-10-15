# M8e — Plugin marketplaces and the `/plugins` browser

Date: 2026-10-14 · Branch: `feat/claude-copilot` · Status: ✅ done, live-tested (official marketplace, install from browser, plugin command run) · Parent: [v1 master plan](2026-10-12-v1-master-plan.md)

User asked for Claude Code's `/plugins` experience (screenshot: tabs
Discover · Installed · Marketplaces · Errors, search, Space to toggle).

## Marketplaces (core `plugin_installer.rs`)
- A marketplace is a git repo (or local dir) with
  `.claude-plugin/marketplace.json` — the open format Claude Code uses.
- Known marketplaces: `~/.dcode-ai/marketplaces/known.json`; git clones in
  `~/.dcode-ai/marketplaces/<name>/` (local directories are read in place).
- Default: `anthropics/claude-plugins-official` is added the first time the
  browser opens with no marketplace (public repo, read-only clone).
- Entry sources supported: relative path, `{source:"github", repo}`,
  `{source:"url", url, ref?}`, `{source:"git-subdir", url, path, ref?}`.
  Entries with `strict: false` and no manifest get one synthesized from
  the marketplace entry.
- Installed plugins record `marketplace:<market>/<plugin>` as source, so
  `update` re-resolves through the (updated) marketplace.
- CLI: `dcode-ai plugin marketplace add|list|update|remove`,
  `dcode-ai plugin install name@market`.

## TUI browser (`/plugins`)
- Tabs: **Discover** (all marketplace plugins, search over name /
  description / category, installed marked ●), **Installed** (enabled ●
  / disabled ○, what each adds), **Marketplaces** (plugin counts, last
  update), **Errors** (manifest / marketplace problems).
- Keys: type to search · ↑↓ · Tab/←→ switch tab · Space/Enter install or
  uninstall (Discover), enable/disable (Installed), update
  (Marketplaces) · `u` update · `x` remove · `a` add marketplace · Esc.
- Git work runs off the UI thread; the browser shows progress and a toast
  with the result. Commands and skills apply immediately; MCP servers and
  hooks on the next session (`/new`).

## Verification
Unit tests: marketplace parsing (all source kinds, strict:false), add/
update/remove of a local marketplace, install via `name@market`, browser
state (filtering, tabs, key handling). Live: open `/plugins`, install and
remove a plugin from the official marketplace.
