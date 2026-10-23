//! The `/mcp` panel's actions: in-session sign-in, reconnect, sign out and
//! turning a server off for this session.

use super::*;
use crate::tui::mcp_panel::{McpAction, McpPanel, McpRow};

/// A sign-in running in the background: pasted redirect URLs go to it.
pub(super) struct McpAuthTask {
    paste: tokio::sync::mpsc::UnboundedSender<String>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for McpAuthTask {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn with_panel(st: &Arc<Mutex<TuiSessionState>>, f: impl FnOnce(&mut McpPanel)) {
    if let Ok(mut g) = st.lock()
        && let Some(p) = g.mcp_panel.as_mut()
    {
        f(p);
    }
}

impl Repl {
    /// One panel row per configured server.
    pub(super) fn mcp_rows(&self) -> Vec<McpRow> {
        self.runtime
            .config()
            .mcp
            .servers
            .iter()
            .map(|s| McpRow {
                name: s.name.clone(),
                url: s.url.clone(),
                origin: if s.origin.is_empty() {
                    "config.toml".into()
                } else {
                    s.origin.clone()
                },
                enabled: s.enabled,
                signed_in: s.url.as_deref().is_some_and(crate::mcp_cli::signed_in),
                tools: self.runtime.mcp_tool_count(&s.name),
            })
            .collect()
    }

    pub(super) async fn handle_mcp_action(
        &mut self,
        action: McpAction,
        st: &Arc<Mutex<TuiSessionState>>,
        auth: &mut Option<McpAuthTask>,
    ) {
        match action {
            McpAction::Authenticate(name) => {
                *auth = Some(self.start_mcp_sign_in(name, st));
            }
            McpAction::PastedRedirect(url) => match auth.as_ref() {
                Some(task) => {
                    let _ = task.paste.send(url);
                }
                None => with_panel(st, |p| {
                    p.finish_auth(
                        "No sign-in is running; choose Authenticate again.".into(),
                        true,
                    )
                }),
            },
            McpAction::CancelAuth => {
                *auth = None;
            }
            McpAction::Reconnect(name) => {
                let result = self.runtime.reconnect_mcp_server(&name).await;
                let tools = self.runtime.mcp_tool_count(&name);
                let (msg, error) = match &result {
                    Ok(n) => (
                        format!(
                            "Connected to `{name}` · {n} tool{}",
                            if *n == 1 { "" } else { "s" }
                        ),
                        false,
                    ),
                    Err(e) => (format!("`{name}` failed: {e}"), true),
                };
                if let Ok(mut g) = st.lock() {
                    if let Some(p) = g.mcp_panel.as_mut() {
                        p.busy = None;
                        if let Some(row) = p.rows.iter_mut().find(|r| r.name == name) {
                            row.tools = tools;
                            row.enabled = true;
                        }
                        if p.in_auth() {
                            let done = if error {
                                format!("Signed in, but connecting failed: {msg}")
                            } else {
                                format!("Authentication successful. {msg}.")
                            };
                            p.finish_auth(done, error);
                        } else {
                            p.status = Some((msg.clone(), error));
                        }
                    }
                    g.push_block(DisplayBlock::System(format!("[mcp] {msg}")));
                    g.touch_transcript();
                }
            }
            McpAction::SignOut(name) => {
                let result = crate::mcp_cli::sign_out(self.runtime.config(), &name);
                if result.is_ok() {
                    self.runtime.disconnect_mcp_server(&name).await;
                }
                with_panel(st, |p| {
                    p.busy = None;
                    if let Some(row) = p.rows.iter_mut().find(|r| r.name == name) {
                        row.signed_in = false;
                        row.tools = 0;
                    }
                    p.status = Some(match result {
                        Ok(true) => (format!("Signed out of `{name}`."), false),
                        Ok(false) => (format!("`{name}` was not signed in."), false),
                        Err(e) => (e.to_string(), true),
                    });
                });
            }
            McpAction::SetEnabled(name, false) => {
                self.runtime.disconnect_mcp_server(&name).await;
                with_panel(st, |p| {
                    p.busy = None;
                    if let Some(row) = p.rows.iter_mut().find(|r| r.name == name) {
                        row.enabled = false;
                        row.tools = 0;
                    }
                    p.status = Some((format!("`{name}` is off for this session."), false));
                });
            }
            McpAction::SetEnabled(name, true) => {
                Box::pin(self.handle_mcp_action(McpAction::Reconnect(name), st, auth)).await;
            }
        }
    }

    /// Run the OAuth sign-in for `name` in the background: show the URL in
    /// the panel, open the browser, take the loopback redirect or a pasted
    /// one, save the tokens, then reconnect the server.
    fn start_mcp_sign_in(&self, name: String, st: &Arc<Mutex<TuiSessionState>>) -> McpAuthTask {
        let (paste, mut pasted) = tokio::sync::mpsc::unbounded_channel::<String>();
        let config = self.runtime.config().clone();
        let st = st.clone();
        let cmd_tx = self.self_cmd_tx.clone();
        let task = tokio::spawn(async move {
            let fail = |st: &Arc<Mutex<TuiSessionState>>, msg: String| {
                with_panel(st, |p| p.finish_auth(msg, true));
            };
            let flow = match crate::mcp_cli::begin_login(&config, &name).await {
                Ok(Some(flow)) => flow,
                Ok(None) => {
                    with_panel(&st, |p| {
                        if let Some(a) = p.auth_mut() {
                            a.note = Some((
                                format!("`{name}` does not ask for sign-in. Connecting…"),
                                false,
                            ));
                        }
                    });
                    if let Some(tx) = &cmd_tx {
                        let _ = tx.send(TuiCmd::McpAction(McpAction::Reconnect(name)));
                    }
                    return;
                }
                Err(e) => return fail(&st, format!("Could not start sign-in: {e}")),
            };
            let opened = crate::oauth_login::try_open_browser(&flow.auth_url);
            with_panel(&st, |p| {
                if let Some(a) = p.auth_mut() {
                    a.url = Some(flow.auth_url.clone());
                    a.note = Some((
                        if opened {
                            "Waiting for you to approve in the browser…".into()
                        } else {
                            "Open the URL above (c copies it), then approve.".into()
                        },
                        false,
                    ));
                }
            });
            let code = {
                let wait = flow.wait_browser();
                tokio::pin!(wait);
                let deadline = tokio::time::sleep(std::time::Duration::from_secs(600));
                tokio::pin!(deadline);
                loop {
                    tokio::select! {
                        r = &mut wait => break r,
                        _ = &mut deadline => break Err(anyhow::anyhow!("timed out waiting for the sign-in")),
                        Some(url) = pasted.recv() => match flow.code_from_pasted(&url) {
                            Ok(code) => break Ok(code),
                            Err(e) => with_panel(&st, |p| {
                                if let Some(a) = p.auth_mut() {
                                    a.note = Some((format!("{e}"), true));
                                }
                            }),
                        },
                    }
                }
            };
            let code = match code {
                Ok(c) => c,
                Err(e) => return fail(&st, format!("Sign-in failed: {e}")),
            };
            with_panel(&st, |p| {
                if let Some(a) = p.auth_mut() {
                    a.note = Some(("Signed in. Connecting…".into(), false));
                }
            });
            if let Err(e) = flow.finish(&code).await {
                return fail(&st, format!("Sign-in failed: {e}"));
            }
            if let Some(tx) = &cmd_tx {
                let _ = tx.send(TuiCmd::McpAction(McpAction::Reconnect(name)));
            }
        });
        McpAuthTask { paste, task }
    }
}
