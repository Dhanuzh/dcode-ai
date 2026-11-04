//! The agent's scratch folder: working files it makes along the way (test
//! logs, screenshots, API responses, generated data), shown live in the
//! dashboard's scratch tab. `<project>/.dcode-ai/scratch/<session-id>/`,
//! git-ignored with the rest of `.dcode-ai/`.

use std::path::{Path, PathBuf};

/// Where session `session_id` keeps its scratch files (a sub-agent's
/// worktree uses its project's folder).
pub fn scratch_dir(workspace: &Path, session_id: &str) -> PathBuf {
    crate::board::project_root(workspace)
        .join(".dcode-ai")
        .join("scratch")
        .join(session_id)
}

/// The system prompt's note about it.
pub fn prompt_section(dir: &Path) -> String {
    format!(
        "\n\n# Scratch folder\nPut working files you make along the way in `{}`: test and \
command logs (e.g. `cmd 2>&1 | tee {0}/tests.log`), screenshots, API responses (JSON), generated \
data. The user sees that folder live in the dashboard, so they see what you see. Never put \
secrets there, and don't put project deliverables there (those belong in the project).",
        dir.display()
    )
}
