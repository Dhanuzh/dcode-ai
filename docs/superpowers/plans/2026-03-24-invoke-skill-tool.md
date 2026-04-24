# Invoke Skill Tool Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an `invoke_skill` tool that the LLM can call to dynamically load a skill's full instructions.

**Architecture:** Create a new `InvokeSkillTool` struct implementing `ToolExecutor`, register it post-construction in `supervisor.rs` (for both safe and full modes), and update the system prompt footer to tell the LLM to use the tool.

**Tech Stack:** Rust, async-trait

**Spec:** `docs/superpowers/specs/2026-03-24-invoke-skill-tool-design.md`

---

### Task 1: Create `invoke_skill.rs` tool implementation

**Files:**
- Create: `crates/core/src/tools/invoke_skill.rs`

- [ ] **Step 1: Create the tool file with struct and constructor**

Create `crates/core/src/tools/invoke_skill.rs`:

```rust
//! Tool that lets the LLM load a skill's full instructions by name.

use crate::skills::SkillCatalog;
use crate::tools::ToolExecutor;
use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};
use std::path::PathBuf;

pub struct InvokeSkillTool {
    workspace_root: PathBuf,
    skill_directories: Vec<PathBuf>,
}

impl InvokeSkillTool {
    pub fn new(workspace_root: PathBuf, skill_directories: Vec<PathBuf>) -> Self {
        Self {
            workspace_root,
            skill_directories,
        }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for InvokeSkillTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "invoke_skill".into(),
            description: "Load a skill's full instructions by name. Use this when a task matches \
                an available skill from the skills manifest. Returns the complete skill \
                instructions to follow."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "skill_name": {
                        "type": "string",
                        "description": "The command name from the skills manifest (e.g., 'brainstorming', 'test-driven-development')"
                    }
                },
                "required": ["skill_name"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let skill_name = call.input["skill_name"]
            .as_str()
            .unwrap_or("")
            .trim()
            .to_string();

        if skill_name.is_empty() {
            return ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some("skill_name is required".into()),
            };
        }

        let skills = match SkillCatalog::discover(&self.workspace_root, &self.skill_directories) {
            Ok(s) => s,
            Err(e) => {
                return ToolResult {
                    call_id: call.id.clone(),
                    success: false,
                    output: String::new(),
                    error: Some(format!("Failed to discover skills: {e}")),
                };
            }
        };

        if let Some(skill) = skills.iter().find(|s| s.command == skill_name) {
            let body = skill.expanded_body();
            ToolResult {
                call_id: call.id.clone(),
                success: true,
                output: format!(
                    "Skill `{}` loaded. Follow these instructions:\n\n{}",
                    skill.command,
                    body.trim()
                ),
                error: None,
            }
        } else {
            let available: Vec<&str> = skills.iter().map(|s| s.command.as_str()).collect();
            ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Skill '{}' not found. Available skills: {}",
                    skill_name,
                    available.join(", ")
                )),
            }
        }
    }
}
```

- [ ] **Step 2: Verify it compiles**

Run: `cargo check -p dcode-ai-core 2>&1 | tail -5`
Expected: may warn about dead code (not registered yet), but no errors

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/tools/invoke_skill.rs
git commit -m "feat: add invoke_skill tool implementation (#34)"
```

---

### Task 2: Export module and register the tool

**Files:**
- Modify: `crates/core/src/tools/mod.rs:1-23` (add module and re-export)
- Modify: `crates/runtime/src/supervisor.rs:148-155` (register tool)

- [ ] **Step 1: Add module declaration and re-export in `mod.rs`**

In `crates/core/src/tools/mod.rs`, add after `pub mod ask_question;` (line 2):

```rust
pub mod invoke_skill;
```

And after `pub use ask_question::AskQuestionTool;` (line 23), add:

```rust
pub use invoke_skill::InvokeSkillTool;
```

- [ ] **Step 2: Register `InvokeSkillTool` in `supervisor.rs`**

In `crates/runtime/src/supervisor.rs`, add the import at the top (after the existing `use dcode_ai_core::tools::AskQuestionTool;` on line 20):

```rust
use dcode_ai_core::tools::InvokeSkillTool;
```

Then add the registration after the `AskQuestionTool` block (after line 183), following the same post-construction pattern:

```rust
tools.register(Box::new(InvokeSkillTool::new(
    workspace_root.clone(),
    config.harness.skill_directories.clone(),
)));
```

Note: `config.harness.skill_directories` is a `Vec<PathBuf>` field on `DcodeAiConfig.harness` — the same field used by `build_system_prompt` in `harness.rs`. This registers in both safe and full modes since the tool is read-only.

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p dcode-ai-runtime 2>&1 | tail -5`
Expected: no errors

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/tools/mod.rs crates/runtime/src/supervisor.rs
git commit -m "feat: register invoke_skill tool in supervisor (#34)"
```

---

### Task 3: Update system prompt skills footer

**Files:**
- Modify: `crates/core/src/harness.rs:144-146`

- [ ] **Step 1: Update the footer text**

In `crates/core/src/harness.rs`, change line 144-146 from:

```rust
    section.push_str(
        "\nUse these skill summaries when relevant. Full skill instructions are loaded only when explicitly invoked by the user or REPL.",
    );
```

To:

```rust
    section.push_str(
        "\nUse the invoke_skill tool to load full instructions when a task matches a skill.",
    );
```

- [ ] **Step 2: Search for any test asserting the old footer string**

Run: `grep -n "explicitly invoked\|Full skill instructions" crates/core/src/harness.rs`

If any test asserts the old footer text, update it to match the new string. The `layers_sections_in_stable_order` test only asserts `"Available Skills:"` (section header), which is unchanged. But verify no other assertion references the old footer.

Then run: `cargo test -p dcode-ai-core layers_sections 2>&1 | tail -10`
Expected: PASS

- [ ] **Step 3: Verify full build**

Run: `cargo check --workspace 2>&1 | tail -5`
Expected: no errors

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/harness.rs
git commit -m "feat: update skills manifest footer to reference invoke_skill tool (#34)"
```

---

### Task 4: Add tests for invoke_skill tool

**Files:**
- Modify: `crates/core/src/tools/invoke_skill.rs` (add test module)

- [ ] **Step 1: Add test module to `invoke_skill.rs`**

Append to the end of `crates/core/src/tools/invoke_skill.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn make_tool(workspace: &std::path::Path) -> InvokeSkillTool {
        InvokeSkillTool::new(
            workspace.to_path_buf(),
            vec![std::path::PathBuf::from(".dcode-ai/skills")],
        )
    }

    fn make_call(skill_name: &str) -> ToolCall {
        ToolCall {
            id: "call-1".into(),
            name: "invoke_skill".into(),
            input: serde_json::json!({ "skill_name": skill_name }),
        }
    }

    #[test]
    fn definition_has_correct_name_and_parameters() {
        let dir = tempfile::tempdir().unwrap();
        let tool = make_tool(dir.path());
        let def = tool.definition();
        assert_eq!(def.name, "invoke_skill");
        assert!(def.description.contains("Load a skill"));
        assert!(def.parameters["properties"]["skill_name"].is_object());
        assert!(def.parameters["required"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("skill_name")));
    }

    #[tokio::test]
    async fn returns_expanded_body_for_valid_skill() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join(".dcode-ai/skills/my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: My Skill\ncommand: my-skill\ndescription: A test skill\n---\nDo the thing.\n\nSee ./helper.md for details.\n",
        )
        .unwrap();
        std::fs::write(skill_dir.join("helper.md"), "Helper content.").unwrap();

        let tool = make_tool(dir.path());
        let result = tool.execute(&make_call("my-skill")).await;

        assert!(result.success);
        assert!(result.output.contains("Skill `my-skill` loaded"));
        assert!(result.output.contains("Do the thing."));
        assert!(result.output.contains("===== helper.md ====="));
        assert!(result.output.contains("Helper content."));
    }

    #[tokio::test]
    async fn returns_error_for_unknown_skill() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join(".dcode-ai/skills/real-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: Real\ncommand: real-skill\n---\nReal body.\n",
        )
        .unwrap();

        let tool = make_tool(dir.path());
        let result = tool.execute(&make_call("nonexistent")).await;

        assert!(!result.success);
        let err = result.error.unwrap();
        assert!(err.contains("not found"));
        assert!(err.contains("real-skill"));
    }

    #[tokio::test]
    async fn returns_error_for_empty_skill_name() {
        let dir = tempfile::tempdir().unwrap();
        let tool = make_tool(dir.path());
        let result = tool.execute(&make_call("")).await;

        assert!(!result.success);
        assert!(result.error.unwrap().contains("skill_name is required"));
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p dcode-ai-core invoke_skill 2>&1 | tail -20`
Expected: all 4 tests pass

- [ ] **Step 3: Commit**

```bash
git add crates/core/src/tools/invoke_skill.rs
git commit -m "test: add invoke_skill tool tests (#34)"
```

---

### Task 5: Full build and test verification

- [ ] **Step 1: Run all workspace tests**

Run: `cargo test --workspace 2>&1 | tail -30`
Expected: all tests pass

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --workspace -- -D warnings 2>&1 | tail -10`
Expected: no warnings

- [ ] **Step 3: Commit if any cleanup needed**

```bash
git add -A && git commit -m "chore: invoke_skill tool - final verification (#34)"
```
