//! Work commands: review, verify, security review, PR comments, plan, run, web, commit.

use super::*;

impl Repl {
    #[allow(unused_variables, unused_mut)]
    pub(super) async fn cmd_work(
        &mut self,
        input: &str,
        command: &str,
        rest: &str,
        mut parts: std::str::SplitWhitespace<'_>,
        out: ReplOutput<'_>,
    ) -> anyhow::Result<bool> {
        match command {
            "/verify" => {
                self.run_verifier(rest.trim(), &out).await?;
            }
            "/second-opinion" => {
                self.run_review(rest.trim(), &out, ReviewKind::SecondOpinion)
                    .await?;
            }
            "/security-review" => {
                self.run_review(rest.trim(), &out, ReviewKind::Security)
                    .await?;
            }
            "/plan" if rest.trim().is_empty() => {
                self.set_mode_and_profile(PermissionMode::Plan);
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.set_agent_profile(&self.current_agent_label);
                    g.set_permission_mode("Plan");
                }
                out.println("Plan mode on — dcode-ai will plan without editing (shift+tab to leave)");
            }
            "/plan" => {
                self.set_mode_and_profile(PermissionMode::Plan);
                if let ReplOutput::Tui(st) = &out
                    && let Ok(mut g) = st.lock()
                {
                    g.set_agent_profile(&self.current_agent_label);
                    g.set_permission_mode("Plan");
                }
                self.run_preset(
                    "Create a step-by-step implementation plan before coding. Call the `update_plan` \
tool with the ordered steps (status pending/in_progress/completed) and keep it updated as you \
work. Focus on concrete steps, risks, and validation.\n\nTask:\n",
                    rest,
                    out,
                )
                .await?
            }
            "/pr-comments" => {
                let ws = self.runtime.workspace_root().to_path_buf();
                let pr = rest.trim().trim_start_matches('#').to_string();
                out.println("[pr-comments] fetching comments with gh…");
                let fetched = tokio::task::spawn_blocking(move || pr_comments(&ws, &pr))
                    .await
                    .unwrap_or_else(|e| Err(e.to_string()));
                match fetched {
                    Ok(text) => {
                        self.run_preset(
                            "Here are the comments on this pull request. Show them grouped by \
file (with the line or code each refers to), then list the changes reviewers ask for, \
most important first, and say which ones you think are already addressed. Do not edit \
anything until I ask.\n\n",
                            &text,
                            out,
                        )
                        .await?
                    }
                    Err(e) => out.eprintln(&format!("[pr-comments] {e}")),
                }
            }
            "/review" => {
                self.run_preset(
                    "Review the requested code or changes. Prioritize bugs, regressions, risks, and missing tests.\n\nReview target:\n",
                    rest,
                    out,
                )
                .await?
            }
            "/fix" => {
                self.run_preset(
                    "Diagnose and fix the issue below. Prefer a minimal verified change.\n\nIssue:\n",
                    rest,
                    out,
                )
                .await?
            }
            "/test" => {
                self.run_preset(
                    "Validate the requested area. Run tests or checks if tools allow, and report what passed or failed.\n\nTarget:\n",
                    rest,
                    out,
                )
                .await?
            }
            "/image" => {
                let st = match &out {
                    ReplOutput::Tui(st) => st,
                    ReplOutput::Stdio => {
                        out.eprintln(
                            "[image] stage images from the full-screen TUI (Ctrl+V, /image paste, /image <path>)",
                        );
                        return Ok(true);
                    }
                };
                let workspace = self.runtime.workspace_root().to_path_buf();
                let sid = self.runtime.session_id().to_string();
                let rest_trim = rest.trim();
                if rest_trim.is_empty() || rest_trim.eq_ignore_ascii_case("paste") {
                    match crate::image_attach::paste_clipboard_image(&workspace, &sid) {
                        Ok(att) => {
                            if let Ok(mut g) = st.lock() {
                                g.attach_image_inline(att);
                            }
                        }
                        Err(e) => out.eprintln(&format!("[image] {e}")),
                    }
                } else if rest_trim.eq_ignore_ascii_case("clear") {
                    if let Ok(mut g) = st.lock() {
                        g.staged_image_attachments.clear();
                    }
                    out.println("[image] cleared staged images");
                } else {
                    let path_text = rest_trim
                        .trim()
                        .trim_matches(|c| matches!(c, '\'' | '"' | '`'));
                    let p = std::path::Path::new(path_text);
                    match crate::image_attach::import_image_file(&workspace, &sid, p) {
                        Ok(att) => {
                            if let Ok(mut g) = st.lock() {
                                g.attach_image_inline(att);
                            }
                        }
                        Err(e) => out.eprintln(&format!("[image] {e}")),
                    }
                }
            }
            // ── /run <shell cmd> ──────────────────────────────────────────────
            "/run" => {
                if rest.trim().is_empty() {
                    out.eprintln("[run] usage: /run <shell command>");
                    return Ok(true);
                }
                let shell_cmd = rest.trim().to_string();
                out.println(&format!("[run] $ {shell_cmd}"));
                let result = Command::new(if cfg!(windows) { "cmd" } else { "sh" })
                    .args(if cfg!(windows) {
                        vec!["/C", &shell_cmd]
                    } else {
                        vec!["-c", &shell_cmd]
                    })
                    .current_dir(self.runtime.workspace_root())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .output()
                    .await;
                match result {
                    Ok(cmd_out) => {
                        let stdout = String::from_utf8_lossy(&cmd_out.stdout).to_string();
                        let stderr = String::from_utf8_lossy(&cmd_out.stderr).to_string();
                        let combined = if stderr.is_empty() {
                            stdout
                        } else if stdout.is_empty() {
                            stderr
                        } else {
                            format!("{stdout}{stderr}")
                        };
                        const MAX_SHOW: usize = 8_000;
                        let truncated = if combined.len() > MAX_SHOW {
                            let mut end = MAX_SHOW;
                            while end > 0 && !combined.is_char_boundary(end) {
                                end -= 1;
                            }
                            format!("{}…\n[output truncated]", &combined[..end])
                        } else {
                            combined.clone()
                        };
                        if truncated.is_empty() {
                            out.println("[run] (no output)");
                        } else {
                            out.print(&truncated);
                        }
                        let exit_label = if cmd_out.status.success() {
                            "[exit 0]".to_string()
                        } else {
                            format!("[exit {}]", cmd_out.status.code().unwrap_or(-1))
                        };
                        out.println(&format!("[run] {exit_label} — output staged (send a message to use it)"));
                        // Stage the output so the user's next message carries it.
                        if let ReplOutput::Tui(st) = &out
                            && let Ok(mut g) = st.lock()
                        {
                            g.pending_context.push(format!(
                                "Output of `{shell_cmd}` ({exit_label}):\n```\n{truncated}\n```"
                            ));
                        }
                    }
                    Err(e) => out.eprintln(&format!("[run] error: {e}")),
                }
            }

            // ── /web <url> ────────────────────────────────────────────────────
            "/web" => {
                let arg = rest.trim();
                if arg.is_empty() {
                    out.eprintln("[web] usage: /web <url | search query>");
                    return Ok(true);
                }
                // Treat the argument as a URL when it has a scheme, or looks like
                // a bare domain (no spaces, has a dot). Otherwise it's a search
                // query → fetch DuckDuckGo results and stage them as context.
                let is_url = arg.starts_with("http://")
                    || arg.starts_with("https://")
                    || (!arg.contains(char::is_whitespace)
                        && arg.contains('.')
                        && !arg.ends_with('.'));

                let (source_url, fetch_url, label) = if is_url {
                    let url = if arg.starts_with("http") {
                        arg.to_string()
                    } else {
                        format!("https://{arg}")
                    };
                    out.println(&format!("[web] Fetching {url}…"));
                    (url.clone(), url, format!("Content fetched from <{arg}>"))
                } else {
                    out.println(&format!("[web] Searching the web for: {arg}…"));
                    let search_url = match reqwest::Url::parse_with_params(
                        "https://html.duckduckgo.com/html/",
                        &[("q", arg)],
                    ) {
                        Ok(u) => u.to_string(),
                        Err(_) => {
                            out.eprintln("[web] could not build search URL");
                            return Ok(true);
                        }
                    };
                    (
                        arg.to_string(),
                        search_url,
                        format!("Web search results for \"{arg}\""),
                    )
                };

                match fetch_url_as_text(&fetch_url).await {
                    Ok(text) => {
                        let preview: String = text.lines().take(3).collect::<Vec<_>>().join(" ");
                        let preview = truncate_str_bytes(&preview, 80);
                        out.println(&format!("[web] {} lines: {preview}…", text.lines().count()));
                        out.println("[web] Staged — send a message to use this context");
                        let block = format!("{label}:\n```text\n{text}\n```");
                        if let ReplOutput::Tui(st) = &out {
                            if let Ok(mut g) = st.lock() {
                                g.pending_context.push(block);
                            }
                        } else {
                            // In stdio mode: run a turn that uses the fetched text.
                            let prompt = format!(
                                "{label} <{source_url}>:\n\n```\n{text}\n```\n\nSummarize the most relevant findings."
                            );
                            let _ = self.runtime.run_turn(&prompt).await;
                        }
                    }
                    Err(e) => out.eprintln(&format!("[web] failed: {e}")),
                }
            }

            // ── /commit ───────────────────────────────────────────────────────
            "/commit" => {
                // Check for staged changes.
                let diff_out = Command::new("git")
                    .args(["diff", "--staged"])
                    .current_dir(self.runtime.workspace_root())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .output()
                    .await;
                let diff = match diff_out {
                    Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
                    Err(e) => {
                        out.eprintln(&format!("[commit] git error: {e}"));
                        return Ok(true);
                    }
                };
                if diff.trim().is_empty() {
                    out.eprintln("[commit] No staged changes — run `git add` first");
                    return Ok(true);
                }
                out.println("[commit] Generating commit message…");
                let prompt = format!(
                    "Write a git commit message for the following staged diff.\n\
                     Use Conventional Commits format (feat/fix/chore/docs/…).\n\
                     Output ONLY the commit message text — no code fences, no extra commentary.\n\
                     Keep the subject line ≤72 chars.\n\
                     \n\
                     ```diff\n{diff}\n```"
                );
                let msg = match self.runtime.run_turn(&prompt).await {
                    Ok(m) => m,
                    Err(e) => {
                        out.eprintln(&format!("[commit] model error: {e}"));
                        return Ok(true);
                    }
                };
                // Strip fences if model wrapped the message anyway.
                let msg = msg
                    .trim()
                    .trim_start_matches("```")
                    .trim_end_matches("```")
                    .trim()
                    .to_string();
                if msg.is_empty() {
                    out.eprintln("[commit] Model returned an empty message — aborting");
                    return Ok(true);
                }
                let commit_out = Command::new("git")
                    .args(["commit", "-m", &msg])
                    .current_dir(self.runtime.workspace_root())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::piped())
                    .output()
                    .await;
                match commit_out {
                    Ok(o) if o.status.success() => {
                        let first_line = msg.lines().next().unwrap_or(&msg);
                        out.println(&format!("[commit] ✓ {first_line}"));
                    }
                    Ok(o) => {
                        let stderr = String::from_utf8_lossy(&o.stderr);
                        out.eprintln(&format!("[commit] git commit failed: {stderr}"));
                    }
                    Err(e) => out.eprintln(&format!("[commit] error: {e}")),
                }
            }

            _ => {}
        }
        Ok(true)
    }
}

/// The open PR's conversation and inline review comments, as text for the
/// model. `pr` is a number, or empty for the current branch's PR.
fn pr_comments(ws: &std::path::Path, pr: &str) -> Result<String, String> {
    let gh = |args: &[&str]| -> Result<String, String> {
        let o = std::process::Command::new("gh")
            .args(args)
            .current_dir(ws)
            .output()
            .map_err(|e| {
                format!("gh not found ({e}); install GitHub CLI and run `gh auth login`")
            })?;
        if !o.status.success() {
            return Err(String::from_utf8_lossy(&o.stderr).trim().to_string());
        }
        Ok(String::from_utf8_lossy(&o.stdout).to_string())
    };
    let mut view = vec!["pr", "view"];
    if !pr.is_empty() {
        view.push(pr);
    }
    view.extend(["--json", "number,title,url,body,comments,reviews"]);
    let info: serde_json::Value =
        serde_json::from_str(&gh(&view)?).map_err(|e| format!("unexpected gh output: {e}"))?;
    let number = info["number"].as_u64().ok_or("no pull request found")?;
    let repo = gh(&[
        "repo",
        "view",
        "--json",
        "nameWithOwner",
        "-q",
        ".nameWithOwner",
    ])?;
    let inline: serde_json::Value = serde_json::from_str(&gh(&[
        "api",
        "--paginate",
        &format!("repos/{}/pulls/{number}/comments", repo.trim()),
    ])?)
    .unwrap_or_default();

    let mut out = format!(
        "PR #{number}: {}\n{}\n",
        info["title"].as_str().unwrap_or(""),
        info["url"].as_str().unwrap_or("")
    );
    let author = |v: &serde_json::Value| v["author"]["login"].as_str().unwrap_or("?").to_string();
    for r in info["reviews"].as_array().into_iter().flatten() {
        let body = r["body"].as_str().unwrap_or("").trim();
        if !body.is_empty() {
            out.push_str(&format!(
                "\n[review by @{} — {}]\n{body}\n",
                author(r),
                r["state"].as_str().unwrap_or("")
            ));
        }
    }
    for c in info["comments"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "\n[comment by @{}]\n{}\n",
            author(c),
            c["body"].as_str().unwrap_or("").trim()
        ));
    }
    let mut n_inline = 0;
    for c in inline.as_array().into_iter().flatten() {
        n_inline += 1;
        let line = c["line"].as_u64().or_else(|| c["original_line"].as_u64());
        out.push_str(&format!(
            "\n[inline by @{} on {}{}]\n```diff\n{}\n```\n{}\n",
            c["user"]["login"].as_str().unwrap_or("?"),
            c["path"].as_str().unwrap_or("?"),
            line.map(|l| format!(":{l}")).unwrap_or_default(),
            c["diff_hunk"]
                .as_str()
                .unwrap_or("")
                .lines()
                .rev()
                .take(6)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n"),
            c["body"].as_str().unwrap_or("").trim()
        ));
    }
    if n_inline == 0
        && info["comments"].as_array().is_none_or(|a| a.is_empty())
        && info["reviews"].as_array().is_none_or(|a| a.is_empty())
    {
        return Err(format!("PR #{number} has no comments yet"));
    }
    Ok(out)
}
