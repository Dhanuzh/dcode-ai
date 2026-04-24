# Skill Supporting Files Inlining Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** When a skill is invoked, automatically inline referenced supporting files from the skill's directory into the prompt.

**Architecture:** Add a `extract_file_references()` function to detect file paths in the SKILL.md body using pattern matching (./paths, @paths, backtick-wrapped paths), add a three-level path resolution strategy, and add an `expanded_body()` method on `Skill` that inlines resolved files. Update `prompt_for_task()` to use the expanded body.

**Tech Stack:** Rust, regex crate

**Spec:** `docs/superpowers/specs/2026-03-24-skill-supporting-files-design.md`

---

### Task 1: Add `regex` dependency to `dcode-ai-core`

**Files:**
- Modify: `crates/core/Cargo.toml`

- [ ] **Step 1: Add regex dependency**

Add to `[dependencies]` section in `crates/core/Cargo.toml`:

```toml
regex = "1"
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p dcode-ai-core 2>&1 | tail -5`
Expected: no errors

- [ ] **Step 3: Commit**

```bash
git add crates/core/Cargo.toml Cargo.lock
git commit -m "chore: add regex dependency to dcode-ai-core (#34)"
```

---

### Task 2: Implement `extract_file_references()` with tests (TDD)

**Files:**
- Modify: `crates/core/src/skills.rs:397-471` (add to test module and add function before tests)

- [ ] **Step 1: Write failing tests for reference extraction**

Add these tests to the `#[cfg(test)] mod tests` block in `crates/core/src/skills.rs`:

```rust
#[test]
fn extracts_dot_slash_references() {
    let body = "Read ./implementer-prompt.md for details.\nAlso see ./subdir/helper.sh here.";
    let refs = extract_file_references(body);
    assert_eq!(refs, vec!["./implementer-prompt.md", "./subdir/helper.sh"]);
}

#[test]
fn extracts_at_prefixed_references() {
    let body = "Use @testing-anti-patterns.md to avoid pitfalls.\nSee @graphviz-conventions.dot too.";
    let refs = extract_file_references(body);
    assert_eq!(refs, vec!["testing-anti-patterns.md", "graphviz-conventions.dot"]);
}

#[test]
fn extracts_backtick_paths_with_directory() {
    let body = "Check `skills/brainstorming/visual-companion.md` for guidance.\nAlso `subdir/file.ts`.";
    let refs = extract_file_references(body);
    assert_eq!(refs, vec!["skills/brainstorming/visual-companion.md", "subdir/file.ts"]);
}

#[test]
fn extracts_backtick_bare_filenames() {
    let body = "See `code-reviewer.md` for the template.\nUse `helper.sh` too.";
    let refs = extract_file_references(body);
    assert_eq!(refs, vec!["code-reviewer.md", "helper.sh"]);
}

#[test]
fn excludes_well_known_files_in_backticks() {
    let body = "Check `CLAUDE.md` and `AGENTS.md` and `GEMINI.md` and `SKILL.md` and `README.md` and `package.json`.";
    let refs = extract_file_references(body);
    assert!(refs.is_empty());
}

#[test]
fn excludes_urls_and_template_paths() {
    let body = "See https://example.com/file.md and path/to/test.md for examples.";
    let refs = extract_file_references(body);
    assert!(refs.is_empty());
}

#[test]
fn deduplicates_preserving_order() {
    let body = "Use ./helper.md first.\nThen `helper.md` again.\nAnd @helper.md once more.";
    let refs = extract_file_references(body);
    assert_eq!(refs, vec!["./helper.md"]);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p dcode-ai-core extract_file_references 2>&1 | tail -10`
Expected: FAIL — function not found

- [ ] **Step 3: Implement `extract_file_references()`**

Add this function before the `#[cfg(test)]` block in `crates/core/src/skills.rs` (after `parse_permission_mode_str`, around line 395):

```rust
/// Extract file references from a skill body.
///
/// Detects: `./path`, `@path`, backtick-wrapped paths with supported extensions.
/// Returns deduplicated list in first-occurrence order, with `@` stripped and `./` preserved.
fn extract_file_references(body: &str) -> Vec<String> {
    use regex::Regex;
    use std::collections::HashSet;
    use std::sync::LazyLock;

    static EXCLUDED_NAMES: &[&str] = &[
        "CLAUDE.md", "AGENTS.md", "GEMINI.md", "SKILL.md", "README.md", "package.json",
    ];

    static DOT_SLASH_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\./[a-zA-Z0-9_][a-zA-Z0-9_./-]*\.(md|sh|ts|js|dot|txt|html|cjs)").unwrap()
    });

    // Require @ at start of line or after whitespace to avoid matching emails
    static AT_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?:^|\s)@([a-zA-Z0-9_][a-zA-Z0-9_./-]*\.(md|sh|ts|js|dot|txt|html|cjs))")
            .unwrap()
    });

    static BACKTICK_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"`([a-zA-Z0-9_][a-zA-Z0-9_./-]*\.(md|sh|ts|js|dot|txt|html|cjs))`").unwrap()
    });

    fn is_excluded(path: &str) -> bool {
        EXCLUDED_NAMES.contains(&path)
            || path.contains("path/to/")
            || path.starts_with("http://")
            || path.starts_with("https://")
    }

    let mut seen = HashSet::new();
    let mut refs = Vec::new();

    // Pattern 1: ./path references
    for m in DOT_SLASH_RE.find_iter(body) {
        let path = m.as_str().to_string();
        let key = path.trim_start_matches("./").to_string();
        if !is_excluded(&key) && !seen.contains(&key) {
            seen.insert(key);
            refs.push(path);
        }
    }

    // Pattern 2: @path references (strip @, require word boundary)
    for cap in AT_RE.captures_iter(body) {
        let path = cap[1].to_string();
        let key = path.clone();
        if !is_excluded(&key) && !seen.contains(&key) {
            seen.insert(key);
            refs.push(path);
        }
    }

    // Pattern 3+4: backtick-wrapped paths
    for cap in BACKTICK_RE.captures_iter(body) {
        let path = cap[1].to_string();
        let key = path.trim_start_matches("./").to_string();
        if !is_excluded(&key) && !EXCLUDED_NAMES.contains(&path.as_str()) && !seen.contains(&key) {
            seen.insert(key);
            refs.push(path);
        }
    }

    refs
}
```

Keep the `use` statements as local imports inside the function — no top-level import needed.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p dcode-ai-core extract_file_references 2>&1 | tail -20`
Expected: all 7 tests pass

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/skills.rs
git commit -m "feat: add extract_file_references for skill supporting files (#34)"
```

---

### Task 3: Implement `resolve_skill_reference()` with tests (TDD)

**Files:**
- Modify: `crates/core/src/skills.rs`

- [ ] **Step 1: Write failing tests for path resolution**

Add to the test module:

```rust
#[test]
fn resolves_reference_in_skill_directory() {
    let dir = tempfile::tempdir().unwrap();
    let skill_dir = dir.path().join("my-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("helper.md"), "helper content").unwrap();

    let result = resolve_skill_reference("./helper.md", &skill_dir);
    assert!(result.is_some());
    assert_eq!(result.unwrap(), skill_dir.join("helper.md"));
}

#[test]
fn resolves_reference_via_catalog_root_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("skills");
    let skill_dir = catalog.join("sdd");
    let other_skill = catalog.join("brainstorming");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::create_dir_all(&other_skill).unwrap();
    std::fs::write(other_skill.join("visual.md"), "visual content").unwrap();

    // Level 2: resolve against catalog root (parent of skill_dir)
    let result = resolve_skill_reference("brainstorming/visual.md", &skill_dir);
    assert!(result.is_some());
    assert_eq!(result.unwrap(), other_skill.join("visual.md"));
}

#[test]
fn resolves_skills_prefix_by_stripping() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("skills");
    let brainstorming = catalog.join("brainstorming");
    std::fs::create_dir_all(&brainstorming).unwrap();
    std::fs::write(brainstorming.join("visual.md"), "content").unwrap();

    let skill_dir = catalog.join("sdd");
    std::fs::create_dir_all(&skill_dir).unwrap();

    // Level 3: strip skills/ prefix, resolve against catalog root
    let result = resolve_skill_reference("skills/brainstorming/visual.md", &skill_dir);
    assert!(result.is_some());
    assert_eq!(result.unwrap(), brainstorming.join("visual.md"));
}

#[test]
fn returns_none_for_missing_file() {
    let dir = tempfile::tempdir().unwrap();
    let skill_dir = dir.path().join("my-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();

    let result = resolve_skill_reference("./nonexistent.md", &skill_dir);
    assert!(result.is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p dcode-ai-core resolve_skill_reference 2>&1 | tail -10`
Expected: FAIL — function not found

- [ ] **Step 3: Implement `resolve_skill_reference()`**

Add before `extract_file_references` in `crates/core/src/skills.rs`:

```rust
/// Resolve a file reference to an absolute path using three-level strategy:
/// 1. Relative to skill directory
/// 2. Relative to catalog root (parent of skill directory)
/// 3. Strip `skills/` prefix and retry against catalog root
///
/// Returns `None` if the file doesn't exist at any level.
fn resolve_skill_reference(reference: &str, skill_directory: &Path) -> Option<PathBuf> {
    let clean = reference.trim_start_matches("./");

    // Level 1: relative to skill directory
    let level1 = skill_directory.join(clean);
    if level1.is_file() {
        return Some(level1);
    }

    // Level 2: relative to catalog root (parent of skill directory)
    if let Some(catalog_root) = skill_directory.parent() {
        let level2 = catalog_root.join(clean);
        if level2.is_file() {
            return Some(level2);
        }

        // Level 3: strip "skills/" prefix and retry against catalog root
        if let Some(stripped) = clean.strip_prefix("skills/") {
            let level3 = catalog_root.join(stripped);
            if level3.is_file() {
                return Some(level3);
            }
        }
    }

    None
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p dcode-ai-core resolve_skill_reference 2>&1 | tail -20`
Expected: all 4 tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/skills.rs
git commit -m "feat: add resolve_skill_reference with three-level strategy (#34)"
```

---

### Task 4: Implement `expanded_body()` with tests (TDD)

**Files:**
- Modify: `crates/core/src/skills.rs:95-145` (add method to `impl Skill`)

- [ ] **Step 1: Write failing test for expanded_body**

Add to the test module:

```rust
#[test]
fn expanded_body_inlines_referenced_files() {
    let dir = tempfile::tempdir().unwrap();
    let skill_dir = dir.path().join("my-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("helper.md"), "Helper content here.").unwrap();

    let skill = Skill {
        name: "test".into(),
        description: None,
        command: "test".into(),
        model: None,
        permission_mode: None,
        context: SkillContextMode::Inline,
        directory: skill_dir,
        body: "Main body.\n\nSee ./helper.md for details.".into(),
        source: SkillSource::FileSystem,
    };

    let expanded = skill.expanded_body();
    assert!(expanded.contains("Main body."));
    assert!(expanded.contains("===== helper.md ====="));
    assert!(expanded.contains("Helper content here."));
}

#[test]
fn expanded_body_skips_missing_files() {
    let dir = tempfile::tempdir().unwrap();
    let skill_dir = dir.path().join("my-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();

    let skill = Skill {
        name: "test".into(),
        description: None,
        command: "test".into(),
        model: None,
        permission_mode: None,
        context: SkillContextMode::Inline,
        directory: skill_dir,
        body: "Main body.\n\nSee ./nonexistent.md for details.".into(),
        source: SkillSource::FileSystem,
    };

    let expanded = skill.expanded_body();
    assert_eq!(expanded.trim(), "Main body.\n\nSee ./nonexistent.md for details.");
}

#[test]
fn expanded_body_inlines_at_prefixed_reference() {
    let dir = tempfile::tempdir().unwrap();
    let skill_dir = dir.path().join("tdd");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("testing-anti-patterns.md"), "Anti-pattern content.").unwrap();

    let skill = Skill {
        name: "test".into(),
        description: None,
        command: "test".into(),
        model: None,
        permission_mode: None,
        context: SkillContextMode::Inline,
        directory: skill_dir,
        body: "Main body.\n\nRead @testing-anti-patterns.md to avoid pitfalls.".into(),
        source: SkillSource::FileSystem,
    };

    let expanded = skill.expanded_body();
    assert!(expanded.contains("===== testing-anti-patterns.md ====="));
    assert!(expanded.contains("Anti-pattern content."));
}

#[test]
fn expanded_body_bare_filename_resolves_only_in_skill_dir() {
    let dir = tempfile::tempdir().unwrap();
    let catalog = dir.path().join("skills");
    let skill_dir = catalog.join("review");
    let other_dir = catalog.join("other");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::create_dir_all(&other_dir).unwrap();
    // File exists in sibling skill dir but NOT in the skill's own dir
    std::fs::write(other_dir.join("template.md"), "Other content.").unwrap();

    let skill = Skill {
        name: "test".into(),
        description: None,
        command: "test".into(),
        model: None,
        permission_mode: None,
        context: SkillContextMode::Inline,
        directory: skill_dir,
        body: "See `template.md` for the template.".into(),
        source: SkillSource::FileSystem,
    };

    let expanded = skill.expanded_body();
    // Should NOT inline — bare filename only resolves against skill dir (Level 1)
    assert!(!expanded.contains("====="));
    assert!(!expanded.contains("Other content."));
}

#[test]
fn expanded_body_deduplicates() {
    let dir = tempfile::tempdir().unwrap();
    let skill_dir = dir.path().join("my-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(skill_dir.join("helper.md"), "Helper.").unwrap();

    let skill = Skill {
        name: "test".into(),
        description: None,
        command: "test".into(),
        model: None,
        permission_mode: None,
        context: SkillContextMode::Inline,
        directory: skill_dir,
        body: "See ./helper.md and `helper.md` again.".into(),
        source: SkillSource::FileSystem,
    };

    let expanded = skill.expanded_body();
    let count = expanded.matches("===== helper.md =====").count();
    assert_eq!(count, 1);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p dcode-ai-core expanded_body 2>&1 | tail -10`
Expected: FAIL — method not found

- [ ] **Step 3: Implement `expanded_body()`**

Add to the `impl Skill` block (after `source_label()`, around line 144):

```rust
/// Return the skill body with referenced supporting files inlined.
///
/// Scans the body for file references (./path, @path, backtick-wrapped paths),
/// resolves them against the skill's directory, and appends their contents.
/// Bare filenames (no `/` or `./` prefix) resolve only against skill directory (Level 1).
pub fn expanded_body(&self) -> String {
    let refs = extract_file_references(&self.body);
    if refs.is_empty() {
        return self.body.clone();
    }

    let mut expanded = self.body.clone();
    for ref_path in &refs {
        let clean = ref_path.trim_start_matches("./");
        let is_bare_filename = !clean.contains('/');

        let resolved = if is_bare_filename {
            // Pattern 4: bare filenames resolve only against skill directory (Level 1)
            let candidate = self.directory.join(clean);
            if candidate.is_file() { Some(candidate) } else { None }
        } else {
            resolve_skill_reference(ref_path, &self.directory)
        };

        if let Some(resolved) = resolved {
            if let Ok(content) = std::fs::read_to_string(&resolved) {
                expanded.push_str(&format!("\n\n===== {} =====\n\n{}", clean, content.trim()));
            }
        }
    }

    expanded
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p dcode-ai-core expanded_body 2>&1 | tail -20`
Expected: all 4 tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/skills.rs
git commit -m "feat: add expanded_body() for skill supporting file inlining (#34)"
```

---

### Task 5: Update `prompt_for_task()` to use expanded body

**Files:**
- Modify: `crates/core/src/skills.rs:107-117`

- [ ] **Step 1: Update `prompt_for_task()`**

Change line 111 in `prompt_for_task()` from:

```rust
        self.body.trim()
```

To:

```rust
        self.expanded_body().trim()
```

The full method becomes:

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

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p dcode-ai-core 2>&1 | tail -5`
Expected: no errors

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/skills.rs
git commit -m "feat: use expanded_body in prompt_for_task for supporting files (#34)"
```

---

### Task 6: Full build and test verification

- [ ] **Step 1: Run all workspace tests**

Run: `cargo test --workspace 2>&1 | tail -30`
Expected: all tests pass

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --workspace -- -D warnings 2>&1 | tail -10`
Expected: no warnings

- [ ] **Step 3: Verify with real superpower skills**

Copy a superpower skill to `.dcode-ai/skills/` and verify expansion works:

```bash
mkdir -p .dcode-ai/skills/test-skill
cp /home/nst/Documents/superpower/skills/subagent-driven-development/SKILL.md .dcode-ai/skills/test-skill/
cp /home/nst/Documents/superpower/skills/subagent-driven-development/implementer-prompt.md .dcode-ai/skills/test-skill/
cp /home/nst/Documents/superpower/skills/subagent-driven-development/spec-reviewer-prompt.md .dcode-ai/skills/test-skill/
cp /home/nst/Documents/superpower/skills/subagent-driven-development/code-quality-reviewer-prompt.md .dcode-ai/skills/test-skill/
```

Then invoke the skill and verify the prompt includes the supporting file contents.

- [ ] **Step 4: Clean up test files**

```bash
rm -rf .dcode-ai/skills/test-skill
```

- [ ] **Step 5: Commit if any cleanup needed**

```bash
git add -A && git commit -m "chore: skill supporting files - final verification (#34)"
```
