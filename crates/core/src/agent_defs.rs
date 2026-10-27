//! Custom sub-agent definitions: Markdown files with YAML frontmatter, in the
//! Claude Code format, that `spawn_subagent` can run by name.
//!
//! ```markdown
//! ---
//! name: test-runner
//! description: Runs the test suite and fixes failures. Use after code changes.
//! tools: Read, Grep, Bash, Edit      # optional; omit for every tool
//! model: sonnet                      # optional; `inherit` or omitted = parent's
//! ---
//! You are a test specialist. Run the tests, read failures, fix them …
//! ```
//!
//! Looked up in `~/.dcode-ai/agents`, `~/.claude/agents`, `.dcode-ai/agents`,
//! `.claude/agents` (later wins) and enabled plugins' `agents/`
//! (`plugin:name`).

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentDef {
    pub name: String,
    pub description: String,
    /// dcode-ai tool names the agent may use; `None` = all tools.
    pub tools: Option<Vec<String>>,
    /// Model alias or id; `None` = inherit the parent's model.
    pub model: Option<String>,
    /// System instructions (the Markdown body).
    pub prompt: String,
    pub path: PathBuf,
}

#[derive(Debug, Default, Deserialize)]
struct Frontmatter {
    name: Option<String>,
    description: Option<String>,
    #[serde(default)]
    tools: Option<serde_yaml::Value>,
    model: Option<String>,
}

/// dcode-ai tools for one name in a `tools:` list. Claude Code names map to
/// their dcode-ai equivalents; dcode-ai names pass through.
fn map_tool(name: &str) -> Vec<&str> {
    match name {
        "Read" => vec!["read_file"],
        "Write" => vec!["write_file", "create_directory"],
        "Edit" | "MultiEdit" => vec!["edit_file", "replace_match", "apply_patch"],
        "NotebookEdit" => vec!["notebook_edit"],
        "Bash" => vec!["execute_bash", "run_background", "run_validation"],
        "Grep" | "Glob" => vec!["search_code", "repo_map", "query_symbols"],
        "LS" => vec!["list_directory"],
        "WebFetch" => vec!["fetch_url"],
        "WebSearch" => vec!["web_search"],
        "TodoWrite" => vec!["update_plan"],
        "Task" => vec!["spawn_subagent"],
        "NotebookRead" => vec!["read_file"],
        other => vec![other],
    }
}

fn parse_tools(value: &serde_yaml::Value) -> Option<Vec<String>> {
    let names: Vec<String> = match value {
        serde_yaml::Value::String(s) => s.split(',').map(|t| t.trim().to_string()).collect(),
        serde_yaml::Value::Sequence(seq) => seq
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
            .collect(),
        _ => return None,
    };
    let mut out: Vec<String> = Vec::new();
    for n in names.iter().filter(|n| !n.is_empty()) {
        if n == "*" {
            return None;
        }
        // MCP tools (`mcp__server__tool`) pass through unchanged.
        for t in map_tool(n) {
            if !out.iter().any(|o| o == t) {
                out.push(t.to_string());
            }
        }
    }
    Some(out)
}

pub fn parse_agent_file(path: &Path) -> Result<AgentDef, String> {
    let raw = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let raw = raw.replace("\r\n", "\n");
    let (fm, body) = match raw
        .strip_prefix("---\n")
        .and_then(|r| r.split_once("\n---"))
    {
        Some((yaml, rest)) => (
            serde_yaml::from_str::<Frontmatter>(yaml)
                .map_err(|e| format!("{}: {e}", path.display()))?,
            rest.trim_start_matches('-').to_string(),
        ),
        None => (Frontmatter::default(), raw.clone()),
    };
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("agent")
        .to_string();
    let model = fm
        .model
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty() && m != "inherit");
    Ok(AgentDef {
        name: fm.name.unwrap_or(stem).trim().to_string(),
        description: fm.description.unwrap_or_default().trim().to_string(),
        tools: fm.tools.as_ref().and_then(parse_tools),
        model,
        prompt: body.trim().to_string(),
        path: path.to_path_buf(),
    })
}

fn collect(dir: &Path, prefix: Option<&str>, out: &mut Vec<AgentDef>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        match parse_agent_file(&path) {
            Ok(mut def) => {
                if let Some(p) = prefix {
                    def.name = format!("{p}:{}", def.name);
                }
                // Later roots override earlier ones (project beats user).
                out.retain(|d| d.name != def.name);
                out.push(def);
            }
            Err(e) => tracing::warn!("agent definition skipped: {e}"),
        }
    }
}

/// All agent definitions visible from `workspace_root`.
pub fn discover(
    workspace_root: &Path,
    plugins: &[dcode_ai_common::plugins::Plugin],
) -> Vec<AgentDef> {
    let mut out = Vec::new();
    if let Some(home) = dcode_ai_common::config::home_dir() {
        collect(&home.join(".claude/agents"), None, &mut out);
        collect(&home.join(".dcode-ai/agents"), None, &mut out);
    }
    collect(&workspace_root.join(".claude/agents"), None, &mut out);
    collect(&workspace_root.join(".dcode-ai/agents"), None, &mut out);
    for plugin in plugins {
        for dir in plugin.agent_dirs() {
            collect(&dir, Some(plugin.name()), &mut out);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Built-in read-only reviewer used by `/verify`.
pub fn verifier() -> AgentDef {
    AgentDef {
        name: "verifier".into(),
        description: "Reviews uncommitted changes for bugs and runs the project's checks".into(),
        tools: Some(
            [
                "read_file",
                "search_code",
                "list_directory",
                "git_status",
                "git_diff",
                "query_symbols",
                "code_intel",
                "repo_map",
                "run_validation",
            ]
            .iter()
            .map(|t| t.to_string())
            .collect(),
        ),
        model: None,
        prompt: VERIFIER_PROMPT.trim().to_string(),
        path: PathBuf::new(),
    }
}

/// `/second-opinion`: the verifier, run on a model from a different family.
pub fn second_opinion() -> AgentDef {
    let mut def = verifier();
    def.name = "second-opinion".into();
    def.description = "Independent review of uncommitted changes by a different model".into();
    def.prompt = format!(
        "{}\nYou are a different model from the one that wrote this change. Do not \
assume its reasoning was right: re-derive what the code must do, check the cases \
it may have missed, and question choices that look plausible but are wrong.",
        def.prompt
    );
    def
}

/// `/security-review`: a read-only reviewer looking only for exploitable
/// vulnerabilities in the change.
pub fn security_reviewer() -> AgentDef {
    let mut def = verifier();
    def.name = "security-reviewer".into();
    def.description = "Security review of the pending change".into();
    def.prompt = SECURITY_PROMPT.trim().to_string();
    def
}

const SECURITY_PROMPT: &str = r#"
You are a senior application-security engineer reviewing a code change. You
cannot edit files. Review ONLY what the change introduces or touches:
1. Read the diff (git_diff, or the base comparison given in the task) and the
   code paths it reaches.
2. Look for exploitable issues: injection (SQL, command, template, path
   traversal), broken authentication or authorization, secrets or tokens in
   code or logs, unsafe deserialization, SSRF, XSS / unsafe HTML, weak or
   misused cryptography, insecure defaults, missing input validation on a
   trust boundary, unsafe `unsafe` code or memory handling.
3. Ignore denial of service, rate limiting, missing hardening with no concrete
   exploit, and anything in tests or docs only.
Report only findings you are confident are real, most severe first:
`[HIGH|MEDIUM|LOW] path:line — vulnerability — how it is exploited — fix`.
If nothing qualifies, reply exactly: `No security issues found.` and list what
you checked.
"#;

const VERIFIER_PROMPT: &str = r#"
You are a careful code reviewer verifying another agent's uncommitted changes.
You cannot edit files. Steps:
1. Read the diff (git_diff) and the surrounding code it touches.
2. Run the project's checks with run_validation (build, tests, lint) where they exist.
3. Look for real defects: logic errors, broken edge cases, missing error handling,
   API misuse, tests that no longer pass, security problems.
Report ONLY concrete problems, most severe first, one per line:
`path:line — problem — suggested fix`. Do not report style nits.
If everything checks out, reply exactly: `No problems found.` followed by what you ran.
"#;

/// Find an agent by exact name (or by its unprefixed name when unique).
pub fn find<'a>(defs: &'a [AgentDef], name: &str) -> Option<&'a AgentDef> {
    defs.iter().find(|d| d.name == name).or_else(|| {
        let mut it = defs
            .iter()
            .filter(|d| d.name.rsplit(':').next() == Some(name));
        let first = it.next()?;
        it.next().is_none().then_some(first)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_claude_style_agent() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        std::fs::create_dir_all(ws.join(".claude/agents")).unwrap();
        std::fs::write(
            ws.join(".claude/agents/tester.md"),
            "---\nname: test-runner\ndescription: Runs tests\ntools: Read, Grep, Bash\nmodel: inherit\n---\nRun the tests.\n",
        )
        .unwrap();
        std::fs::create_dir_all(ws.join(".dcode-ai/agents")).unwrap();
        std::fs::write(
            ws.join(".dcode-ai/agents/reviewer.md"),
            "---\ndescription: Reviews diffs\nmodel: opus\ntools: [read_file, \"mcp__gh__pr\"]\n---\nReview.",
        )
        .unwrap();

        let defs = discover(ws, &[]);
        let t = find(&defs, "test-runner").unwrap();
        assert_eq!(t.description, "Runs tests");
        assert_eq!(t.model, None);
        let tools = t.tools.clone().unwrap();
        assert!(tools.contains(&"read_file".to_string()));
        assert!(tools.contains(&"search_code".to_string()));
        assert!(tools.contains(&"execute_bash".to_string()));
        assert!(!tools.contains(&"write_file".to_string()));
        assert_eq!(t.prompt, "Run the tests.");

        let r = find(&defs, "reviewer").unwrap();
        assert_eq!(r.model.as_deref(), Some("opus"));
        assert_eq!(
            r.tools.clone().unwrap(),
            vec!["read_file".to_string(), "mcp__gh__pr".to_string()]
        );
    }

    #[test]
    fn missing_tools_means_all_and_star_means_all() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.md");
        std::fs::write(&p, "---\nname: a\ntools: \"*\"\n---\nx").unwrap();
        assert_eq!(parse_agent_file(&p).unwrap().tools, None);
        std::fs::write(&p, "plain body only").unwrap();
        let d = parse_agent_file(&p).unwrap();
        assert_eq!((d.name.as_str(), d.tools.clone()), ("a", None));
    }

    #[test]
    fn find_accepts_unique_unprefixed_plugin_names() {
        let def = |n: &str| AgentDef {
            name: n.into(),
            description: String::new(),
            tools: None,
            model: None,
            prompt: String::new(),
            path: PathBuf::new(),
        };
        let defs = vec![def("demo:helper"), def("x:dup"), def("y:dup")];
        assert_eq!(find(&defs, "helper").unwrap().name, "demo:helper");
        assert!(find(&defs, "dup").is_none());
    }
}
