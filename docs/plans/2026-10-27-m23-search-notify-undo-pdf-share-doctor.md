# M23 — session content search, auto titles, desktop notifications, shell undo, PDF/notebooks, /share, project settings, tips, /doctor (v1.0.22)

Status: implemented

1. **`/resume` searches what was said.** Entries carry the session's user
   and assistant text (loaded in the background after the picker opens).
   Query words match in any order; a content match shows a highlighted
   snippet instead of the meta line.
2. **Session titles.** After the first turn of an unnamed session a cheap
   model (`[model] compact_model`, else Haiku on Claude) writes a 3-6 word
   title in the background; once per session, applied only if still
   unnamed and still the current session. Bare `/rename` asks for one and
   replaces the name.
3. **Desktop notifications** when the terminal is unfocused and a turn
   finishes, an approval or a question waits: Windows toast via PowerShell
   (also from WSL, PowerShell's app id), `osascript` on macOS, `notify-send`
   on Linux; OSC 9 + bell otherwise.
4. **Shell commands can be undone.** Before a batch with `execute_bash` /
   `run_validation`, the working tree is written as a git tree through a
   throwaway index (`git add -A` + `write-tree`, the user's staging area is
   untouched); afterwards the trees are diffed and every changed file's old
   content goes into the turn's undo record, so `/undo` and `/rewind` cover
   `sed`, generators and the like. Git workspaces only; ignored files
   (`node_modules`) are not tracked; more than 2,000 changed files skips it.
5. **PDF**: `read_file` returns text page by page (`pdf-extract`, panics
   caught); `offset`/`limit` count pages. Scanned PDFs say so.
6. **Notebooks**: `read_file` renders `.ipynb` as numbered cells with
   outputs; new `notebook_edit` (replace / insert / delete a cell), Claude
   Code's `NotebookEdit` maps to it. Writes Jupyter's one-space JSON.
7. **`/export` fixed and `/share` added.** `/export` was reading a key the
   event log doesn't have and wrote an empty file. Both now build Markdown
   from the conversation (turns + one line per tool call, no system prompt or
   tool output) with secrets masked (known secrets, bearer tokens,
   `key=value`, long hex keys in URLs, home folder → `~`). `/export copy`
   copies it; `/share` posts a secret gist with `gh` and copies the URL.
8. **Project settings**: the committed `.dcode.toml` layer already existed;
   `/project-config` shows it and saves the current model (`model`) or
   permission mode (`mode`); `/permissions allow <rule> --project` writes
   rules there.
9. **First-run tips**, shown once (`~/.dcode-ai/.tips-shown`): modes, `@`
   and `!`, `/btw`.
10. **`/doctor` rewritten.** It still checked API keys (dcode-ai is
    OAuth-only). Now: sign-in for the provider in use, git, ripgrep, config
    files that fail to parse, MCP servers, session disk use. Fixes offered in
    one prompt: install ripgrep (through the `!` path, so sudo gets its
    popup), turn off a failing MCP server for the folder, clean sessions.
11. **Tokens per turn** in the "✻ Cooked for 12s · 48.3k tokens" line.

## 12. Claude plan usage (found from the user's logs)

Per-call input was 20–35k tokens; a live check showed caching works inside
one `claude` process, so the cost came from process restarts, each
re-sending the conversation as cache writes (1-hour cache, 2×):
- auto mode's SAFE/ASK check called `provider.chat` with no tools and an
  empty system prompt on the session's provider → key mismatch → main
  process killed, then killed again on the next real step (55 checks in one
  log). Tool-less calls now use a throwaway process (`one_shot`) and the
  check uses `utility_model` (compact_model, else Haiku on Claude);
- tool schemas were part of the process key; added tools are now sent as a
  `<new_tools>` block in the next message (a changed schema still restarts);
- a dropped receiver (Esc) killed the process; the turn is drained and the
  process kept, with a note that the interrupted reply's tool calls didn't
  run;
- verdict cache key drops plain `export` steps, keeps `cd` targets.
Restart reasons are logged at info level (`claude-code: …`).
