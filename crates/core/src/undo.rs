use dcode_ai_common::tool::{ToolCall, ToolResult};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
enum NodeSnapshot {
    Absent,
    File(Vec<u8>),
    Dir {
        dirs: Vec<PathBuf>,
        files: Vec<(PathBuf, Vec<u8>)>,
    },
}

#[derive(Debug, Clone)]
struct UndoEntry {
    /// History index of the user message that started this turn, or `None`
    /// once history was rewritten (compaction) and the index is meaningless.
    msg_pos: Option<usize>,
    changed_paths: Vec<PathBuf>,
    before: HashMap<PathBuf, NodeSnapshot>,
    after: HashMap<PathBuf, NodeSnapshot>,
}

#[derive(Debug, Default)]
struct PendingTurn {
    msg_pos: Option<usize>,
    touched: Vec<PathBuf>,
    before: HashMap<PathBuf, NodeSnapshot>,
    had_successful_mutation: bool,
}

#[derive(Debug, Default)]
pub struct UndoManager {
    undo_stack: Vec<UndoEntry>,
    redo_stack: Vec<UndoEntry>,
    pending: Option<PendingTurn>,
}

impl UndoManager {
    /// Start recording a turn whose user message sits at history index
    /// `msg_pos`.
    pub fn begin_turn(&mut self, msg_pos: usize) {
        self.pending = Some(PendingTurn {
            msg_pos: Some(msg_pos),
            ..Default::default()
        });
    }

    /// A turn ended early (error / cancel). Edits it already made stay on
    /// disk, so they are still recorded for `/undo` and `/rewind`.
    pub fn abort_turn(&mut self) {
        if let Err(e) = self.finalize_turn() {
            tracing::warn!("undo snapshot for aborted turn failed: {e}");
        }
    }

    /// History was rewritten (compaction, restore): message positions no
    /// longer line up, so recorded turns can be undone but not rewound to.
    pub fn invalidate_positions(&mut self) {
        for entry in self.undo_stack.iter_mut().chain(self.redo_stack.iter_mut()) {
            entry.msg_pos = None;
        }
        if let Some(p) = self.pending.as_mut() {
            p.msg_pos = None;
        }
    }

    /// Paths changed by turns that started at or after history index `pos`.
    pub fn paths_changed_since(&self, pos: usize) -> Vec<PathBuf> {
        let mut out = Vec::new();
        for entry in &self.undo_stack {
            if entry.msg_pos.is_some_and(|p| p >= pos) {
                for path in &entry.changed_paths {
                    insert_dedup_path(&mut out, path.clone());
                }
            }
        }
        out
    }

    /// Restore files to how they were before the turn at history index
    /// `pos`: undo, newest first, every recorded turn from there on.
    /// Returns the number of distinct paths restored.
    pub fn rewind_to(&mut self, pos: usize) -> Result<usize, String> {
        let mut restored: Vec<PathBuf> = Vec::new();
        while self
            .undo_stack
            .last()
            .is_some_and(|e| e.msg_pos.is_some_and(|p| p >= pos))
        {
            let entry = self.undo_stack.pop().expect("checked non-empty");
            apply_state_map(&entry.before)?;
            for path in entry.changed_paths {
                insert_dedup_path(&mut restored, path);
            }
        }
        // Rewound turns are gone from history; redoing them makes no sense.
        self.redo_stack.clear();
        Ok(restored.len())
    }

    pub fn record_tool_call(&mut self, call: &ToolCall, workspace_root: &Path) {
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        for path in mutating_tool_paths(call, workspace_root) {
            if !path.starts_with(workspace_root) {
                continue;
            }
            insert_dedup_path(&mut pending.touched, path);
        }
        for path in &pending.touched {
            if !pending.before.contains_key(path) {
                pending.before.insert(
                    path.clone(),
                    snapshot_node(path).unwrap_or(NodeSnapshot::Absent),
                );
            }
        }
    }

    pub fn note_results(&mut self, tool_calls: &[ToolCall], results: &[ToolResult]) {
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        if pending.touched.is_empty() {
            return;
        }

        let by_id: HashMap<&str, &str> = tool_calls
            .iter()
            .map(|call| (call.id.as_str(), call.name.as_str()))
            .collect();

        if results.iter().any(|r| {
            r.success
                && by_id
                    .get(r.call_id.as_str())
                    .is_some_and(|name| is_mutating_tool(name))
        }) {
            pending.had_successful_mutation = true;
        }
    }

    pub fn finalize_turn(&mut self) -> Result<(), String> {
        let Some(pending) = self.pending.take() else {
            return Ok(());
        };
        if !pending.had_successful_mutation || pending.touched.is_empty() {
            return Ok(());
        }

        let mut after = HashMap::new();
        let mut changed_paths = Vec::new();
        for path in pending.touched {
            let before = pending
                .before
                .get(&path)
                .cloned()
                .unwrap_or(NodeSnapshot::Absent);
            let after_state = snapshot_node(&path).unwrap_or(NodeSnapshot::Absent);
            if before != after_state {
                changed_paths.push(path.clone());
            }
            after.insert(path, after_state);
        }

        if changed_paths.is_empty() {
            return Ok(());
        }

        self.redo_stack.clear();
        self.undo_stack.push(UndoEntry {
            msg_pos: pending.msg_pos,
            changed_paths,
            before: pending.before,
            after,
        });
        Ok(())
    }

    pub fn undo_last(&mut self) -> Result<Option<String>, String> {
        let Some(entry) = self.undo_stack.pop() else {
            return Ok(None);
        };
        apply_state_map(&entry.before)?;
        let n = entry.changed_paths.len();
        self.redo_stack.push(entry);
        Ok(Some(format!("Undid last turn ({n} path(s) restored)")))
    }

    pub fn redo_last(&mut self) -> Result<Option<String>, String> {
        let Some(entry) = self.redo_stack.pop() else {
            return Ok(None);
        };
        apply_state_map(&entry.after)?;
        let n = entry.changed_paths.len();
        self.undo_stack.push(entry);
        Ok(Some(format!("Redid last turn ({n} path(s) restored)")))
    }
}

fn is_mutating_tool(name: &str) -> bool {
    matches!(
        name,
        "write_file"
            | "edit_file"
            | "replace_match"
            | "apply_patch"
            | "create_directory"
            | "delete_path"
            | "rename_path"
            | "move_path"
            | "copy_path"
    )
}

fn mutating_tool_paths(call: &ToolCall, workspace_root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let path = |k: &str| call.input.get(k).and_then(|v| v.as_str());
    let push = |out: &mut Vec<PathBuf>, raw: Option<&str>| {
        if let Some(raw) = raw {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                out.push(workspace_root.join(trimmed));
            }
        }
    };

    match call.name.as_str() {
        "write_file" | "edit_file" | "replace_match" | "apply_patch" | "create_directory"
        | "delete_path" => {
            push(&mut out, path("path"));
        }
        "rename_path" | "move_path" => {
            push(&mut out, path("from"));
            push(&mut out, path("to"));
        }
        "copy_path" => {
            push(&mut out, path("to"));
        }
        _ => {}
    }

    out
}

fn insert_dedup_path(paths: &mut Vec<PathBuf>, new_path: PathBuf) {
    if paths
        .iter()
        .any(|existing| new_path.starts_with(existing) || existing == &new_path)
    {
        return;
    }
    paths.retain(|existing| !existing.starts_with(&new_path));
    paths.push(new_path);
}

fn snapshot_node(path: &Path) -> Result<NodeSnapshot, String> {
    let md = match fs::metadata(path) {
        Ok(md) => md,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(NodeSnapshot::Absent),
        Err(e) => return Err(format!("stat {}: {e}", path.display())),
    };

    if md.is_file() {
        let bytes = fs::read(path).map_err(|e| format!("read {}: {e}", path.display()))?;
        return Ok(NodeSnapshot::File(bytes));
    }

    if !md.is_dir() {
        return Ok(NodeSnapshot::Absent);
    }

    let mut dirs = vec![PathBuf::new()];
    let mut files: Vec<(PathBuf, Vec<u8>)> = Vec::new();
    let mut stack = vec![PathBuf::new()];

    while let Some(rel) = stack.pop() {
        let abs = path.join(&rel);
        let entries = fs::read_dir(&abs).map_err(|e| format!("read_dir {}: {e}", abs.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("dir entry {}: {e}", abs.display()))?;
            let child_abs = entry.path();
            let name = entry.file_name();
            let child_rel = rel.join(name);
            let child_md = entry
                .metadata()
                .map_err(|e| format!("metadata {}: {e}", child_abs.display()))?;
            if child_md.is_dir() {
                dirs.push(child_rel.clone());
                stack.push(child_rel);
            } else if child_md.is_file() {
                let bytes = fs::read(&child_abs)
                    .map_err(|e| format!("read {}: {e}", child_abs.display()))?;
                files.push((child_rel, bytes));
            }
        }
    }

    dirs.sort_by_key(|p| p.components().count());
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(NodeSnapshot::Dir { dirs, files })
}

fn restore_node(path: &Path, snap: &NodeSnapshot) -> Result<(), String> {
    if let Ok(md) = fs::metadata(path) {
        if md.is_dir() {
            fs::remove_dir_all(path)
                .map_err(|e| format!("remove_dir_all {}: {e}", path.display()))?;
        } else {
            fs::remove_file(path).map_err(|e| format!("remove_file {}: {e}", path.display()))?;
        }
    }

    match snap {
        NodeSnapshot::Absent => Ok(()),
        NodeSnapshot::File(bytes) => {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("create_dir_all {}: {e}", parent.display()))?;
            }
            fs::write(path, bytes).map_err(|e| format!("write {}: {e}", path.display()))
        }
        NodeSnapshot::Dir { dirs, files } => {
            fs::create_dir_all(path)
                .map_err(|e| format!("create_dir_all {}: {e}", path.display()))?;
            for rel_dir in dirs {
                if rel_dir.as_os_str().is_empty() {
                    continue;
                }
                let abs = path.join(rel_dir);
                fs::create_dir_all(&abs)
                    .map_err(|e| format!("create_dir_all {}: {e}", abs.display()))?;
            }
            for (rel_file, bytes) in files {
                let abs = path.join(rel_file);
                if let Some(parent) = abs.parent() {
                    fs::create_dir_all(parent)
                        .map_err(|e| format!("create_dir_all {}: {e}", parent.display()))?;
                }
                fs::write(&abs, bytes).map_err(|e| format!("write {}: {e}", abs.display()))?;
            }
            Ok(())
        }
    }
}

fn apply_state_map(states: &HashMap<PathBuf, NodeSnapshot>) -> Result<(), String> {
    let mut paths: Vec<&PathBuf> = states.keys().collect();
    paths.sort_by_key(|b| std::cmp::Reverse(b.components().count()));
    for path in paths {
        if let Some(state) = states.get(path) {
            restore_node(path, state)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn undo_redo_roundtrip_file_write() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("a.txt");
        fs::write(&path, "before").expect("seed");

        let mut mgr = UndoManager::default();
        mgr.begin_turn(1);

        let call = ToolCall {
            id: "c1".into(),
            name: "write_file".into(),
            input: serde_json::json!({"path":"a.txt","content":"after"}),
        };
        mgr.record_tool_call(&call, tmp.path());

        fs::write(&path, "after").expect("mutate");
        let result = ToolResult {
            call_id: "c1".into(),
            success: true,
            output: String::new(),
            error: None,
        };
        mgr.note_results(std::slice::from_ref(&call), std::slice::from_ref(&result));
        mgr.finalize_turn().expect("finalize");

        let msg = mgr.undo_last().expect("undo");
        assert!(msg.is_some());
        assert_eq!(fs::read_to_string(&path).expect("read"), "before");

        let msg = mgr.redo_last().expect("redo");
        assert!(msg.is_some());
        assert_eq!(fs::read_to_string(&path).expect("read"), "after");
    }

    #[test]
    fn undo_handles_created_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("new.txt");

        let mut mgr = UndoManager::default();
        mgr.begin_turn(1);
        let call = ToolCall {
            id: "c2".into(),
            name: "write_file".into(),
            input: serde_json::json!({"path":"new.txt","content":"x"}),
        };
        mgr.record_tool_call(&call, tmp.path());
        fs::write(&path, "x").expect("write");
        let result = ToolResult {
            call_id: "c2".into(),
            success: true,
            output: String::new(),
            error: None,
        };
        mgr.note_results(std::slice::from_ref(&call), std::slice::from_ref(&result));
        mgr.finalize_turn().expect("finalize");

        mgr.undo_last().expect("undo");
        assert!(!path.exists());
    }

    fn write_turn(mgr: &mut UndoManager, root: &Path, pos: usize, id: &str, content: &str) {
        mgr.begin_turn(pos);
        let call = ToolCall {
            id: id.into(),
            name: "write_file".into(),
            input: serde_json::json!({"path":"a.txt","content":content}),
        };
        mgr.record_tool_call(&call, root);
        fs::write(root.join("a.txt"), content).expect("write");
        let result = ToolResult {
            call_id: id.into(),
            success: true,
            output: String::new(),
            error: None,
        };
        mgr.note_results(std::slice::from_ref(&call), std::slice::from_ref(&result));
    }

    #[test]
    fn rewind_restores_every_turn_from_a_message() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("a.txt");
        fs::write(&path, "v0").expect("seed");
        let mut mgr = UndoManager::default();

        write_turn(&mut mgr, tmp.path(), 1, "c1", "v1");
        mgr.finalize_turn().expect("finalize");
        write_turn(&mut mgr, tmp.path(), 5, "c2", "v2");
        // An errored turn still keeps its snapshot.
        mgr.abort_turn();
        write_turn(&mut mgr, tmp.path(), 9, "c3", "v3");
        mgr.finalize_turn().expect("finalize");

        assert_eq!(mgr.paths_changed_since(5).len(), 1);
        assert_eq!(mgr.rewind_to(5).expect("rewind"), 1);
        assert_eq!(fs::read_to_string(&path).expect("read"), "v1");
        // The first turn is untouched and still undoable.
        mgr.undo_last().expect("undo");
        assert_eq!(fs::read_to_string(&path).expect("read"), "v0");
    }

    #[test]
    fn rewind_skips_turns_with_unknown_positions() {
        let tmp = tempfile::tempdir().expect("tempdir");
        fs::write(tmp.path().join("a.txt"), "v0").expect("seed");
        let mut mgr = UndoManager::default();
        write_turn(&mut mgr, tmp.path(), 3, "c1", "v1");
        mgr.finalize_turn().expect("finalize");
        mgr.invalidate_positions();
        assert_eq!(mgr.rewind_to(0).expect("rewind"), 0);
        assert_eq!(fs::read_to_string(tmp.path().join("a.txt")).unwrap(), "v1");
    }
}
