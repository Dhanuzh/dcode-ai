use super::*;

pub(super) fn collect_worktree_entries(
    workspace_root: &Path,
) -> anyhow::Result<Vec<WorktreeCommandEntry>> {
    let manager = dcode_ai_runtime::worktree::WorktreeManager::new(workspace_root);
    if !manager.is_git_repo() {
        anyhow::bail!(
            "not a git repository: {}",
            dcode_ai_common::config::canonicalize_simplified(workspace_root)
                .unwrap_or_else(|_| workspace_root.to_path_buf())
                .display()
        );
    }

    let mut entries = manager
        .list_worktrees()
        .into_iter()
        .map(|info| {
            let changed_files = manager
                .changed_files(&info.worktree_path, &info.base_branch)
                .len();
            let (ahead, behind) = manager.ahead_behind(&info.worktree_path, &info.base_branch);
            WorktreeCommandEntry {
                session_id: info.session_id,
                branch_name: info.branch_name,
                base_branch: info.base_branch,
                worktree_path: info.worktree_path,
                changed_files,
                ahead,
                behind,
            }
        })
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.session_id.cmp(&right.session_id));
    Ok(entries)
}

pub(super) fn list_worktrees(workspace_root: &Path, json: bool) -> anyhow::Result<()> {
    let worktrees = collect_worktree_entries(workspace_root)?;
    if json {
        print_json(&WorktreeListOutput { worktrees }, false)?;
    } else if worktrees.is_empty() {
        println!("No dcode-ai worktrees found");
    } else {
        for worktree in worktrees {
            print_human_worktree(&worktree);
        }
    }
    Ok(())
}

pub(super) fn prune_worktrees(workspace_root: &Path, json: bool) -> anyhow::Result<()> {
    let manager = dcode_ai_runtime::worktree::WorktreeManager::new(workspace_root);
    if !manager.is_git_repo() {
        anyhow::bail!(
            "not a git repository: {}",
            dcode_ai_common::config::canonicalize_simplified(workspace_root)
                .unwrap_or_else(|_| workspace_root.to_path_buf())
                .display()
        );
    }
    manager.prune_stale();
    let worktrees = collect_worktree_entries(workspace_root)?;
    if json {
        print_json(
            &WorktreePruneOutput {
                pruned: true,
                worktrees,
            },
            false,
        )?;
    } else {
        println!("Pruned git worktree metadata");
        if worktrees.is_empty() {
            println!("No dcode-ai worktrees found");
        } else {
            for worktree in worktrees {
                print_human_worktree(&worktree);
            }
        }
    }
    Ok(())
}

pub(super) fn merge_worktree(
    workspace_root: &Path,
    session_id: &str,
    base: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let manager = dcode_ai_runtime::worktree::WorktreeManager::new(workspace_root);
    if !manager.is_git_repo() {
        anyhow::bail!(
            "not a git repository: {}",
            dcode_ai_common::config::canonicalize_simplified(workspace_root)
                .unwrap_or_else(|_| workspace_root.to_path_buf())
                .display()
        );
    }

    let entries = collect_worktree_entries(workspace_root)?;
    let listed = entries.iter().find(|entry| entry.session_id == session_id);
    let base_branch = match base {
        Some(branch) => branch.to_string(),
        None => listed
            .map(|entry| entry.base_branch.clone())
            .ok_or_else(|| {
                anyhow::anyhow!("no dcode-ai worktree found for session `{session_id}`")
            })?,
    };
    let branch_name = listed
        .map(|entry| entry.branch_name.clone())
        .unwrap_or_else(|| format!("dcode-ai/{session_id}"));

    // Uncommitted sub-agent work would otherwise be left behind.
    let worktree = workspace_root
        .join(".dcode-ai")
        .join("worktrees")
        .join(session_id);
    if worktree.exists() {
        manager
            .commit_pending(&worktree, &format!("dcode-ai sub-agent {session_id}"))
            .map_err(anyhow::Error::msg)?;
    }
    manager
        .merge_into_base(session_id, &base_branch)
        .map_err(anyhow::Error::msg)?;

    let output = WorktreeMergeOutput {
        session_id: session_id.to_string(),
        branch_name,
        base_branch,
        merged: true,
    };
    if json {
        print_json(&output, false)?;
    } else {
        println!(
            "Merged {} into {} for {}",
            output.branch_name, output.base_branch, output.session_id
        );
    }
    Ok(())
}

pub(super) fn print_human_worktree(worktree: &WorktreeCommandEntry) {
    println!(
        "{}  branch={}  base={}  ahead={}  behind={}  changed={}  path={}",
        worktree.session_id,
        worktree.branch_name,
        worktree.base_branch,
        worktree.ahead,
        worktree.behind,
        worktree.changed_files,
        worktree.worktree_path.display()
    );
}
