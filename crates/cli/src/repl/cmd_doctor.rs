//! `/doctor`: check what dcode-ai depends on (sign-in, git, ripgrep, MCP
//! servers, config, disk) and offer to fix what it can.

use super::*;
use crate::tui::choice::{ChoiceAction, ChoicePrompt, DoctorFix};
use dcode_ai_common::config::{AnthropicBackend, ProviderKind};
use dcode_ai_common::event::McpStartupStatus;

/// One check's result.
struct Check {
    ok: bool,
    line: String,
    fix: Option<(String, DoctorFix)>,
}

fn ok(line: impl Into<String>) -> Check {
    Check {
        ok: true,
        line: line.into(),
        fix: None,
    }
}

fn problem(line: impl Into<String>, fix: Option<(String, DoctorFix)>) -> Check {
    Check {
        ok: false,
        line: line.into(),
        fix,
    }
}

/// First line of `<bin> --version`, if it runs.
fn version_of(bin: &str) -> Option<String> {
    let out = std::process::Command::new(bin)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status.success().then(|| {
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_string()
    })
}

/// The command that installs ripgrep here.
fn ripgrep_install() -> Option<String> {
    let on_path = dcode_ai_common::packs::on_path;
    if cfg!(windows) {
        Some("winget install BurntSushi.ripgrep.MSVC".into())
    } else if on_path("brew") {
        Some("brew install ripgrep".into())
    } else if on_path("apt-get") {
        Some("sudo apt-get install -y ripgrep".into())
    } else if on_path("dnf") {
        Some("sudo dnf install -y ripgrep".into())
    } else if on_path("pacman") {
        Some("sudo pacman -S --noconfirm ripgrep".into())
    } else {
        None
    }
}

/// Size of a folder tree in bytes.
fn dir_size(dir: &std::path::Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| match e.metadata() {
                    Ok(m) if m.is_dir() => dir_size(&e.path()),
                    Ok(m) => m.len(),
                    Err(_) => 0,
                })
                .sum()
        })
        .unwrap_or(0)
}

/// Checks that run programs or read files (off the async runtime).
fn system_checks(
    config: &dcode_ai_common::config::DcodeAiConfig,
    workspace: &std::path::Path,
) -> Vec<Check> {
    let mut out = Vec::new();
    // Sign-in for the provider in use.
    match config.provider.default {
        ProviderKind::Anthropic => {
            let a = &config.provider.anthropic;
            match a.backend {
                AnthropicBackend::ClaudeCode => {
                    if !dcode_ai_common::provider_runtime::has_claude_cli() {
                        out.push(problem(
                            "Claude: the `claude` (Claude Code) program is not installed — install it, then sign in with `claude`",
                            None,
                        ));
                    } else if dcode_ai_common::provider_runtime::claude_code_signed_in() {
                        out.push(ok("Claude: signed in through Claude Code"));
                    } else {
                        out.push(problem(
                            "Claude: Claude Code is not signed in — run `claude` in a terminal and type /login",
                            None,
                        ));
                    }
                }
                AnthropicBackend::Api => {
                    if dcode_ai_common::provider_runtime::anthropic_signed_in(a) {
                        out.push(ok("Claude: signed in to the Console (ant)"));
                    } else {
                        out.push(problem(
                            "Claude: not signed in to the Console — run /login",
                            None,
                        ));
                    }
                }
            }
        }
        ProviderKind::Copilot => {
            let store = dcode_ai_common::auth::AuthStore::load().unwrap_or_default();
            if store.copilot.is_some() || config.provider.api_key_present_for(ProviderKind::Copilot)
            {
                out.push(ok("GitHub Copilot: signed in"));
            } else {
                out.push(problem("GitHub Copilot: not signed in — run /login", None));
            }
        }
    }
    match version_of("git") {
        Some(v) => out.push(ok(v)),
        None => out.push(problem(
            "git is not installed — worktrees, diffs, /rewind of shell changes need it",
            None,
        )),
    }
    match version_of("rg") {
        Some(v) => out.push(ok(v)),
        None => out.push(problem(
            "ripgrep (rg) is not installed — code search falls back to a slow scan",
            ripgrep_install().map(|cmd| (format!("install ripgrep: {cmd}"), DoctorFix::Shell(cmd))),
        )),
    }
    // Config files that fail to parse are skipped silently otherwise.
    for path in [
        dcode_ai_common::config::global_config_file(),
        Some(dcode_ai_common::config::project_config_path(workspace)),
        Some(dcode_ai_common::config::workspace_config_path(workspace)),
    ]
    .into_iter()
    .flatten()
    .filter(|p| p.exists())
    {
        match std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|t| toml::from_str::<toml::Table>(&t).map_err(|e| e.to_string()))
        {
            Ok(_) => out.push(ok(format!("config {} reads fine", path.display()))),
            Err(e) => out.push(problem(
                format!(
                    "config {} has an error: {}",
                    path.display(),
                    e.lines().next().unwrap_or("")
                ),
                None,
            )),
        }
    }
    let sessions = workspace.join(&config.session.history_dir);
    let size = dir_size(&sessions);
    const BIG: u64 = 500 * 1024 * 1024;
    if size > BIG {
        out.push(problem(
            format!("sessions in this folder use {} MB", size / (1024 * 1024)),
            Some(("delete empty sessions".into(), DoctorFix::CleanSessions)),
        ));
    } else {
        out.push(ok(format!("sessions: {} MB", size / (1024 * 1024))));
    }
    out
}

impl Repl {
    pub(super) async fn doctor(&mut self, out: &ReplOutput<'_>) {
        let config = self.runtime.config().clone();
        let ws = self.runtime.workspace_root().to_path_buf();
        let mut checks = tokio::task::spawn_blocking(move || system_checks(&config, &ws))
            .await
            .unwrap_or_default();
        // MCP servers, from the live session.
        if let ReplOutput::Tui(st) = out
            && let Ok(g) = st.lock()
        {
            for (name, status) in &g.mcp_server_statuses {
                match status {
                    McpStartupStatus::Ready => checks.push(ok(format!(
                        "MCP `{name}`: connected · {} tools",
                        self.runtime.mcp_tool_count(name)
                    ))),
                    McpStartupStatus::Failed { message } if message.contains("sign-in") => checks
                        .push(problem(
                            format!(
                                "MCP `{name}`: needs sign-in — open /mcp and choose Authenticate"
                            ),
                            None,
                        )),
                    McpStartupStatus::Failed { message } => checks.push(problem(
                        format!(
                            "MCP `{name}` fails to start: {}",
                            message.lines().next().unwrap_or("")
                        ),
                        Some((
                            format!("turn off `{name}` in this folder"),
                            DoctorFix::DisableMcp(name.clone()),
                        )),
                    )),
                    _ => {}
                }
            }
        }
        let problems = checks.iter().filter(|c| !c.ok).count();
        let mut lines: Vec<String> = checks
            .iter()
            .map(|c| format!("{} {}", if c.ok { "✔" } else { "✗" }, c.line))
            .collect();
        lines.insert(
            0,
            if problems == 0 {
                "Everything looks good.".into()
            } else {
                format!("{problems} problem(s) found.")
            },
        );
        lines.insert(1, String::new());
        let fixes: Vec<(String, DoctorFix)> = checks.into_iter().filter_map(|c| c.fix).collect();
        match out {
            ReplOutput::Tui(st) => {
                if let Ok(mut g) = st.lock() {
                    if fixes.is_empty() {
                        g.open_info_modal("Doctor", lines);
                    } else {
                        let mut body = lines;
                        body.push(String::new());
                        body.push("dcode-ai can:".into());
                        body.extend(fixes.iter().map(|(what, _)| format!("  • {what}")));
                        let n = fixes.len();
                        g.choice_prompts.push_front(ChoicePrompt {
                            title: "Doctor".into(),
                            body,
                            options: vec![
                                format!("Fix {n} problem{}", if n == 1 { "" } else { "s" }),
                                "Leave it".into(),
                            ],
                            index: 0,
                            esc_index: 1,
                            hint: None,
                            action: ChoiceAction::DoctorFix {
                                fixes: fixes.into_iter().map(|(_, f)| f).collect(),
                            },
                        });
                        g.touch_transcript();
                    }
                }
            }
            _ => {
                for l in &lines {
                    out.println(l);
                }
                for (what, _) in &fixes {
                    out.println(&format!("fix: {what}"));
                }
            }
        }
    }

    pub(super) async fn apply_doctor_fixes(
        &mut self,
        fixes: Vec<DoctorFix>,
        st: &Arc<Mutex<TuiSessionState>>,
    ) {
        for fix in fixes {
            match fix {
                // Through the normal `!` path, so sudo gets its password popup.
                DoctorFix::Shell(cmd) => {
                    if let Some(tx) = &self.self_cmd_tx {
                        let _ = tx.send(TuiCmd::Submit(format!("! {cmd}")));
                    }
                }
                DoctorFix::DisableMcp(name) => {
                    self.runtime.disconnect_mcp_server(&name).await;
                    let msg = match dcode_ai_common::folders::set_mcp_enabled(
                        self.runtime.workspace_root(),
                        &name,
                        false,
                    ) {
                        Ok(()) => format!(
                            "[doctor] turned off MCP `{name}` in this folder (/mcp turns it back on)"
                        ),
                        Err(e) => format!("[doctor] {e}"),
                    };
                    if let Ok(mut g) = st.lock() {
                        g.mcp_server_statuses.remove(&name);
                        g.push_block(DisplayBlock::System(msg));
                        g.touch_transcript();
                    }
                }
                DoctorFix::CleanSessions => {
                    let _ =
                        Box::pin(self.handle_command("/sessions-clean", ReplOutput::Tui(st))).await;
                }
            }
        }
    }
}
