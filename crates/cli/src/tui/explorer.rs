//! The dashboard's files tab as a project explorer: the whole project tree
//! (respecting `.gitignore`), what the agent touched marked on it, folders
//! that open and close.

use crate::tui::activity::{FileMap, FileTouch};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Most entries scanned (huge monorepos stay responsive).
const MAX_ENTRIES: usize = 20_000;

/// One project entry: relative path, and whether it's a folder.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Entry {
    pub path: String,
    pub dir: bool,
}

#[derive(Debug, Default)]
pub struct Explorer {
    /// Scanned entries, sorted (folders before files within a folder).
    pub entries: Vec<Entry>,
    /// Folders opened.
    pub open: HashSet<String>,
    /// Selected row (in the visible rows).
    pub cursor: usize,
    /// Show only what the agent touched (the old view).
    pub touched_only: bool,
    /// A scan running in the background, and where its result lands.
    pending: Option<Arc<Mutex<Option<Vec<Entry>>>>>,
    /// The file map's size when last scanned (new files → scan again).
    scanned_for: Option<(usize, usize)>,
    pub truncated: bool,
}

/// A row to draw.
pub struct Row<'a> {
    pub depth: usize,
    pub name: String,
    pub path: String,
    pub dir: bool,
    pub open: bool,
    pub touch: Option<&'a FileTouch>,
    /// A folder with something touched inside.
    pub touched_inside: bool,
}

fn scan(root: &Path) -> (Vec<Entry>, bool) {
    let mut out = Vec::new();
    let mut truncated = false;
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .filter_entry(|e| e.file_name() != ".git")
        .sort_by_file_path(|a, b| a.cmp(b))
        .build();
    for e in walker.flatten() {
        let Ok(rel) = e.path().strip_prefix(root) else {
            continue;
        };
        let path = rel.to_string_lossy().replace('\\', "/");
        if path.is_empty() {
            continue;
        }
        out.push(Entry {
            dir: e.file_type().is_some_and(|t| t.is_dir()),
            path,
        });
        if out.len() >= MAX_ENTRIES {
            truncated = true;
            break;
        }
    }
    (out, truncated)
}

/// Sort so each folder's subfolders come before its files.
fn tree_order(mut entries: Vec<Entry>) -> Vec<Entry> {
    entries.sort_by(|a, b| {
        let (pa, pb) = (Path::new(&a.path), Path::new(&b.path));
        let ca: Vec<_> = pa.components().collect();
        let cb: Vec<_> = pb.components().collect();
        for (i, (x, y)) in ca.iter().zip(cb.iter()).enumerate() {
            if x != y {
                // At the first difference: a folder (more to come, or a dir
                // entry) sorts before a file.
                let a_dir = i + 1 < ca.len() || a.dir;
                let b_dir = i + 1 < cb.len() || b.dir;
                return b_dir.cmp(&a_dir).then_with(|| {
                    x.as_os_str()
                        .to_string_lossy()
                        .to_lowercase()
                        .cmp(&y.as_os_str().to_string_lossy().to_lowercase())
                });
            }
        }
        ca.len().cmp(&cb.len())
    });
    entries
}

impl Explorer {
    /// Starts a background scan when there's none yet or the agent made
    /// new files since; picks up a finished one.
    pub fn refresh(&mut self, root: &Path, files: &FileMap) {
        if let Some(slot) = &self.pending {
            let done = slot.lock().ok().and_then(|mut s| s.take());
            if let Some(entries) = done {
                self.truncated = entries.len() >= MAX_ENTRIES;
                self.entries = tree_order(entries);
                self.pending = None;
            }
            return;
        }
        let key = (files.files.len(), files.edited());
        if self.scanned_for == Some(key) {
            return;
        }
        self.scanned_for = Some(key);
        let slot: Arc<Mutex<Option<Vec<Entry>>>> = Arc::default();
        let out = slot.clone();
        let root: PathBuf = root.to_path_buf();
        std::thread::spawn(move || {
            let (entries, _) = scan(&root);
            if let Ok(mut s) = out.lock() {
                *s = Some(entries);
            }
        });
        self.pending = Some(slot);
    }

    pub fn rescan(&mut self) {
        self.scanned_for = None;
    }

    pub fn scanning(&self) -> bool {
        self.pending.is_some() && self.entries.is_empty()
    }

    /// The rows to show now: top level, plus inside open folders.
    pub fn rows<'a>(&self, files: &'a FileMap) -> Vec<Row<'a>> {
        // Folders with something touched inside.
        let mut touched_dirs: HashSet<String> = HashSet::new();
        for p in files.files.keys() {
            let mut cur = p.as_str();
            while let Some((parent, _)) = cur.rsplit_once('/') {
                touched_dirs.insert(parent.to_string());
                cur = parent;
            }
        }
        let mut rows = Vec::new();
        for e in &self.entries {
            let parts: Vec<&str> = e.path.split('/').collect();
            // Every ancestor must be open.
            let visible = (1..parts.len()).all(|i| self.open.contains(&parts[..i].join("/")));
            if !visible {
                continue;
            }
            rows.push(Row {
                depth: parts.len() - 1,
                name: parts.last().copied().unwrap_or_default().to_string(),
                path: e.path.clone(),
                dir: e.dir,
                open: e.dir && self.open.contains(&e.path),
                touch: files.files.get(&e.path),
                touched_inside: e.dir && touched_dirs.contains(&e.path),
            });
        }
        rows
    }

    /// Opens or closes the folder at row `i`; returns the file path when the
    /// row is a file.
    pub fn activate(&mut self, files: &FileMap, i: usize) -> Option<String> {
        let (path, dir) = {
            let rows = self.rows(files);
            let r = rows.get(i)?;
            (r.path.clone(), r.dir)
        };
        if dir {
            if !self.open.remove(&path) {
                self.open.insert(path);
            }
            None
        } else {
            Some(path)
        }
    }

    /// `←`: close the folder at the cursor, or go to its parent.
    pub fn close_or_up(&mut self, files: &FileMap) {
        let rows = self.rows(files);
        let Some(r) = rows.get(self.cursor) else {
            return;
        };
        if r.dir && r.open {
            let p = r.path.clone();
            self.open.remove(&p);
        } else if let Some((parent, _)) = r.path.rsplit_once('/') {
            let parent = parent.to_string();
            if let Some(i) = rows.iter().position(|x| x.path == parent) {
                self.cursor = i;
            }
        }
    }

    pub fn move_cursor(&mut self, files: &FileMap, step: i32) {
        let n = self.rows(files).len();
        if n == 0 {
            return;
        }
        self.cursor = (self.cursor as i32 + step).clamp(0, n as i32 - 1) as usize;
    }

    /// Opens every folder on the way to `path` (to show a touched file).
    pub fn reveal(&mut self, path: &str) {
        let parts: Vec<&str> = path.split('/').collect();
        for i in 1..parts.len() {
            self.open.insert(parts[..i].join("/"));
        }
    }
}

/// Count of touched files per top-level folder (for the header).
pub fn touched_summary(files: &FileMap) -> BTreeMap<String, usize> {
    let mut m = BTreeMap::new();
    for p in files.files.keys() {
        let top = p.split('/').next().unwrap_or("").to_string();
        *m.entry(top).or_insert(0) += 1;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_project_tree_opens_and_closes_and_skips_ignored_files() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("src/ui")).unwrap();
        std::fs::create_dir_all(root.join("target/debug")).unwrap();
        std::fs::write(root.join(".gitignore"), "target/\n").unwrap();
        std::fs::write(root.join("README.md"), "").unwrap();
        std::fs::write(root.join("src/main.rs"), "").unwrap();
        std::fs::write(root.join("src/ui/view.rs"), "").unwrap();
        std::fs::write(root.join("target/debug/x"), "").unwrap();
        // `ignore` respects .gitignore only inside a git repo.
        std::fs::create_dir_all(root.join(".git")).unwrap();

        let mut files = FileMap::default();
        files.touch(
            "edit_file",
            &serde_json::json!({"path": "src/ui/view.rs"}),
            chrono::Local::now(),
        );
        let mut ex = Explorer::default();
        ex.refresh(root, &files);
        for _ in 0..200 {
            ex.refresh(root, &files);
            if !ex.entries.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let names = |ex: &Explorer| -> Vec<String> {
            ex.rows(&files)
                .iter()
                .map(|r| {
                    format!(
                        "{}{}{}",
                        "  ".repeat(r.depth),
                        r.name,
                        if r.dir { "/" } else { "" }
                    )
                })
                .collect()
        };
        let rows = names(&ex);
        assert_eq!(rows[0], "src/", "folders first: {rows:?}");
        assert!(rows.contains(&"README.md".to_string()));
        assert!(
            !rows.iter().any(|r| r.contains("target")),
            "gitignored: {rows:?}"
        );
        // src has something touched inside; open it, then ui.
        assert!(ex.rows(&files)[0].touched_inside);
        assert_eq!(ex.activate(&files, 0), None);
        let rows = names(&ex);
        assert_eq!(&rows[..3], ["src/", "  ui/", "  main.rs"]);
        ex.reveal("src/ui/view.rs");
        let r = ex.rows(&files);
        let view = r.iter().find(|r| r.name == "view.rs").unwrap();
        assert_eq!(view.touch.map(|t| t.edits), Some(1));
        let idx = r.iter().position(|r| r.name == "view.rs").unwrap();
        assert_eq!(ex.activate(&files, idx), Some("src/ui/view.rs".into()));
        // ← on a file goes to its folder; ← on the open folder closes it.
        ex.cursor = idx;
        ex.close_or_up(&files);
        assert_eq!(ex.rows(&files)[ex.cursor].name, "ui");
        ex.close_or_up(&files);
        assert!(!ex.open.contains("src/ui"));
    }
}
