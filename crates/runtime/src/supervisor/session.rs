//! Session lifecycle management: creation, naming, query, cleanup, and pruning.

use crate::last_session::LastSessionStore;
use crate::session_store::SessionStore;
use chrono::Utc;
use dcode_ai_common::config::DcodeAiConfig;
use dcode_ai_common::message::{MessageContent, Role};
use dcode_ai_common::session::{SessionState, SessionStatus};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Generate a unique session ID based on microsecond timestamp + monotonic counter.
pub fn generate_session_id() -> String {
    static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("session-{}-{counter}", Utc::now().timestamp_micros())
}

/// Derive a session name from the first non-empty line of a prompt.
pub fn derive_session_name(prompt: &str) -> Option<String> {
    let first_line = prompt
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(str::trim)?;
    normalize_session_name(first_line)
}

/// Normalize a raw session name: collapse whitespace, strip control chars, cap length.
pub fn normalize_session_name(raw: &str) -> Option<String> {
    if raw.trim().is_empty() {
        return None;
    }
    let collapsed = raw
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>();
    let compact = collapsed.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return None;
    }
    const MAX: usize = 72;
    let mut out = String::new();
    for ch in compact.chars().take(MAX) {
        out.push(ch);
    }
    if compact.chars().count() > MAX {
        out.push('…');
    }
    Some(out)
}

/// Query the current state of a session from its store.
pub async fn query_session_state(
    session_store: &SessionStore,
    session_id: &str,
) -> Result<SessionState, String> {
    session_store
        .load(session_id)
        .await
        .map_err(|e| e.to_string())
}

/// List all session IDs in a workspace.
pub async fn list_sessions(session_store: &SessionStore) -> Result<Vec<String>, String> {
    session_store.list().await.map_err(|e| e.to_string())
}

/// Remove sessions that have no meaningful interaction history.
/// Returns the deleted session IDs.
pub async fn cleanup_empty_sessions(session_store: &SessionStore) -> Result<Vec<String>, String> {
    let ids = session_store.list().await.map_err(|e| e.to_string())?;
    let mut to_delete = Vec::new();
    for id in ids {
        let session = match session_store.load(&id).await {
            Ok(s) => s,
            Err(_) => continue,
        };
        if session.meta.status == SessionStatus::Running {
            continue;
        }
        if !session_state_has_meaningful_interaction(&session) {
            to_delete.push(id);
        }
    }
    session_store
        .delete_many(&to_delete)
        .await
        .map_err(|e| e.to_string())
}

fn session_state_has_meaningful_interaction(session: &SessionState) -> bool {
    let has_non_system_message = session.messages.iter().any(|m| {
        !matches!(m.role, Role::System)
            && match &m.content {
                MessageContent::Text(text) => !text.trim().is_empty(),
                MessageContent::Parts(parts) => !parts.is_empty(),
            }
    });
    has_non_system_message
        || session.total_input_tokens > 0
        || session.total_output_tokens > 0
        || session.estimated_cost_usd > 0.0
        || session
            .meta
            .session_name
            .as_ref()
            .is_some_and(|name| !name.trim().is_empty())
        || session
            .meta
            .session_summary
            .as_ref()
            .is_some_and(|summary| !summary.trim().is_empty())
        || session.meta.parent_session_id.is_some()
        || !session.meta.child_session_ids.is_empty()
        || session.meta.orchestration.is_some()
}

/// Clean up stale sessions: sessions marked as Running whose PID is no longer alive
/// and whose socket no longer exists. Marks them as Error.
pub async fn cleanup_stale_sessions(session_store: &SessionStore) {
    let ids = match session_store.list().await {
        Ok(ids) => ids,
        Err(_) => return,
    };

    for id in ids {
        let mut session = match session_store.load(&id).await {
            Ok(s) => s,
            Err(_) => continue,
        };

        if session.meta.status != SessionStatus::Running {
            continue;
        }

        let pid_alive = session.meta.pid.map(is_pid_alive).unwrap_or(false);

        let socket_exists = session
            .meta
            .socket_path
            .as_ref()
            .map(|p| p.exists())
            .unwrap_or(false);

        if !pid_alive && !socket_exists {
            session.meta.status = SessionStatus::Error;
            session.meta.updated_at = Utc::now();
            let _ = session_store.save(&session).await;
        }
    }
}

fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        false
    }
}

/// Configuration for pruning sessions.
#[derive(Debug, Clone)]
pub struct PruneConfig {
    /// Keep only this many most-recent sessions (by updated_at).
    /// Sessions beyond this count are deleted. Set to 0 to delete all eligible.
    pub keep_last: usize,
    /// Delete sessions older than this duration.
    pub older_than: chrono::Duration,
    /// Only prune sessions matching these statuses. Empty = all statuses.
    pub status_filter: Vec<SessionStatus>,
    /// If true, only report what would be deleted without actually deleting.
    pub dry_run: bool,
    /// Also remove associated worktrees.
    pub remove_worktrees: bool,
}

impl Default for PruneConfig {
    fn default() -> Self {
        Self {
            keep_last: 20,
            older_than: chrono::Duration::hours(7 * 24), // 7 days
            status_filter: Vec::new(),
            dry_run: false,
            remove_worktrees: true,
        }
    }
}

/// Result of a prune operation.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PruneResult {
    pub deleted: Vec<String>,
    pub skipped_running: Vec<String>,
    pub worktrees_removed: Vec<String>,
    pub dry_run: bool,
}

/// Prune sessions based on age and count limits.
/// Always preserves Running sessions.
pub async fn prune_sessions(
    session_store: &SessionStore,
    workspace_root: &Path,
    cfg: &PruneConfig,
) -> Result<PruneResult, String> {
    let (snapshots, _unreadable) = session_store
        .load_all_snapshots()
        .await
        .map_err(|e| e.to_string())?;

    let now = Utc::now();

    // Separate running sessions from the rest
    let running_ids: Vec<String> = snapshots
        .iter()
        .filter(|s| s.status == SessionStatus::Running)
        .map(|s| s.id.clone())
        .collect();

    // Sort non-running by updated_at descending (most recent first)
    let mut candidates: Vec<_> = snapshots
        .iter()
        .filter(|s| s.status != SessionStatus::Running)
        .collect();
    candidates.sort_by_key(|b| std::cmp::Reverse(b.updated_at));

    // Apply status filter
    let candidates: Vec<_> = if cfg.status_filter.is_empty() {
        candidates
    } else {
        candidates
            .into_iter()
            .filter(|s| cfg.status_filter.contains(&s.status))
            .collect()
    };

    // Apply age filter: keep sessions within the time window
    let mut to_delete: Vec<String> = candidates
        .iter()
        .filter(|s| now.signed_duration_since(s.updated_at) > cfg.older_than)
        .map(|s| s.id.clone())
        .collect();

    // Apply keep-last: the first N (most recent) are protected
    if cfg.keep_last > 0 && candidates.len() > cfg.keep_last {
        let protected: std::collections::HashSet<String> = candidates
            .iter()
            .take(cfg.keep_last)
            .map(|s| s.id.clone())
            .collect();
        to_delete.retain(|id| !protected.contains(id));
        // Also add candidates beyond keep-last that aren't already in to_delete
        for s in candidates.iter().skip(cfg.keep_last) {
            if !to_delete.contains(&s.id) {
                to_delete.push(s.id.clone());
            }
        }
    }

    // Deduplicate
    to_delete.sort();
    to_delete.dedup();

    // Remove running sessions from delete list
    let mut skipped_running = Vec::new();
    to_delete.retain(|id| {
        if running_ids.contains(id) {
            skipped_running.push(id.clone());
            false
        } else {
            true
        }
    });

    if cfg.dry_run {
        return Ok(PruneResult {
            deleted: to_delete,
            skipped_running,
            worktrees_removed: Vec::new(),
            dry_run: true,
        });
    }

    // Perform deletion
    let deleted = session_store
        .delete_many(&to_delete)
        .await
        .map_err(|e| e.to_string())?;

    // Remove worktrees
    let mut worktrees_removed = Vec::new();
    if cfg.remove_worktrees {
        let wt_mgr = crate::worktree::WorktreeManager::new(workspace_root);
        for id in &deleted {
            if let Err(e) = wt_mgr.remove_worktree(id, true) {
                tracing::warn!("failed to remove worktree for session {id}: {e}");
            } else {
                worktrees_removed.push(id.clone());
            }
        }
    }

    Ok(PruneResult {
        deleted,
        skipped_running,
        worktrees_removed,
        dry_run: false,
    })
}

/// Get the last session ID from `.dcode-ai/.last_session`, if it exists and is valid.
/// Falls back to finding the most recently updated session in the sessions directory.
pub async fn get_last_session_id(
    config: &DcodeAiConfig,
    workspace_root: &Path,
) -> anyhow::Result<Option<String>> {
    // First, try the explicit last-session pointer
    let store = LastSessionStore::new(workspace_root.join(&config.session.last_session_file));
    match store.load().await {
        Ok(Some(id)) => {
            // Verify the session still exists on disk.
            let session_store = SessionStore::new(workspace_root.join(&config.session.history_dir));
            match session_store.load(&id).await {
                Ok(_) => return Ok(Some(id)),
                Err(_) => {
                    // Session file missing or corrupted; clear the stale pointer.
                    let _ = store.clear().await;
                }
            }
        }
        Ok(None) => {
            // No pointer file - fall through to scan sessions dir
        }
        Err(e) => {
            tracing::warn!("failed to load last session pointer: {}", e);
            // Fall through to scan sessions dir
        }
    }

    // Fallback: find the most recently updated session in the sessions directory
    let session_store = SessionStore::new(workspace_root.join(&config.session.history_dir));
    let ids = match session_store.list().await {
        Ok(ids) => ids,
        Err(e) => {
            tracing::debug!("failed to list sessions: {}", e);
            return Ok(None);
        }
    };

    let mut latest: Option<(String, chrono::DateTime<chrono::Utc>)> = None;
    for id in ids {
        match session_store.load(&id).await {
            Ok(session) => {
                let should_replace = latest
                    .as_ref()
                    .map(|(_, updated_at)| session.meta.updated_at > *updated_at)
                    .unwrap_or(true);
                if should_replace {
                    latest = Some((session.meta.id, session.meta.updated_at));
                }
            }
            Err(_) => continue,
        }
    }

    if let Some((id, _)) = latest {
        // Update the last-session pointer for future runs
        let _ = store.save(&id).await;
        Ok(Some(id))
    } else {
        Ok(None)
    }
}
