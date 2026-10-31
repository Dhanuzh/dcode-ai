//! Seeing and sharing a session: `/timeline`, `/files`, `/sounds`,
//! `/speak`, `/watch`, `/handoff`.

use super::*;

/// Who is handing a session over: git's user name, else the login name.
fn handoff_author(workspace: &std::path::Path) -> String {
    std::process::Command::new("git")
        .args(["config", "user.name"])
        .current_dir(workspace)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|n| !n.is_empty())
        .or_else(|| std::env::var("USER").ok())
        .or_else(|| std::env::var("USERNAME").ok())
        .unwrap_or_else(|| "a teammate".into())
}

/// Working-tree changes (paths), for the handoff note.
fn changed_files(workspace: &std::path::Path) -> Vec<String> {
    std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(workspace)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter_map(|l| l.get(3..).map(str::to_string))
                .filter(|p| !p.starts_with(".dcode-ai/"))
                .collect()
        })
        .unwrap_or_default()
}

impl Repl {
    #[allow(unused_variables, unused_mut)]
    pub(super) async fn cmd_team(
        &mut self,
        input: &str,
        command: &str,
        rest: &str,
        mut parts: std::str::SplitWhitespace<'_>,
        out: ReplOutput<'_>,
    ) -> anyhow::Result<bool> {
        match command {
            "/office" => {
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.office_open = true;
                } else {
                    out.println("[office] the office needs the full-screen terminal UI");
                }
            }
            "/timeline" => {
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.timeline_open = true;
                } else {
                    out.println("[timeline] the timeline needs the full-screen terminal UI");
                }
            }
            "/files" => {
                let lines: Vec<String> = match &out {
                    ReplOutput::Tui(st) => st
                        .lock()
                        .map(|g| {
                            let map = &g.file_map;
                            let mut lines = vec![format!(
                                "[files] {} touched · {} edited (dashboard: Ctrl+\\ then Alt+4)",
                                map.files.len(),
                                map.edited()
                            )];
                            for (depth, name, touch) in map.tree() {
                                let indent = "  ".repeat(depth + 1);
                                lines.push(match touch {
                                    None => format!("{indent}{name}"),
                                    Some(t) if t.edits > 0 => format!(
                                        "{indent}✎ {name}  {} edit{} · {}",
                                        t.edits,
                                        if t.edits == 1 { "" } else { "s" },
                                        crate::tui::activity::ago(t.last_at)
                                    ),
                                    Some(t) => format!(
                                        "{indent}· {name}  read {}× · {}",
                                        t.reads,
                                        crate::tui::activity::ago(t.last_at)
                                    ),
                                });
                            }
                            lines
                        })
                        .unwrap_or_default(),
                    _ => vec!["[files] needs the terminal UI".into()],
                };
                for l in lines {
                    out.println(&l);
                }
            }
            "/sounds" => {
                let on = match rest.trim() {
                    "on" => Some(true),
                    "off" => Some(false),
                    _ => None,
                };
                let Some(on) = on else {
                    let now = self.runtime.config().ui.sounds;
                    out.println(&format!(
                        "[sounds] {} — /sounds on|off (a bell when a long turn finishes or dcode-ai needs you)",
                        if now { "on" } else { "off" }
                    ));
                    return Ok(true);
                };
                self.runtime.config_mut().ui.sounds = on;
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.sounds = on;
                }
                let path = dcode_ai_common::config::global_config_file();
                if let Some(path) = path {
                    let _ = dcode_ai_common::config::set_toml_value(
                        &path,
                        "ui",
                        "sounds",
                        toml::Value::Boolean(on),
                    );
                }
                out.println(&format!(
                    "[sounds] {} (saved to your config)",
                    if on { "on" } else { "off" }
                ));
            }
            "/speak" => {
                let last = self
                    .runtime
                    .messages()
                    .iter()
                    .rev()
                    .find(|m| m.role == dcode_ai_common::message::Role::Assistant)
                    .map(|m| m.content.to_summary_text())
                    .unwrap_or_default();
                match crate::tui::notify::speak(&last) {
                    Ok(()) => out.println("[speak] reading the last reply aloud…"),
                    Err(e) => out.eprintln(&format!("[speak] {e}")),
                }
            }
            "/watch" => {
                let arg = rest.trim();
                if arg == "stop" {
                    if self.watch.take().is_some() {
                        out.println("[watch] stopped — the link no longer works");
                    } else {
                        out.println("[watch] no watch link is running");
                    }
                    return Ok(true);
                }
                let lan = arg == "lan";
                if let Some(w) = &self.watch
                    && w.lan == lan
                {
                    out.println(&format!("[watch] already live: {}", w.url));
                    return Ok(true);
                }
                let title = self
                    .runtime
                    .session_name()
                    .map(str::to_string)
                    .unwrap_or_else(|| self.runtime.session_id().to_string());
                match crate::watch::start(self.runtime.event_log_path(), title, lan).await {
                    Ok(w) => {
                        let _ = crate::tui::clipboard::copy_to_clipboard(&w.url);
                        out.println(&format!("[watch] live, read-only: {}", w.url));
                        if lan {
                            out.println(
                                "[watch] reachable by anyone on this network who has the link — /watch stop ends it",
                            );
                        } else {
                            out.println(
                                "[watch] this machine only (copied) · /watch lan to share on your network · /watch stop",
                            );
                        }
                        self.watch = Some(w);
                    }
                    Err(e) => out.eprintln(&format!("[watch] could not start: {e}")),
                }
            }
            "/handoff" => {
                let _ = self.runtime.save().await;
                let ws = self.runtime.workspace_root().to_path_buf();
                let todo = match &out {
                    ReplOutput::Tui(st) => st.lock().ok().and_then(|g| g.current_plan.clone()),
                    _ => None,
                };
                let handoff = crate::session_share_cli::Handoff {
                    from: handoff_author(&ws),
                    at: chrono::Utc::now(),
                    note: rest.trim().to_string(),
                    todo,
                    changed_files: changed_files(&ws),
                };
                let id = self.runtime.session_id().to_string();
                let sessions = ws.join(&self.runtime.config().session.history_dir);
                let file = ws.join(format!("{id}.dcode-session"));
                match crate::session_share_cli::export_with(
                    &sessions,
                    &id,
                    Some(file.clone()),
                    Some(handoff),
                )
                .await
                {
                    Ok(path) => {
                        out.println(&format!(
                            "[handoff] wrote {} (secrets masked) — your teammate runs `dcode-ai session import {}` and says \"continue\"",
                            path.display(),
                            path.file_name().and_then(|n| n.to_str()).unwrap_or_default()
                        ));
                        if rest.trim().is_empty() {
                            out.println(
                                "[handoff] tip: /handoff <note> adds what's done and what's next",
                            );
                        }
                    }
                    Err(e) => out.eprintln(&format!("[handoff] {e}")),
                }
            }
            _ => return Ok(false),
        }
        Ok(true)
    }
}
