//! Renderer-agnostic description of a tool call's display payload.
//!
//! Ported from koda-core/src/tools/summary.rs (MIT, koda project).
//! Single source of truth for "what arg keys does each tool use?" —
//! every koda-style renderer pattern-matches on `ToolCallKind` instead
//! of poking at the raw JSON, so renderers can't drift on which arg
//! key means "the path."

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallSummary {
    pub name: String,
    pub kind: ToolCallKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolCallKind {
    Bash {
        command: String,
    },
    Path {
        path: String,
    },
    Grep {
        pattern: String,
        dir: String,
    },
    Glob {
        pattern: String,
        base: Option<String>,
    },
    List {
        dir: String,
    },
    WebFetch {
        url: String,
    },
    Generic {
        value: Option<String>,
    },
}

impl ToolCallSummary {
    pub fn from_call(name: &str, args: &Value) -> Self {
        let kind = match name {
            "Bash" => ToolCallKind::Bash {
                command: first_string(args, &["command", "cmd"]).unwrap_or_default(),
            },
            "Read" | "Write" | "Edit" | "Delete" => ToolCallKind::Path {
                path: first_string(args, &["file_path", "path"]).unwrap_or_default(),
            },
            "Grep" => ToolCallKind::Grep {
                pattern: first_string(args, &["search_string", "pattern"]).unwrap_or_default(),
                dir: first_string(args, &["file_path", "path", "directory"])
                    .unwrap_or_else(|| ".".to_string()),
            },
            "Glob" => ToolCallKind::Glob {
                pattern: first_string(args, &["pattern"]).unwrap_or_default(),
                base: first_string(args, &["file_path", "path", "directory"]),
            },
            "List" => ToolCallKind::List {
                dir: first_string(args, &["file_path", "path", "directory"])
                    .unwrap_or_else(|| ".".to_string()),
            },
            "WebFetch" => ToolCallKind::WebFetch {
                url: first_string(args, &["url"]).unwrap_or_default(),
            },
            _ => ToolCallKind::Generic {
                value: first_string_in_object(args),
            },
        };
        Self {
            name: name.to_string(),
            kind,
        }
    }
}

fn first_string(args: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| args.get(k).and_then(|v| v.as_str()).map(|s| s.to_string()))
}

fn first_string_in_object(args: &Value) -> Option<String> {
    args.as_object()?
        .iter()
        .find_map(|(_, v)| v.as_str().map(|s| s.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn bash_reads_command_key() {
        let s = ToolCallSummary::from_call("Bash", &json!({ "command": "ls -la" }));
        assert_eq!(
            s.kind,
            ToolCallKind::Bash {
                command: "ls -la".into()
            }
        );
    }

    #[test]
    fn bash_falls_back_to_cmd_alias() {
        let s = ToolCallSummary::from_call("Bash", &json!({ "cmd": "echo hi" }));
        assert_eq!(
            s.kind,
            ToolCallKind::Bash {
                command: "echo hi".into()
            }
        );
    }

    #[test]
    fn path_family_share_shape() {
        for name in ["Read", "Write", "Edit", "Delete"] {
            let s = ToolCallSummary::from_call(name, &json!({ "file_path": "src/foo.rs" }));
            assert_eq!(
                s.kind,
                ToolCallKind::Path {
                    path: "src/foo.rs".into()
                }
            );
        }
    }

    #[test]
    fn grep_with_pattern_alias() {
        let live = ToolCallSummary::from_call("Grep", &json!({"pattern": "x"}));
        let history = ToolCallSummary::from_call("Grep", &json!({"search_string": "x"}));
        assert_eq!(live.kind, history.kind);
    }

    #[test]
    fn grep_uses_file_path_key_from_schema() {
        let s =
            ToolCallSummary::from_call("Grep", &json!({"pattern": "TODO", "file_path": "src/lib"}));
        if let ToolCallKind::Grep { pattern, dir } = s.kind {
            assert_eq!(pattern, "TODO");
            assert_eq!(dir, "src/lib");
        } else {
            panic!("expected Grep kind");
        }
    }

    #[test]
    fn glob_surfaces_file_path_when_present() {
        let s =
            ToolCallSummary::from_call("Glob", &json!({"pattern": "**/*.rs", "file_path": "src"}));
        if let ToolCallKind::Glob { pattern, base } = s.kind {
            assert_eq!(pattern, "**/*.rs");
            assert_eq!(base.as_deref(), Some("src"));
        } else {
            panic!("expected Glob kind");
        }
    }

    #[test]
    fn list_default_dir_is_dot() {
        let s = ToolCallSummary::from_call("List", &json!({}));
        if let ToolCallKind::List { dir } = s.kind {
            assert_eq!(dir, ".");
        } else {
            panic!("expected List kind");
        }
    }

    #[test]
    fn generic_falls_back_to_first_string() {
        let s = ToolCallSummary::from_call("Unknown", &json!({"a": 1, "b": "hello"}));
        if let ToolCallKind::Generic { value } = s.kind {
            assert_eq!(value.as_deref(), Some("hello"));
        } else {
            panic!("expected Generic kind");
        }
    }
}
