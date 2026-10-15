//! One-line summary of what a plugin adds.

use dcode_ai_common::plugins::Plugin;

/// One line per contribution kind, e.g. `3 commands · 1 skill · 1 MCP server · 2 hooks`.
pub fn contributions(plugin: &Plugin) -> String {
    let count_md = |dir: &std::path::Path| -> usize {
        fn walk(d: &std::path::Path, depth: usize) -> usize {
            let Ok(entries) = std::fs::read_dir(d) else {
                return 0;
            };
            entries
                .flatten()
                .map(|e| {
                    let p = e.path();
                    if p.is_dir() && depth < 4 {
                        walk(&p, depth + 1)
                    } else {
                        usize::from(p.extension().and_then(|x| x.to_str()) == Some("md"))
                    }
                })
                .sum()
        }
        walk(dir, 0)
    };
    let commands: usize = plugin.command_dirs().iter().map(|d| count_md(d)).sum();
    let skills = std::fs::read_dir(plugin.skills_dir())
        .map(|e| {
            e.flatten()
                .filter(|e| e.path().join("SKILL.md").exists())
                .count()
        })
        .unwrap_or(0);
    let agents = std::fs::read_dir(plugin.root.join("agents"))
        .map(|e| {
            e.flatten()
                .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
                .count()
        })
        .unwrap_or(0);
    let mcp = plugin.mcp_servers().len();
    let h = plugin.hooks();
    let hooks = h.session_start.len()
        + h.session_end.len()
        + h.pre_tool_use.len()
        + h.post_tool_use.len()
        + h.post_tool_failure.len()
        + h.approval_requested.len()
        + h.subagent_start.len()
        + h.subagent_stop.len()
        + h.user_prompt_submit.len()
        + h.stop.len()
        + h.pre_compact.len();
    let mut parts = Vec::new();
    let mut add = |n: usize, one: &str, many: &str| {
        if n > 0 {
            parts.push(format!("{n} {}", if n == 1 { one } else { many }));
        }
    };
    add(commands, "command", "commands");
    add(skills, "skill", "skills");
    add(agents, "agent", "agents");
    add(mcp, "MCP server", "MCP servers");
    add(hooks, "hook", "hooks");
    if parts.is_empty() {
        "nothing to load".into()
    } else {
        parts.join(" · ")
    }
}
