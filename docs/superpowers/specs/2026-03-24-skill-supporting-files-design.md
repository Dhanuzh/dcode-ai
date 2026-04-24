# Skill Supporting Files Inlining Design

**Issue:** [#34](https://github.com/madebyaris/dcode-ai/issues/34) (sub-project 1 of 4)
**Date:** 2026-03-24
**Status:** Approved

## Summary

When a skill is invoked via `/command`, automatically detect file references in the SKILL.md body, read the referenced files from the skill's directory, and inline their contents into the prompt. This ensures skills that reference supporting files (like superpower's `./implementer-prompt.md`, `@testing-anti-patterns.md`, etc.) work correctly.

## Motivation

Complex skills like superpowers reference sibling files in their SKILL.md body (e.g., `./implementer-prompt.md`, `@spec-reviewer-prompt.md`, `skills/brainstorming/visual-companion.md`). Currently DCODE_AI only loads the SKILL.md body — the LLM has no way to access these referenced files, causing skill invocation to fail.

## Design

### Reference Detection

Scan the SKILL.md body for file references matching these **intentional reference patterns only**:

1. **`./` prefixed paths**: `./filename.md`, `./subdir/file.md` — strongest signal of intentional reference
2. **`@` prefixed paths**: `@filename.md`, `@testing-anti-patterns.md` — Claude Code convention for "include this file". Strip `@` during path resolution.
3. **Backtick-wrapped paths with directory component**: `` `subdir/file.md` ``, `` `skills/brainstorming/visual-companion.md` `` — when the path contains a `/`
4. **Backtick-wrapped filenames with supported extension**: `` `code-reviewer.md` ``, `` `helper.sh` `` — bare filenames in backticks, resolved only against `skill.directory`. This catches same-directory references like `requesting-code-review`'s `` `code-reviewer.md` ``.

**Explicitly excluded** (not matched):
- Bare filenames outside backticks without `./` or `@` prefix (e.g., prose mentions of `CLAUDE.md`)
- Well-known config files in backticks: `CLAUDE.md`, `AGENTS.md`, `GEMINI.md`, `SKILL.md`, `package.json`, `README.md`
- URLs (anything starting with `http://` or `https://`)
- Paths containing `path/to/` (template examples)
- The file `SKILL.md` itself

**Supported extensions**: `.md`, `.sh`, `.ts`, `.js`, `.dot`, `.txt`, `.html`, `.cjs`

Detection produces a deduplicated list of reference paths, preserving first-occurrence order.

### Path Resolution

All references are resolved using a **three-level strategy**:

1. **Primary**: resolve relative to `skill.directory` (the SKILL.md parent folder)
2. **Catalog root fallback**: if not found, resolve relative to the parent of `skill.directory` (the catalog root, e.g., `~/.dcode-ai/skills/` for a skill at `~/.dcode-ai/skills/brainstorming/SKILL.md`)
3. **Strip `skills/` prefix**: if still not found and the reference starts with `skills/`, strip the `skills/` prefix and retry against the catalog root. This handles references like `skills/brainstorming/visual-companion.md` where the author's mental model treats `skills/` as a top-level namespace.

Pattern 4 (bare backtick filenames) only uses level 1 — resolved against `skill.directory` only. This prevents bare filenames from accidentally matching files in other skill directories.

This handles same-directory references (`./helper.md`), cross-skill references (`brainstorming/visual-companion.md`), and namespaced references (`skills/brainstorming/visual-companion.md`).

If a file doesn't exist at any resolution level, it's silently skipped.

### Inlining Logic

New method on `Skill`:

```rust
pub fn expanded_body(&self) -> String
```

Steps:
1. Extract file references from `self.body` using the patterns above
2. For each reference, resolve path using the three-level strategy
3. Read file contents (skip if read fails or file doesn't exist)
4. Build expanded body:
   - Original `self.body` first
   - Each referenced file appended as:
     ```
     \n\n===== referenced-file.md =====\n\n[file contents]
     ```

The `=====` separator avoids conflict with markdown `---` (horizontal rules / YAML frontmatter).

### Integration

`prompt_for_task()` changes from using `self.body.trim()` to `self.expanded_body().trim()`:

```rust
pub fn prompt_for_task(&self, task: &str) -> String {
    let mut prompt = format!(
        "Use the skill `{}`.\n\nSkill instructions:\n{}\n",
        self.command,
        self.expanded_body().trim()
    );
    if !task.trim().is_empty() {
        prompt.push_str(&format!("\nTask:\n{}\n", task.trim()));
    }
    prompt
}
```

### What stays the same

- `parse_skill_file()` — no changes
- `SkillCatalog::discover()` — no changes
- `Skill.body` field — stores raw SKILL.md content, unexpanded
- `manifest_summary()` — uses description, not expanded body
- Slash menu / system prompt skill listing — unchanged
- AGENTS.md parsing — unchanged (AGENTS.md skills don't have supporting files)

### Known Limitations

- **No size limit** on inlined content (by design — user preference)
- **Extension list is fixed** — `.md`, `.sh`, `.ts`, `.js`, `.dot`, `.txt`, `.html`, `.cjs`. Can be extended later if needed.
- **References inside code fences are matched** — references inside fenced code blocks (e.g., bad-example illustrations) are scanned. Non-existent files are silently skipped, so this is benign but slightly surprising.
- **No viewport scrolling** for large expanded prompts — relies on existing context window management

### Testing

- **Reference detection with `./` paths**: Body with `./foo.md` and `./subdir/bar.md`, verify correct paths extracted in order.
- **Reference detection with `@` paths**: Body with `@helper.md` and `@testing-anti-patterns.md`, verify `@` stripped and paths resolved.
- **Reference detection with backtick directory paths**: Body with `` `skills/brainstorming/visual-companion.md` ``, verify matched.
- **Reference detection with bare backtick filenames**: Body with `` `code-reviewer.md` ``, verify matched. Body with `` `CLAUDE.md` ``, verify NOT matched (excluded).
- **Exclusions**: Body with `AGENTS.md`, `https://example.com/file.md`, `path/to/test.md`, verify none matched.
- **`expanded_body()`**: Temp skill directory with SKILL.md referencing `./helper.md`, verify expanded output contains both with `===== helper.md =====` separator.
- **Missing files**: Reference non-existent file, verify silently skipped, rest expands correctly.
- **Three-level resolution**: Skill in `skills/sdd/SKILL.md` referencing `skills/brainstorming/file.md`, verify `skills/` prefix is stripped and file resolves against catalog root.
- **Bare backtick resolves only against skill directory**: `` `code-reviewer.md` `` in a skill at `skills/review/` resolves only to `skills/review/code-reviewer.md`, NOT to files in other directories.
- **Deduplication**: Same file referenced by `./helper.md` and `` `helper.md` ``, verify inlined only once.
- **No regressions**: Existing `parse_skill_file` and `parse_agents_md` tests pass unchanged.

## Files Affected

- `crates/core/src/skills.rs` — add `expanded_body()` method, reference extraction function, update `prompt_for_task()`
