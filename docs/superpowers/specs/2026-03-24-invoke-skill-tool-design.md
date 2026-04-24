# Invoke Skill Tool Design

**Issue:** [#34](https://github.com/madebyaris/dcode-ai/issues/34) (sub-project 2 of 4)
**Date:** 2026-03-24
**Status:** Approved

## Summary

Add an `invoke_skill` tool that the LLM can call to dynamically load a skill's full instructions. This enables the LLM to auto-invoke skills when a task matches, and enables cross-skill chaining (skills telling the LLM to invoke other skills).

## Motivation

Currently, skills can only be invoked by the user typing `/command`. The system prompt includes a skills manifest (name + one-line description) but the LLM has no way to load full skill instructions on its own. This means skills like superpowers' `using-superpowers` — which instructs the LLM to invoke other skills — cannot work.

## Design

### Tool Definition

```
name: "invoke_skill"
description: "Load a skill's full instructions by name. Use this when a task matches
              an available skill from the skills manifest. Returns the complete skill
              instructions to follow."
parameters:
  type: object
  properties:
    skill_name:
      type: string
      description: "The command name from the skills manifest (e.g., 'brainstorming', 'test-driven-development')"
  required: ["skill_name"]
```

### Tool Behavior

**On success:** Returns the expanded skill body (with inlined supporting files from sub-project 1):
```
Skill `{command}` loaded. Follow these instructions:

{expanded_body}
```

**On failure:** Returns error with available skill names:
```
Skill '{name}' not found. Available skills: brainstorming, test-driven-development, writing-plans, ...
```

**No side effects:** The tool only returns text. It does NOT change model, permission mode, or any session state. Model/permission overrides only apply via explicit user `/command` invocation.

### Implementation Structure

**New file:** `crates/core/src/tools/invoke_skill.rs`

Follows the `ToolExecutor` trait pattern (same as `ask_question.rs`):

```rust
pub struct InvokeSkillTool {
    workspace_root: PathBuf,
    skill_directories: Vec<PathBuf>,
}
```

The `execute()` method:
1. Parse `skill_name` from `call.input`
2. Call `SkillCatalog::discover()` with stored workspace root and skill directories
3. Find matching skill by `command` field
4. Return `skill.expanded_body()` as the tool result
5. On no match, return error listing available skill commands

### Registration

Register `InvokeSkillTool` **post-construction** in `crates/runtime/src/supervisor.rs`, following the same pattern used for `AskQuestionTool` (lines ~180-183). Do NOT modify the `with_default_readonly_tools` / `with_default_full_tools` factory signatures.

Since `invoke_skill` is a pure read operation (no side effects), register it in **both** readonly and full tool modes — it should be available even in plan/safe mode.

The tool needs `workspace_root` (already available at the registration site) and `skill_directories` (from `config.harness.skill_directories`, same field already used by `build_system_prompt`).

### System Prompt Update

Change the skills manifest footer in `crates/core/src/harness.rs` (line ~144-146) from:

```
"Use these skill summaries when relevant. Full skill instructions are loaded only when explicitly invoked by the user or REPL."
```

To:

```
"Use the invoke_skill tool to load full instructions when a task matches a skill."
```

### What stays the same

- `SkillCatalog::discover()` — unchanged
- `expanded_body()` — reused from sub-project 1
- `/command` slash invocation — still works via REPL with model/permission overrides
- All other tools — unchanged
- Skills manifest format in system prompt — only footer text changes

### Testing

- Unit test: `invoke_skill` with valid skill name returns expanded body containing skill instructions
- Unit test: `invoke_skill` with unknown skill name returns error listing available skills
- Unit test: tool definition has correct name, description, and parameter schema
- Existing tests: `harness.rs` tests that assert system prompt content **will** need the footer text updated to match the new string

## Files Affected

- Create: `crates/core/src/tools/invoke_skill.rs` — new tool implementation
- Modify: `crates/core/src/tools/mod.rs` — export new module
- Modify: tool registration site — register `InvokeSkillTool`
- Modify: `crates/core/src/harness.rs:144-146` — update skills manifest footer
