//! Starter packs for `dcode-ai setup`: role and language bundles of plugins,
//! MCP servers and language-tool hints.
//!
//! Built-in packs ship in the binary; more are read from
//! `~/.dcode-ai/packs/*.toml` and `<repo>/.dcode-ai/packs/*.toml` (same
//! `[[pack]]` format, a later id replaces an earlier one).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const BUILTIN: &str = include_str!("packs/builtin.toml");

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackKind {
    Role,
    Language,
}

/// An MCP server a pack offers (added to `.mcp.json` after asking).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackMcp {
    pub name: String,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

/// A program the pack's plugins need on PATH. Never installed by dcode-ai.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackTool {
    pub command: String,
    pub install: String,
    #[serde(default)]
    pub why: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pack {
    pub id: String,
    pub name: String,
    pub kind: PackKind,
    #[serde(default)]
    pub description: String,
    /// Marker files (`Cargo.toml`) or `*.ext` patterns at the repo root.
    #[serde(default)]
    pub detect: Vec<String>,
    /// `name@marketplace` plugin references.
    #[serde(default)]
    pub plugins: Vec<String>,
    #[serde(default)]
    pub mcp: Vec<PackMcp>,
    #[serde(default)]
    pub tools: Vec<PackTool>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PackFile {
    #[serde(default)]
    pack: Vec<Pack>,
}

pub fn parse(text: &str) -> Result<Vec<Pack>, String> {
    let file: PackFile = toml::from_str(text).map_err(|e| e.to_string())?;
    for p in &file.pack {
        if p.id.is_empty()
            || !p
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
        {
            return Err(format!("invalid pack id `{}`", p.id));
        }
    }
    Ok(file.pack)
}

pub fn builtin() -> Vec<Pack> {
    parse(BUILTIN).expect("built-in packs parse")
}

fn pack_dirs(workspace_root: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(home) = crate::config::dcode_ai_home_dir() {
        dirs.push(home.join("packs"));
    }
    dirs.push(workspace_root.join(".dcode-ai/packs"));
    dirs
}

/// Built-in packs, then user and project pack files. Returns the packs and
/// any file errors.
pub fn load(workspace_root: &Path) -> (Vec<Pack>, Vec<String>) {
    load_from(builtin(), &pack_dirs(workspace_root))
}

fn load_from(mut packs: Vec<Pack>, dirs: &[PathBuf]) -> (Vec<Pack>, Vec<String>) {
    let mut errors = Vec::new();
    for dir in dirs {
        let Ok(rd) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut files: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "toml"))
            .collect();
        files.sort();
        for f in files {
            let parsed = std::fs::read_to_string(&f)
                .map_err(|e| e.to_string())
                .and_then(|t| parse(&t));
            match parsed {
                Ok(list) => {
                    for p in list {
                        match packs.iter_mut().find(|q| q.id == p.id) {
                            Some(slot) => *slot = p,
                            None => packs.push(p),
                        }
                    }
                }
                Err(e) => errors.push(format!("{}: {e}", f.display())),
            }
        }
    }
    (packs, errors)
}

/// Language packs whose markers exist at the workspace root.
pub fn detect<'a>(packs: &'a [Pack], workspace_root: &Path) -> Vec<&'a Pack> {
    let names: Vec<String> = std::fs::read_dir(workspace_root)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    packs
        .iter()
        .filter(|p| p.kind == PackKind::Language)
        .filter(|p| {
            p.detect.iter().any(|m| match m.strip_prefix('*') {
                Some(suffix) => names.iter().any(|n| n.ends_with(suffix)),
                None => names.iter().any(|n| n == m),
            })
        })
        .collect()
}

/// Whether `command` is an executable on PATH.
pub fn on_path(command: &str) -> bool {
    let Some(paths) = std::env::var_os("PATH") else {
        return false;
    };
    let exts: &[&str] = if cfg!(windows) {
        &["", ".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };
    std::env::split_paths(&paths).any(|dir| {
        exts.iter()
            .any(|e| dir.join(format!("{command}{e}")).is_file())
    })
}

/// What setup added for each installed pack, so `--remove` undoes only that.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackRecord {
    /// Plugin names setup installed for this pack.
    #[serde(default)]
    pub plugins: Vec<String>,
    /// `(scope, name)` MCP servers setup added.
    #[serde(default)]
    pub mcp: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupState {
    #[serde(default)]
    pub packs: BTreeMap<String, PackRecord>,
}

pub fn state_path() -> Option<PathBuf> {
    crate::config::dcode_ai_home_dir().map(|h| h.join("setup.json"))
}

impl SetupState {
    pub fn load() -> Self {
        state_path()
            .map(|p| Self::load_from(&p))
            .unwrap_or_default()
    }

    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let path = state_path().ok_or("no home directory")?;
        self.save_to(&path)
    }

    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Whether another installed pack (not `except`) recorded `plugin`.
    pub fn plugin_used_elsewhere(&self, plugin: &str, except: &str) -> bool {
        self.packs
            .iter()
            .any(|(id, r)| id != except && r.plugins.iter().any(|p| p == plugin))
    }
}

/// A team file: `packs = ["backend", "rust"]`, plus optional inline
/// `[[pack]]` definitions.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TeamFile {
    #[serde(default)]
    pub packs: Vec<String>,
    #[serde(default, rename = "pack", skip_serializing_if = "Vec::is_empty")]
    pub defs: Vec<Pack>,
}

impl TeamFile {
    pub fn read(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn write(&self, path: &Path) -> Result<(), String> {
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(
            path,
            format!(
                "# dcode-ai starter packs — install with: dcode-ai setup --from {}\n{text}",
                path.display()
            ),
        )
        .map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// Plugin name in a `name@marketplace` reference.
pub fn plugin_name(reference: &str) -> &str {
    reference.split_once('@').map_or(reference, |(n, _)| n)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_packs_parse_and_have_unique_ids() {
        let packs = builtin();
        assert!(packs.len() >= 12);
        let mut ids: Vec<_> = packs.iter().map(|p| p.id.as_str()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), packs.len());
        for p in &packs {
            assert!(!p.plugins.is_empty(), "{} has no plugins", p.id);
            assert!(p.plugins.iter().all(|r| r.contains('@')), "{}", p.id);
        }
        assert!(packs.iter().any(|p| p.kind == PackKind::Role));
    }

    #[test]
    fn detects_languages_by_marker_and_extension() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        std::fs::write(dir.path().join("tool.py"), "").unwrap();
        let packs = builtin();
        let mut found: Vec<_> = detect(&packs, dir.path())
            .iter()
            .map(|p| p.id.clone())
            .collect();
        found.sort();
        assert_eq!(found, ["python", "rust"]);
    }

    #[test]
    fn user_files_add_and_replace_packs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("team.toml"),
            r#"
[[pack]]
id = "rust"
name = "Rust (team)"
kind = "language"
plugins = ["x@m"]

[[pack]]
id = "acme"
name = "Acme"
kind = "role"
plugins = ["acme-tools@acme"]
"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("bad.toml"),
            "[[pack]]\nid = 'a b'\nname='x'\nkind='role'",
        )
        .unwrap();
        let (packs, errors) = load_from(builtin(), &[dir.path().to_path_buf()]);
        assert_eq!(errors.len(), 1);
        assert_eq!(
            packs.iter().find(|p| p.id == "rust").unwrap().name,
            "Rust (team)"
        );
        assert!(packs.iter().any(|p| p.id == "acme"));
    }

    #[test]
    fn state_tracks_shared_plugins_and_team_file_round_trips() {
        let mut st = SetupState::default();
        st.packs.insert(
            "backend".into(),
            PackRecord {
                plugins: vec!["code-review".into()],
                mcp: vec![],
            },
        );
        st.packs.insert(
            "frontend".into(),
            PackRecord {
                plugins: vec!["code-review".into(), "playwright".into()],
                mcp: vec![],
            },
        );
        assert!(st.plugin_used_elsewhere("code-review", "frontend"));
        assert!(!st.plugin_used_elsewhere("playwright", "frontend"));

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("team.toml");
        let team = TeamFile {
            packs: vec!["backend".into(), "rust".into()],
            defs: vec![],
        };
        team.write(&path).unwrap();
        assert_eq!(TeamFile::read(&path).unwrap(), team);
        assert_eq!(
            plugin_name("code-review@claude-plugins-official"),
            "code-review"
        );
    }
}
