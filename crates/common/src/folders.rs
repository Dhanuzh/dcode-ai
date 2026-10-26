//! Per-folder choices remembered across sessions (`~/.dcode-ai/folders.json`),
//! such as the permission mode picked at startup — asked once per folder.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderChoice {
    /// Startup permission choice: `manual`, `accept_edits` or `auto`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission: Option<String>,
    /// MCP servers turned off in `/mcp` for this folder.
    #[serde(default, skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub disabled_mcp: std::collections::BTreeSet<String>,
}

impl FolderChoice {
    fn is_empty(&self) -> bool {
        self.permission.is_none() && self.disabled_mcp.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderChoices {
    /// Canonical folder path → choices.
    #[serde(default)]
    pub folders: BTreeMap<String, FolderChoice>,
}

pub fn store_path() -> Option<PathBuf> {
    crate::config::dcode_ai_home_dir().map(|h| h.join("folders.json"))
}

fn key(folder: &Path) -> String {
    crate::config::canonicalize_simplified(folder)
        .unwrap_or_else(|_| folder.to_path_buf())
        .display()
        .to_string()
}

impl FolderChoices {
    pub fn load_from(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save_to(&self, path: &Path) -> Result<(), String> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn permission_for(&self, folder: &Path) -> Option<&str> {
        self.folders.get(&key(folder))?.permission.as_deref()
    }

    pub fn set_permission(&mut self, folder: &Path, choice: &str) {
        self.folders.entry(key(folder)).or_default().permission = Some(choice.to_string());
    }
}

/// The startup permission choice remembered for `folder`, if any.
pub fn remembered_permission(folder: &Path) -> Option<String> {
    let path = store_path()?;
    FolderChoices::load_from(&path)
        .permission_for(folder)
        .map(String::from)
}

/// Remember `choice` for `folder` so the startup question isn't asked again.
pub fn remember_permission(folder: &Path, choice: &str) -> Result<(), String> {
    let path = store_path().ok_or("no home directory")?;
    let mut all = FolderChoices::load_from(&path);
    all.set_permission(folder, choice);
    all.save_to(&path)
}

/// Forget `folder`'s startup choice so the question is asked again.
/// Returns whether there was one.
pub fn forget_permission(folder: &Path) -> Result<bool, String> {
    let path = store_path().ok_or("no home directory")?;
    let mut all = FolderChoices::load_from(&path);
    let had = all
        .folders
        .get_mut(&key(folder))
        .and_then(|c| c.permission.take())
        .is_some();
    all.folders.retain(|_, c| !c.is_empty());
    if had {
        all.save_to(&path)?;
    }
    Ok(had)
}

impl FolderChoices {
    pub fn disabled_mcp_for(&self, folder: &Path) -> Vec<String> {
        self.folders
            .get(&key(folder))
            .map(|c| c.disabled_mcp.iter().cloned().collect())
            .unwrap_or_default()
    }

    pub fn set_mcp_enabled(&mut self, folder: &Path, server: &str, enabled: bool) {
        let k = key(folder);
        let entry = self.folders.entry(k.clone()).or_default();
        if enabled {
            entry.disabled_mcp.remove(server);
        } else {
            entry.disabled_mcp.insert(server.to_string());
        }
        if entry.is_empty() {
            self.folders.remove(&k);
        }
    }
}

/// MCP servers turned off in `/mcp` for `folder`.
pub fn disabled_mcp(folder: &Path) -> Vec<String> {
    store_path()
        .map(|p| FolderChoices::load_from(&p).disabled_mcp_for(folder))
        .unwrap_or_default()
}

/// Remember an MCP server as on or off for `folder`.
pub fn set_mcp_enabled(folder: &Path, server: &str, enabled: bool) -> Result<(), String> {
    let path = store_path().ok_or("no home directory")?;
    let mut all = FolderChoices::load_from(&path);
    all.set_mcp_enabled(folder, server, enabled);
    all.save_to(&path)
}

/// Every folder with a remembered startup choice: `(folder, choice)`.
pub fn remembered_permissions() -> Vec<(String, String)> {
    store_path()
        .map(|p| FolderChoices::load_from(&p))
        .unwrap_or_default()
        .folders
        .into_iter()
        .filter_map(|(f, c)| c.permission.map(|p| (f, p)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_are_per_folder_and_survive_a_reload() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let store = dir.path().join("folders.json");
        let mut c = FolderChoices::default();
        c.set_permission(&a, "auto");
        c.save_to(&store).unwrap();
        let back = FolderChoices::load_from(&store);
        assert_eq!(back.permission_for(&a), Some("auto"));
        assert_eq!(back.permission_for(&a.join(".")), Some("auto"));
        assert_eq!(back.permission_for(&b), None);
    }

    #[test]
    fn mcp_servers_can_be_turned_off_per_folder() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        std::fs::create_dir_all(&a).unwrap();
        let mut c = FolderChoices::default();
        c.set_mcp_enabled(&a, "zoho", false);
        assert_eq!(c.disabled_mcp_for(&a), vec!["zoho".to_string()]);
        c.set_mcp_enabled(&a, "zoho", true);
        assert!(c.disabled_mcp_for(&a).is_empty());
        assert!(c.folders.is_empty(), "an empty entry is dropped");
    }
}
