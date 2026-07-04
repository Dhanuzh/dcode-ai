use crate::ipc_pending::{ApprovalPendingMap, QuestionPendingMap};
use dcode_ai_common::config::{DcodeAiConfig, PermissionMode};
use dcode_ai_common::event::{AgentEvent, EndReason, QuestionSelection};
use dcode_ai_common::session::{OrchestrationContext, SessionSnapshot};
use dcode_ai_core::approval::{ApprovalHandler, ApprovalVerdict};
use dcode_ai_core::provider::ProviderError;
use dcode_ai_core::tools::spawn_subagent::SpawnRequest;
use dcode_ai_runtime::ipc::IpcHandle;
use dcode_ai_runtime::supervisor::{Supervisor, SupervisorConfig, SupervisorHandle};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::mpsc;

/// Resolve a pending `ask_question` without going through `SessionRuntime` (e.g. TUI side task
/// while `run_turn` is blocked waiting on the same question).
pub fn dispatch_question_answer(
    qp: &Option<QuestionPendingMap>,
    question_id: &str,
    selection: QuestionSelection,
) -> bool {
    let Some(qp) = qp else {
        return false;
    };
    let Ok(mut m) = qp.lock() else {
        return false;
    };
    let Some(tx) = m.remove(question_id) else {
        return false;
    };
    tx.send(selection).is_ok()
}

/// Resolve a pending approval without going through the main command loop.
pub fn dispatch_tool_approval(
    approvals: &Option<ApprovalPendingMap>,
    call_id: &str,
    verdict: ApprovalVerdict,
) -> bool {
    let Some(approvals) = approvals else {
        return false;
    };
    let Ok(mut map) = approvals.lock() else {
        return false;
    };
    let Some(tx) = map.remove(call_id) else {
        return false;
    };
    tx.send(verdict).is_ok()
}

/// Thin CLI wrapper around the runtime `Supervisor`.
/// Keeps the same public API so existing CLI code (repl, main) works unchanged.
pub struct SessionRuntime {
    supervisor: Supervisor,
    handle: Option<SupervisorHandle>,
    question_pending: Option<QuestionPendingMap>,
    config: DcodeAiConfig,
    /// Shared map of pending tool-call approvals. Stored so `request_cancel()`
    /// can deny all pending approvals immediately instead of letting the
    /// approval handler block for up to 300 seconds.
    ipc_approval_pending: Option<ApprovalPendingMap>,
}

impl SessionRuntime {
    pub fn take_event_rx(&mut self) -> Option<tokio::sync::mpsc::Receiver<AgentEvent>> {
        self.handle.as_mut()?.take_event_rx()
    }

    pub fn event_log_path(&self) -> std::path::PathBuf {
        self.supervisor.event_log_path()
    }

    pub async fn run_turn(&mut self, prompt: &str) -> Result<String, ProviderError> {
        self.supervisor.run_turn(prompt).await
    }

    pub async fn run_turn_with_images(
        &mut self,
        prompt: &str,
        attachments: Vec<dcode_ai_common::message::ImageAttachment>,
    ) -> Result<String, ProviderError> {
        self.supervisor
            .run_turn_with_images(prompt, &attachments)
            .await
    }

    pub async fn finish(&mut self, reason: EndReason) {
        self.supervisor.finish(reason).await;
    }

    pub async fn save(&self) -> Result<(), String> {
        self.supervisor.save().await
    }

    pub fn take_ipc_handle(&mut self) -> Option<IpcHandle> {
        self.handle.as_mut()?.take_ipc_handle()
    }

    pub fn take_ipc_approval_pending(&mut self) -> Option<ApprovalPendingMap> {
        let map = self.handle.as_mut()?.take_approval_pending();
        self.ipc_approval_pending = map.clone();
        map
    }

    /// Pending `ask_question` resolvers (same map the runtime tool waits on).
    pub fn question_pending(&self) -> Option<QuestionPendingMap> {
        self.question_pending.clone()
    }

    /// Submit an answer for the current interactive question (TUI / REPL).
    pub fn submit_question_answer(&self, question_id: &str, selection: QuestionSelection) -> bool {
        dispatch_question_answer(&self.question_pending, question_id, selection)
    }

    /// Accept the model's suggested answer when exactly one question is pending.
    pub fn submit_suggested_answer(&self) -> bool {
        let Some(ref qp) = self.question_pending else {
            return false;
        };
        let Ok(mut m) = qp.lock() else {
            return false;
        };
        let keys: Vec<String> = m.keys().cloned().collect();
        if keys.len() != 1 {
            return false;
        }
        let id = keys[0].clone();
        let Some(tx) = m.remove(&id) else {
            return false;
        };
        tx.send(QuestionSelection::Suggested).is_ok()
    }

    pub fn session_id(&self) -> &str {
        self.supervisor.session_id()
    }

    pub fn session_name(&self) -> Option<&str> {
        self.supervisor.session_name()
    }

    pub fn model(&self) -> &str {
        &self.supervisor.model
    }

    pub fn workspace_root(&self) -> &std::path::Path {
        &self.supervisor.workspace_root
    }

    pub fn take_spawn_rx(&mut self) -> Option<mpsc::Receiver<SpawnRequest>> {
        self.handle.as_mut()?.take_spawn_rx()
    }

    pub fn messages(&self) -> &[dcode_ai_common::message::Message] {
        &self.supervisor.agent().messages
    }

    pub fn set_model(&mut self, model: impl Into<String>) {
        let model = model.into();
        self.supervisor.model = model.clone();
        self.supervisor.agent_mut().model = model;
    }

    pub fn permission_mode(&self) -> PermissionMode {
        self.supervisor.agent().approval.mode()
    }

    pub fn set_permission_mode(&mut self, mode: PermissionMode) {
        self.supervisor.agent_mut().approval.set_mode(mode);
    }

    pub fn add_session_allow_pattern(&mut self, pattern: String) {
        self.supervisor
            .agent_mut()
            .approval
            .add_session_allow(pattern);
    }

    pub fn request_cancel(&self) {
        self.supervisor.request_cancel();
        // Also deny all pending approvals so the approval handler doesn't
        // block for up to 300 seconds while the cancel flag goes unchecked.
        if let Some(ref map) = self.ipc_approval_pending
            && let Ok(mut m) = map.lock()
        {
            for (_, tx) in m.drain() {
                let _ = tx.send(ApprovalVerdict::Denied);
            }
        }
    }

    pub fn cancel_handle(&self) -> Arc<AtomicBool> {
        self.supervisor.cancel_handle()
    }

    pub fn event_tx(&self) -> Option<tokio::sync::mpsc::Sender<AgentEvent>> {
        self.supervisor.event_tx()
    }

    pub fn mcp_manager(&self) -> &Arc<dcode_ai_core::tools::mcp::McpConnectionManager> {
        self.supervisor.mcp_manager()
    }

    pub async fn list_session_ids(&self) -> Result<Vec<String>, String> {
        let store = dcode_ai_runtime::session_store::SessionStore::new(
            self.workspace_root().join(&self.config.session.history_dir),
        );
        store.list().await.map_err(|err| err.to_string())
    }

    pub async fn cleanup_empty_sessions(&self) -> Result<Vec<String>, String> {
        let store = dcode_ai_runtime::session_store::SessionStore::new(
            self.workspace_root().join(&self.config.session.history_dir),
        );
        dcode_ai_runtime::supervisor::cleanup_empty_sessions(&store).await
    }

    pub fn config(&self) -> &DcodeAiConfig {
        &self.config
    }

    pub fn config_mut(&mut self) -> &mut DcodeAiConfig {
        &mut self.config
    }

    /// Replace merged config and rebuild the provider (fails if API key missing, etc.).
    pub fn apply_dcode_ai_config(&mut self, config: DcodeAiConfig) -> Result<(), ProviderError> {
        self.supervisor.apply_dcode_ai_config(config.clone())?;
        self.config = config;
        Ok(())
    }

    /// Sync the current config into the supervisor and rebuild the system prompt
    /// in place (e.g. after `/personality`). No provider rebuild.
    pub fn refresh_system_prompt(&mut self) {
        self.supervisor.refresh_system_prompt(self.config.clone());
    }

    pub fn snapshot(&self) -> SessionSnapshot {
        self.supervisor.snapshot()
    }

    pub fn compact_summary(&self) -> String {
        self.supervisor.compact_summary()
    }

    pub fn compaction_preview(&self) -> String {
        self.supervisor.compaction_preview()
    }

    pub fn set_session_summary(&mut self, summary: Option<String>) {
        self.supervisor.set_session_summary(summary);
    }

    pub fn set_session_name(&mut self, name: Option<String>) {
        self.supervisor.set_session_name(name);
    }

    pub fn undo_last_turn(&mut self) -> Result<Option<String>, String> {
        self.supervisor.undo_last_turn()
    }

    pub fn redo_last_turn(&mut self) -> Result<Option<String>, String> {
        self.supervisor.redo_last_turn()
    }

    pub async fn append_memory_note(
        &self,
        kind: &str,
        content: Option<String>,
    ) -> Result<(), String> {
        self.supervisor.append_memory_note(kind, content).await
    }

    pub fn memory_store_path(&self) -> std::path::PathBuf {
        self.supervisor.memory_store_path()
    }

    /// Start a fresh session: save the current one, generate a new ID, clear messages.
    pub async fn new_session(&mut self) -> Result<(), String> {
        self.supervisor.finish(EndReason::Completed).await;
        self.supervisor.save().await?;
        self.supervisor.reset_for_new_session();
        Ok(())
    }

    /// Fork the current session: save the original, then continue with the same
    /// conversation under a new session ID. Returns the new ID.
    pub async fn fork_session(&mut self) -> Result<String, String> {
        self.supervisor.save().await?;
        let new_id = self.supervisor.fork_session();
        self.supervisor.save().await?;
        Ok(new_id)
    }

    /// Snapshot conversation messages for an ephemeral `/side` aside.
    pub fn snapshot_messages(&self) -> Vec<dcode_ai_common::message::Message> {
        self.supervisor.snapshot_messages()
    }

    /// Restore messages captured by [`Self::snapshot_messages`], discarding the
    /// aside (returning to the main thread).
    pub fn restore_messages(&mut self, messages: Vec<dcode_ai_common::message::Message>) {
        self.supervisor.restore_messages(messages);
    }

    /// Write a line to an interactive_exec session's stdin (local — the text
    /// never reaches the model). Used by `/input` for e.g. sudo passwords.
    pub fn interactive_write(&self, id: u32, text: &str) -> Result<(), String> {
        self.supervisor.interactive_write(id, text)
    }

    /// List active interactive sessions as `(id, running, command)`.
    pub fn interactive_sessions(&self) -> Vec<(u32, bool, String)> {
        self.supervisor.interactive_sessions()
    }

    /// Save the current session and swap in a resumed one in-process.
    pub async fn resume_in_process(
        &mut self,
        session_id: &str,
        safe_mode: bool,
        interactive_approvals: bool,
        approval_handler: Option<Arc<dyn dcode_ai_core::approval::ApprovalHandler>>,
    ) -> Result<(), ProviderError> {
        // Preserve session-scoped allow patterns (e.g. startup-approve-all `*`)
        // so they survive across session resume boundaries.
        let old_allow = self.supervisor.agent().approval.session_allow.clone();

        let _ = self.supervisor.save().await;
        self.supervisor.finish(EndReason::Completed).await;

        let mut new_runtime = build_resumed_session_runtime(
            self.config.clone(),
            &self.supervisor.workspace_root.clone(),
            safe_mode,
            interactive_approvals,
            session_id,
            approval_handler,
        )
        .await?;

        // Carry over session-scoped allow patterns from the old supervisor.
        for pattern in old_allow {
            new_runtime.add_session_allow_pattern(pattern);
        }

        // Refresh the approval-pending map reference so the new supervisor's
        // pending approvals can be resolved on cancel.
        new_runtime.ipc_approval_pending = new_runtime
            .handle
            .as_mut()
            .and_then(|h| h.take_approval_pending());

        *self = new_runtime;
        Ok(())
    }

    /// Save the current session, then re-root the whole runtime to a different
    /// workspace directory (rebuilds tools/provider/MCP) and start a fresh
    /// session there. Used by `/project` switching.
    pub async fn reroot_in_process(
        &mut self,
        workspace_root: &Path,
        safe_mode: bool,
        interactive_approvals: bool,
        approval_handler: Option<Arc<dyn dcode_ai_core::approval::ApprovalHandler>>,
    ) -> Result<(), ProviderError> {
        // Preserve session-scoped allow patterns across the reroot.
        let old_allow = self.supervisor.agent().approval.session_allow.clone();

        let _ = self.supervisor.save().await;
        self.supervisor.finish(EndReason::Completed).await;

        // Re-layer config for the target workspace (global → project
        // `.dcode.toml` → local) so each project picks up its own settings.
        // Fall back to the current config if the new workspace has none.
        let config = DcodeAiConfig::load_for_workspace(workspace_root)
            .unwrap_or_else(|_| self.config.clone());

        let mut new_runtime = build_session_runtime(
            config,
            workspace_root,
            safe_mode,
            interactive_approvals,
            None,
            approval_handler,
            None,
        )
        .await?;

        // Carry over session-scoped allow patterns.
        for pattern in old_allow {
            new_runtime.add_session_allow_pattern(pattern);
        }

        // Refresh the approval-pending map reference for cancel.
        new_runtime.ipc_approval_pending = new_runtime
            .handle
            .as_mut()
            .and_then(|h| h.take_approval_pending());

        *self = new_runtime;
        Ok(())
    }
}

pub async fn build_session_runtime(
    config: DcodeAiConfig,
    workspace_root: &Path,
    safe_mode: bool,
    interactive_approvals: bool,
    session_id: Option<String>,
    ipc_approval_handler: Option<Arc<dyn ApprovalHandler>>,
    orchestration_context: Option<OrchestrationContext>,
) -> Result<SessionRuntime, ProviderError> {
    let approval_handler = ipc_approval_handler;

    let mut supervisor = Supervisor::create(SupervisorConfig {
        config: config.clone(),
        workspace_root: workspace_root.to_path_buf(),
        safe_mode,
        interactive_approvals,
        session_id,
        approval_handler,
        orchestration_context,
    })
    .await?;

    let mut handle = supervisor.take_handle();
    let question_pending = handle.take_question_pending();
    let approval_pending = handle.approval_pending_ref();
    Ok(SessionRuntime {
        supervisor,
        handle: Some(handle),
        question_pending,
        config,
        ipc_approval_pending: approval_pending,
    })
}

pub async fn build_resumed_session_runtime(
    config: DcodeAiConfig,
    workspace_root: &Path,
    safe_mode: bool,
    interactive_approvals: bool,
    session_id: &str,
    approval_handler: Option<Arc<dyn ApprovalHandler>>,
) -> Result<SessionRuntime, ProviderError> {
    let mut supervisor = Supervisor::resume(
        config.clone(),
        workspace_root,
        safe_mode,
        interactive_approvals,
        session_id,
        approval_handler,
    )
    .await?;
    let mut handle = supervisor.take_handle();
    let question_pending = handle.take_question_pending();
    let approval_pending = handle.approval_pending_ref();
    Ok(SessionRuntime {
        supervisor,
        handle: Some(handle),
        question_pending,
        config,
        ipc_approval_pending: approval_pending,
    })
}
