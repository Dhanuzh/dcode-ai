//! `dcode-ai policy`: show permission rules and test what a tool call would do.

use clap::Subcommand;
use dcode_ai_common::config::{DcodeAiConfig, PolicyRules};
use dcode_ai_common::tool::PermissionTier;
use dcode_ai_core::approval::ApprovalPolicy;

#[derive(Debug, Subcommand)]
pub enum PolicyCommand {
    /// List the rules in effect (config.toml and policy.toml files)
    Show,
    /// What would happen to a tool call: `policy test execute_bash "git push"`
    Test {
        /// Tool name, e.g. execute_bash, write_file, delete_path
        tool: String,
        /// The command, path, or raw JSON input
        input: Option<String>,
    },
}

/// Tool input JSON for a convenient CLI argument.
pub fn input_json(tool: &str, input: Option<&str>) -> String {
    let Some(raw) = input else {
        return "{}".into();
    };
    if serde_json::from_str::<serde_json::Value>(raw).is_ok_and(|v| v.is_object()) {
        return raw.to_string();
    }
    let key = match tool {
        "execute_bash" | "run_background" | "run_validation" => "command",
        "fetch_url" => "url",
        "web_search" | "search_code" => "query",
        _ => "path",
    };
    serde_json::json!({ key: raw }).to_string()
}

pub fn tier_label(tier: PermissionTier) -> &'static str {
    match tier {
        PermissionTier::Allowed => "allow",
        PermissionTier::Ask => "ask",
        PermissionTier::Denied => "deny",
    }
}

pub fn run(config: &DcodeAiConfig, workspace_root: &std::path::Path, cmd: PolicyCommand) {
    match cmd {
        PolicyCommand::Show => {
            let p = &config.permissions;
            println!("mode: {:?}", p.mode);
            let show = |name: &str, cfg: &[String], pol: &[(String, String)]| {
                for r in cfg {
                    println!("  {name:<5} {r}   (config.toml)");
                }
                for (r, origin) in pol {
                    println!("  {name:<5} {r}   ({origin})");
                }
            };
            show("deny", &p.deny, &p.policy.deny);
            show("ask", &p.ask, &p.policy.ask);
            show("allow", &p.allow, &p.policy.allow);
            println!("policy files (deny > ask > allow > mode default):");
            for path in PolicyRules::paths(workspace_root) {
                let mark = if path.exists() { "●" } else { "○" };
                println!("  {mark} {}", path.display());
            }
        }
        PolicyCommand::Test { tool, input } => {
            let policy = ApprovalPolicy::new(config.permissions.clone());
            let (tier, why) = policy.explain(&tool, &input_json(&tool, input.as_deref()));
            println!("{}: {why}", tier_label(tier));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_json_wraps_plain_arguments() {
        assert_eq!(
            input_json("execute_bash", Some("git push")),
            r#"{"command":"git push"}"#
        );
        assert_eq!(
            input_json("write_file", Some("a.txt")),
            r#"{"path":"a.txt"}"#
        );
        assert_eq!(input_json("x", Some(r#"{"a":1}"#)), r#"{"a":1}"#);
    }

    #[test]
    fn policy_files_deny_ask_and_allow_with_reasons() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".dcode-ai")).unwrap();
        let path = dir.path().join(".dcode-ai/policy.toml");
        std::fs::write(
            &path,
            "deny = [\"execute_bash:git push --force*\"]\nask = [\"execute_bash:git push*\"]\nallow = [\"execute_bash:cargo test*\"]\n",
        )
        .unwrap();
        let mut config = DcodeAiConfig::default();
        config.permissions.policy.load_file(&path).unwrap();
        let policy = ApprovalPolicy::new(config.permissions);
        let check =
            |cmd: &str| policy.explain("execute_bash", &input_json("execute_bash", Some(cmd)));

        let (t, why) = check("git push --force origin main");
        assert_eq!(t, PermissionTier::Denied);
        assert!(
            why.contains("git push --force*") && why.contains("policy.toml"),
            "{why}"
        );
        assert_eq!(check("git push origin main").0, PermissionTier::Ask);
        let (t, why) = check("cargo test -p x");
        assert_eq!(t, PermissionTier::Allowed);
        assert!(why.starts_with("allowed by rule"), "{why}");
        // Read-only commands stay allowed by the mode default.
        assert_eq!(check("git status").0, PermissionTier::Allowed);
    }
}
