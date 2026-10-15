//! Branch-picker logic: filtering the branch list by the typed query and
//! resolving Enter into a switch-or-create command. Extracted from `tui::app`.

use crate::tui::app::TuiCmd;

pub(crate) fn branch_filter_text(query: &str) -> &str {
    query.trim().strip_prefix('/').unwrap_or(query.trim())
}

pub(crate) fn filtered_branch_indices(branches: &[String], query: &str) -> Vec<usize> {
    let filter = branch_filter_text(query).to_ascii_lowercase();
    if filter.is_empty() {
        return (0..branches.len()).collect();
    }
    branches
        .iter()
        .enumerate()
        .filter(|(_, branch)| branch.to_ascii_lowercase().contains(&filter))
        .map(|(idx, _)| idx)
        .collect()
}

pub(crate) fn branch_picker_enter_command(
    branches: &[String],
    query: &str,
    selected_filtered_idx: usize,
) -> Option<TuiCmd> {
    let raw_query = query.trim();
    let branch_name = branch_filter_text(raw_query).trim();
    let filtered = filtered_branch_indices(branches, raw_query);

    if raw_query.starts_with('/') {
        return (!branch_name.is_empty()).then(|| TuiCmd::CreateBranch(branch_name.to_string()));
    }

    if !branch_name.is_empty()
        && let Some((idx, _)) = branches
            .iter()
            .enumerate()
            .find(|(_, branch)| branch.eq_ignore_ascii_case(branch_name))
    {
        return Some(TuiCmd::SwitchBranch(branches[idx].clone()));
    }

    filtered
        .get(selected_filtered_idx)
        .copied()
        .map(|idx| TuiCmd::SwitchBranch(branches[idx].clone()))
}
