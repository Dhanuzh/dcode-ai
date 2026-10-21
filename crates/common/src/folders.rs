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
}
