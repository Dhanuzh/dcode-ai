//! Shared external-edit staleness tracking for file tools.
//!
//! `read_file` and every successful write record the file's mtime here. A
//! mutating tool checks the disk mtime against the recorded one first: if the
//! file changed on disk since the agent last saw it (user edit, formatter,
//! another process), the write is refused with a hint to re-read — instead of
//! silently clobbering the external change.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    /// Never seen, or unchanged since we last read/wrote it.
    Fresh,
    /// Disk mtime is newer than when the agent last read/wrote the file.
    StaleExternalEdit,
}

/// Cloneable handle; all clones share one mtime table.
#[derive(Debug, Clone, Default)]
pub struct FileFreshness {
    seen: Arc<Mutex<HashMap<PathBuf, SystemTime>>>,
}

impl FileFreshness {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record the file's current mtime (after a successful read or write).
    pub fn note(&self, path: &Path) {
        if let Ok(meta) = std::fs::metadata(path)
            && let Ok(mtime) = meta.modified()
            && let Ok(mut seen) = self.seen.lock()
        {
            seen.insert(path.to_path_buf(), mtime);
        }
    }

    /// Compare the disk mtime against the recorded one. Untracked files are
    /// `Fresh` (the agent may legitimately create or overwrite files it never
    /// read).
    pub fn check(&self, path: &Path) -> Freshness {
        let recorded = match self.seen.lock() {
            Ok(seen) => match seen.get(path) {
                Some(t) => *t,
                None => return Freshness::Fresh,
            },
            Err(_) => return Freshness::Fresh,
        };
        match std::fs::metadata(path).and_then(|m| m.modified()) {
            Ok(disk) if disk > recorded => Freshness::StaleExternalEdit,
            _ => Freshness::Fresh,
        }
    }

    /// Standard refusal message for a stale write target.
    pub fn stale_error(path: &Path) -> String {
        format!(
            "{} changed on disk after it was last read (external edit?). \
             Re-read it with read_file before modifying it.",
            path.display()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn untracked_file_is_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "x").unwrap();
        let fresh = FileFreshness::new();
        assert_eq!(fresh.check(&path), Freshness::Fresh);
    }

    #[test]
    fn unchanged_file_stays_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "x").unwrap();
        let fresh = FileFreshness::new();
        fresh.note(&path);
        assert_eq!(fresh.check(&path), Freshness::Fresh);
    }

    #[test]
    fn external_edit_marks_stale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "x").unwrap();
        let fresh = FileFreshness::new();
        fresh.note(&path);
        // Force a strictly newer mtime regardless of filesystem granularity.
        let newer = SystemTime::now() + Duration::from_secs(5);
        std::fs::write(&path, "external change").unwrap();
        filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(newer)).unwrap();
        assert_eq!(fresh.check(&path), Freshness::StaleExternalEdit);
    }

    #[test]
    fn re_noting_after_write_clears_staleness() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "x").unwrap();
        let fresh = FileFreshness::new();
        fresh.note(&path);
        let newer = SystemTime::now() + Duration::from_secs(5);
        std::fs::write(&path, "external").unwrap();
        filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(newer)).unwrap();
        assert_eq!(fresh.check(&path), Freshness::StaleExternalEdit);
        fresh.note(&path);
        assert_eq!(fresh.check(&path), Freshness::Fresh);
    }

    #[test]
    fn clones_share_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.txt");
        std::fs::write(&path, "x").unwrap();
        let a = FileFreshness::new();
        let b = a.clone();
        a.note(&path);
        let newer = SystemTime::now() + Duration::from_secs(5);
        filetime::set_file_mtime(&path, filetime::FileTime::from_system_time(newer)).unwrap();
        assert_eq!(b.check(&path), Freshness::StaleExternalEdit);
    }
}
