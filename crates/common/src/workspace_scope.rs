//! Folders the agent may work in besides the workspace (`/add-dir`,
//! `--add-dir`, `[permissions] additional_directories`), like Claude Code's
//! additional working directories. File tools accept paths under the
//! workspace or under any of these.

use std::path::{Path, PathBuf};
use std::sync::{OnceLock, RwLock};

fn extra() -> &'static RwLock<Vec<PathBuf>> {
    static EXTRA: OnceLock<RwLock<Vec<PathBuf>>> = OnceLock::new();
    EXTRA.get_or_init(|| RwLock::new(Vec::new()))
}

/// Allow `dir` (must exist) for this process. Returns its canonical path.
pub fn add_dir(dir: &Path) -> Result<PathBuf, String> {
    let canonical = crate::config::canonicalize_simplified(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    if !canonical.is_dir() {
        return Err(format!("{} is not a directory", canonical.display()));
    }
    if let Ok(mut dirs) = extra().write()
        && !dirs.contains(&canonical)
    {
        dirs.push(canonical.clone());
    }
    Ok(canonical)
}

/// The added folders (canonical).
pub fn dirs() -> Vec<PathBuf> {
    extra().read().map(|d| d.clone()).unwrap_or_default()
}

/// Whether canonical `path` is inside the workspace `root` or an added
/// folder.
pub fn allows(root: &Path, path: &Path) -> bool {
    path.starts_with(root) || dirs().iter().any(|d| path.starts_with(d))
}

/// The added folder containing canonical `path`, if any.
pub fn added_dir_containing(path: &Path) -> Option<PathBuf> {
    dirs().into_iter().find(|d| path.starts_with(d))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn added_dirs_extend_the_workspace() {
        let ws = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let root = crate::config::canonicalize_simplified(ws.path()).unwrap();
        let file = other.path().join("notes.txt");
        std::fs::write(&file, "x").unwrap();
        let file = crate::config::canonicalize_simplified(&file).unwrap();
        assert!(!allows(&root, &file) || dirs().iter().any(|d| file.starts_with(d)));
        let added = add_dir(other.path()).unwrap();
        assert!(allows(&root, &file));
        assert_eq!(added_dir_containing(&file), Some(added));
        assert!(add_dir(&file).is_err(), "files are not directories");
    }
}
