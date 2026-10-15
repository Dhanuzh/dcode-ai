//! Language servers for code intelligence: built-ins, user / project
//! `lsp.json` files and plugin `lspServers`, plus picking the one that fits
//! a workspace.
//!
//! File format (Claude Code's plugin `lspServers` / `.lsp.json`):
//!
//! ```json
//! { "lspServers": {
//!     "pyright": { "command": "pyright-langserver", "args": ["--stdio"],
//!                  "extensionToLanguage": { ".py": "python", ".pyi": "python" } } } }
//! ```

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspServerSpec {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    /// File extension (with dot, lowercase) → LSP `languageId`.
    pub extensions: BTreeMap<String, String>,
}

impl LspServerSpec {
    fn builtin(name: &str, command: &str, args: &[&str], exts: &[(&str, &str)]) -> Self {
        Self {
            name: name.into(),
            command: command.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            extensions: exts
                .iter()
                .map(|(e, l)| (e.to_string(), l.to_string()))
                .collect(),
        }
    }

    /// `languageId` for a file, defaulting to the server's first language.
    pub fn language_id(&self, path: &Path) -> String {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{}", e.to_ascii_lowercase()));
        ext.and_then(|e| self.extensions.get(&e).cloned())
            .or_else(|| self.extensions.values().next().cloned())
            .unwrap_or_else(|| "plaintext".into())
    }
}

/// Servers dcode-ai knows without configuration.
pub fn builtin_servers() -> Vec<LspServerSpec> {
    vec![
        LspServerSpec::builtin("rust-analyzer", "rust-analyzer", &[], &[(".rs", "rust")]),
        LspServerSpec::builtin(
            "typescript",
            "typescript-language-server",
            &["--stdio"],
            &[
                (".ts", "typescript"),
                (".tsx", "typescriptreact"),
                (".js", "javascript"),
                (".jsx", "javascriptreact"),
                (".mjs", "javascript"),
                (".cjs", "javascript"),
            ],
        ),
        LspServerSpec::builtin(
            "pyright",
            "pyright-langserver",
            &["--stdio"],
            &[(".py", "python"), (".pyi", "python")],
        ),
        LspServerSpec::builtin("gopls", "gopls", &[], &[(".go", "go")]),
        LspServerSpec::builtin(
            "clangd",
            "clangd",
            &[],
            &[
                (".c", "c"),
                (".h", "c"),
                (".cc", "cpp"),
                (".cpp", "cpp"),
                (".hpp", "cpp"),
            ],
        ),
    ]
}

/// Parse an `lspServers` map (or a document holding one). `vars` are
/// `${…}` substitutions such as `CLAUDE_PLUGIN_ROOT`.
pub fn parse_servers(doc: &Value, vars: &BTreeMap<String, String>) -> Vec<LspServerSpec> {
    let map = doc
        .get("lspServers")
        .unwrap_or(doc)
        .as_object()
        .cloned()
        .unwrap_or_default();
    let x = |s: &str| crate::mcp_json::expand_vars(s, vars);
    map.into_iter()
        .filter_map(|(name, spec)| {
            let command = x(spec.get("command")?.as_str()?);
            let args = spec
                .get("args")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).map(x).collect())
                .unwrap_or_default();
            let extensions: BTreeMap<String, String> = spec
                .get("extensionToLanguage")
                .and_then(Value::as_object)
                .map(|m| {
                    m.iter()
                        .filter_map(|(ext, lang)| {
                            let ext = ext.to_ascii_lowercase();
                            let ext = if ext.starts_with('.') {
                                ext
                            } else {
                                format!(".{ext}")
                            };
                            Some((ext, lang.as_str()?.to_string()))
                        })
                        .collect()
                })
                .unwrap_or_default();
            (!command.is_empty() && !extensions.is_empty()).then_some(LspServerSpec {
                name,
                command,
                args,
                extensions,
            })
        })
        .collect()
}

fn read_file_servers(path: &Path) -> Vec<LspServerSpec> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .map(|doc| parse_servers(&doc, &BTreeMap::new()))
        .unwrap_or_default()
}

/// Every configured server, highest priority first: project `.lsp.json`,
/// user `~/.dcode-ai/lsp.json`, enabled plugins, then built-ins.
pub fn discover(workspace_root: &Path, plugins: &[crate::plugins::Plugin]) -> Vec<LspServerSpec> {
    let mut out = read_file_servers(&workspace_root.join(".lsp.json"));
    if let Some(home) = crate::config::dcode_ai_home_dir() {
        out.extend(read_file_servers(&home.join("lsp.json")));
    }
    for p in plugins {
        out.extend(p.lsp_servers());
    }
    out.extend(builtin_servers());
    out
}

/// Count source files per extension (bounded walk, skipping build output
/// and VCS directories).
pub fn extension_counts(root: &Path, limit: usize) -> HashMap<String, usize> {
    const SKIP: [&str; 8] = [
        ".git",
        "target",
        "node_modules",
        "dist",
        "build",
        ".venv",
        "venv",
        ".dcode-ai",
    ];
    let mut counts = HashMap::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    let mut seen = 0usize;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            let Ok(ty) = entry.file_type() else { continue };
            if ty.is_dir() {
                if !SKIP.contains(&name.as_ref()) {
                    stack.push(path);
                }
            } else if ty.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    *counts
                        .entry(format!(".{}", ext.to_ascii_lowercase()))
                        .or_insert(0) += 1;
                }
                seen += 1;
                if seen >= limit {
                    return counts;
                }
            }
        }
    }
    counts
}

/// Whether `command` resolves on `PATH` (or is an existing path).
pub fn command_available(command: &str) -> bool {
    let p = Path::new(command);
    if p.components().count() > 1 {
        return p.is_file();
    }
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            let c = dir.join(command);
            c.is_file() || (cfg!(windows) && dir.join(format!("{command}.exe")).is_file())
        })
    })
}

/// The server for the workspace's dominant language: the spec covering the
/// most files, preferring ones whose binary is installed, then priority.
pub fn pick(
    specs: &[LspServerSpec],
    counts: &HashMap<String, usize>,
    available: impl Fn(&str) -> bool,
) -> Option<LspServerSpec> {
    let score = |s: &LspServerSpec| -> usize {
        s.extensions
            .keys()
            .map(|e| counts.get(e).copied().unwrap_or(0))
            .sum()
    };
    let mut best: Option<(usize, bool, usize)> = None; // (score, available, index)
    for (i, s) in specs.iter().enumerate() {
        let sc = score(s);
        if sc == 0 {
            continue;
        }
        let av = available(&s.command);
        let better = match best {
            None => true,
            Some((bs, bav, _)) => (av, sc) > (bav, bs),
        };
        if better {
            best = Some((sc, av, i));
        }
    }
    best.map(|(_, _, i)| specs[i].clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_claude_lsp_servers_with_plugin_root() {
        let vars = BTreeMap::from([("CLAUDE_PLUGIN_ROOT".to_string(), "/p".to_string())]);
        let specs = parse_servers(
            &json!({"lspServers": {
                "ruby": {"command": "${CLAUDE_PLUGIN_ROOT}/bin/ruby-lsp", "args": ["--stdio"],
                         "extensionToLanguage": {".rb": "ruby", "rake": "ruby"}},
                "broken": {"command": "x"}
            }}),
            &vars,
        );
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].command, "/p/bin/ruby-lsp");
        assert_eq!(specs[0].extensions[".rake"], "ruby");
        assert_eq!(specs[0].language_id(Path::new("a/Rakefile.RB")), "ruby");
    }

    #[test]
    fn picks_dominant_language_preferring_installed_servers() {
        let specs = builtin_servers();
        let counts = HashMap::from([
            (".py".to_string(), 40),
            (".ts".to_string(), 10),
            (".rs".to_string(), 3),
        ]);
        let all = |_: &str| true;
        assert_eq!(pick(&specs, &counts, all).unwrap().name, "pyright");
        // pyright missing → the installed server with the most files.
        let no_pyright = |c: &str| c != "pyright-langserver";
        assert_eq!(
            pick(&specs, &counts, no_pyright).unwrap().name,
            "typescript"
        );
        assert!(pick(&specs, &HashMap::new(), all).is_none());
    }

    #[test]
    fn counts_skip_build_dirs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/x")).unwrap();
        std::fs::write(dir.path().join("src/a.py"), "").unwrap();
        std::fs::write(dir.path().join("node_modules/x/b.js"), "").unwrap();
        let c = extension_counts(dir.path(), 1000);
        assert_eq!(c.get(".py"), Some(&1));
        assert_eq!(c.get(".js"), None);
    }
}
