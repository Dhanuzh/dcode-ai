//! Decision prompts answered in the TUI: merging a sub-agent's branch and
//! `/memory tidy`.

use super::*;
use crate::tui::choice::{ChoiceAction, ChoicePrompt};
use dcode_ai_runtime::memory_store::{MemoryStore, tidy_plan};
use dcode_ai_runtime::worktree::WorktreeManager;

/// Remove notes by (file, id); returns how many went.
async fn remove_notes(remove: Vec<(std::path::PathBuf, String)>) -> usize {
    let mut removed = 0;
    let mut by_file: std::collections::BTreeMap<std::path::PathBuf, Vec<String>> =
        Default::default();
    for (path, id) in remove {
        by_file.entry(path).or_default().push(id);
    }
    for (path, ids) in by_file {
        let store = MemoryStore::new(&path);
        let Ok(mut m) = store.load().await else {
            continue;
        };
        let before = m.notes.len();
        m.notes.retain(|n| !ids.contains(&n.id));
        if m.notes.len() != before && store.save(&m).await.is_ok() {
            removed += before - m.notes.len();
        }
    }
    removed
}

fn say(st: &Arc<Mutex<TuiSessionState>>, text: String) {
    if let Ok(mut g) = st.lock() {
        g.push_block(DisplayBlock::System(text));
        g.touch_transcript();
    }
}

impl Repl {
    pub(super) async fn handle_choice(
        &mut self,
        action: ChoiceAction,
        index: usize,
        st: &Arc<Mutex<TuiSessionState>>,
    ) {
        match action {
            ChoiceAction::SubagentChanges {
                child_session_id,
                task,
                worktree_path,
                base_branch: _,
            } => {
                let root = self.runtime.workspace_root().to_path_buf();
                let id = child_session_id.clone();
                let task_line: String =
                    task.lines().next().unwrap_or("").chars().take(60).collect();
                let msg = tokio::task::spawn_blocking(move || {
                    let mgr = WorktreeManager::new(&root);
                    let branch = format!("dcode-ai/{id}");
                    match index {
                        0 => {
                            if let Err(e) = mgr.commit_pending(
                                &worktree_path,
                                &format!("dcode-ai sub-agent: {task_line}"),
                            ) {
                                return format!("[merge] could not commit the sub-agent's work: {e}");
                            }
                            match mgr.merge_into_current(&id, &format!("Merge sub-agent: {task_line}")) {
                                Ok(()) => {
                                    let _ = mgr.remove_worktree(&id, true);
                                    format!("[merge] merged {branch} and removed its worktree")
                                }
                                Err(e) => format!("[merge] {e}"),
                            }
                        }
                        1 => format!(
                            "[merge] kept {branch} — merge later with `dcode-ai worktrees merge {id}`"
                        ),
                        _ => match mgr.remove_worktree(&id, true) {
                            Ok(()) => format!("[merge] discarded {branch}"),
                            Err(e) => format!("[merge] {e}"),
                        },
                    }
                })
                .await
                .unwrap_or_else(|e| format!("[merge] {e}"));
                say(st, msg);
            }
            ChoiceAction::DoctorFix { fixes } => {
                if index == 0 {
                    self.apply_doctor_fixes(fixes, st).await;
                }
            }
            ChoiceAction::MemoryTidy { remove } => {
                if index != 0 {
                    say(st, "[memory] left as is".into());
                    return;
                }
                let removed = remove_notes(remove).await;
                say(st, format!("[memory] removed {removed} note(s)"));
            }
        }
    }

    pub(super) fn show_choice_detail(
        &self,
        action: ChoiceAction,
        st: &Arc<Mutex<TuiSessionState>>,
    ) {
        let ChoiceAction::SubagentChanges {
            worktree_path,
            base_branch,
            child_session_id,
            ..
        } = action
        else {
            return;
        };
        let mgr = WorktreeManager::new(self.runtime.workspace_root());
        let diff = mgr.branch_diff(&worktree_path, &base_branch);
        let lines: Vec<String> = if diff.trim().is_empty() {
            vec!["No changes against the base branch.".into()]
        } else {
            diff.lines().map(str::to_string).collect()
        };
        if let Ok(mut g) = st.lock() {
            g.open_info_modal(format!("dcode-ai/{child_session_id}"), lines);
        }
    }

    /// `/memory tidy`: find duplicate and empty notes and ask before
    /// removing them.
    pub(super) async fn memory_tidy(&self, out: &ReplOutput<'_>, apply: bool) {
        let mut paths = vec![self.runtime.memory_store_path()];
        if let Some(user) = dcode_ai_runtime::memory_tool::user_memory_path() {
            paths.insert(0, user);
        }
        let mut total = 0;
        let mut remove = Vec::new();
        let mut body = Vec::new();
        for path in paths {
            let Ok(m) = MemoryStore::new(&path).load().await else {
                continue;
            };
            total += m.notes.len();
            for r in tidy_plan(&m.notes) {
                let text: String = r.content.split_whitespace().collect::<Vec<_>>().join(" ");
                body.push(format!(
                    "  {}  {} · {}",
                    r.id,
                    r.reason,
                    crate::tui::render_helpers::truncate_chars(&text, 60)
                ));
                remove.push((path.clone(), r.id));
            }
        }
        if remove.is_empty() {
            out.println(&format!(
                "[memory] already tidy — {total} note(s), no duplicates"
            ));
            return;
        }
        let n = remove.len();
        if apply {
            let removed = remove_notes(remove).await;
            out.println(&format!("[memory] removed {removed} note(s)"));
            return;
        }
        const SHOWN: usize = 8;
        let more = body.len().saturating_sub(SHOWN);
        body.truncate(SHOWN);
        if more > 0 {
            body.push(format!("  … {more} more"));
        }
        body.insert(0, format!("{n} of {total} notes are duplicates or empty:"));
        let prompt = ChoicePrompt {
            title: "Tidy memory".into(),
            body,
            options: vec![
                format!("Remove {n} note{}", if n == 1 { "" } else { "s" }),
                "Cancel".into(),
            ],
            index: 0,
            esc_index: 1,
            hint: Some("Esc cancel".into()),
            action: ChoiceAction::MemoryTidy { remove },
        };
        match out {
            ReplOutput::Tui(st) => {
                if let Ok(mut g) = st.lock() {
                    g.choice_prompts.push_front(prompt);
                    g.touch_transcript();
                }
            }
            _ => {
                for l in &prompt.body {
                    out.println(l);
                }
                out.println("[memory] run `/memory tidy apply` to remove them");
            }
        }
    }
}
