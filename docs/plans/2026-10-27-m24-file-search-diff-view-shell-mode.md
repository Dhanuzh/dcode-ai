# M24 — `@` file search, full-screen diff, shell mode, working line, 200k window (v1.0.23)

Status: implemented

1. **`@` file search like Claude Code.** Files come from `git ls-files
   --cached --others --exclude-standard` (`.gitignore` respected, new files
   included; bounded walk outside git) plus their folders as `dir/`
   entries. Fuzzy ranking (subsequence; bonuses for matches in the file
   name, at word starts, in a row, name prefix / exact stem; shorter and
   shallower paths first). A typed folder (`src/tui/`) lists its children,
   ranked by the part after the folder. Picking a folder keeps the list open.
   The list rescans every 10 s while open. Drawn as a plain list under the
   prompt: `❯`, dim folder part, matched letters bold, eight rows.
2. **Full-screen diff view** (`diff_overlay.rs`): changed files on the left
   (name first, folder dim, `+N −M`), the file's diff on the right with old
   and new line numbers and full-width add/remove colours. ↑↓ file, Tab
   switches pane, PgUp/PgDn/Space scroll, `n`/`p` hunk, `r r` revert hunk,
   `g` refresh, `q` close. Opened by Enter or a click in the dashboard's
   changes tab, or bare `/diff`.
3. **Changes rail** is a compact list: a summary line (`N files +X −Y`),
   then one row per file; j/k move by file.
4. **Shell mode**: a leading `!` becomes the pink prompt and the footer says
   `! for shell mode` (Claude Code).
5. **dcode-ai's own working line** instead of Claude Code's star spinner
   and random verbs: a sweeping bar `▰▰▱▱▱▱`, the actual activity
   (`thinking`, `writing`, `running bash`) with a shimmer, time, output
   tokens, `esc to stop`. Turn end: `✓ done in 12s · 48.3k tokens`. The
   dashboard's session rail uses `◆`.
6. **Working window capped at 200k** unless the model id says `[1m]`:
   `claude-sonnet-5` reports 1M, so compaction waited until ~1M and every
   step re-read that much context. Claude Code works within 200k by
   default. Applies to compaction and to every context gauge.
