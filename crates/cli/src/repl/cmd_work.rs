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
            "/board" => {
                use dcode_ai_common::board::{Board, NewCard};
                let ws = self.runtime.workspace_root().to_path_buf();
                let (sub, arg) = rest
                    .trim()
                    .split_once(char::is_whitespace)
                    .map(|(a, b)| (a, b.trim()))
                    .unwrap_or((rest.trim(), ""));
                match sub {
                    "" | "open" => match &out {
                        ReplOutput::Tui(st) => {
                            if let Ok(mut g) = st.lock() {
                                g.board_open = true;
                            }
                        }
                        _ => out.println(&Board::load(&ws).render_text()),
                    },
                    "list" | "ls" => {
                        for l in Board::load(&ws).render_text().lines() {
                            out.println(&format!("[board] {l}"));
                        }
                    }
                    "add" if !arg.is_empty() => {
                        use dcode_ai_common::board::Space;
                        // `/board add jira <title>` makes a ticket; plain is personal.
                        let (space, title) = match arg.split_once(' ') {
                            Some((w, t)) if Space::parse(w).is_some() => {
                                (Space::parse(w).unwrap_or_default(), t.trim())
                            }
                            _ => (Space::Personal, arg),
                        };
                        let by = dcode_ai_cli::people::my_name(&ws);
                        let linked = Board::load(&ws).jira.is_some();
                        if space == Space::Jira && !linked {
                            out.eprintln("[board] link Jira first: /board jira <PROJECT-KEY>");
                            return Ok(true);
                        }
                        match Board::update(&ws, |b| {
                            b.add(
                                NewCard {
                                    title: title.to_string(),
                                    space,
                                    ..Default::default()
                                },
                                &by,
                            )
                        }) {
                            Ok(k) if space == Space::Jira => {
                                self.kick_board_sync(&out);
                                out.println(&format!("[board] added {k} — its Jira ticket is being made"))
                            }
                            Ok(k) => out.println(&format!("[board] added {k} (personal) — /board to see it")),
                            Err(e) => out.eprintln(&format!("[board] could not save: {e}")),
                        }
                    }
                    "jira" if arg == "off" || arg == "unlink" => {
                        let _ = Board::update(&ws, |b| b.jira = None);
                        if let ReplOutput::Tui(st) = &out
                            && let Ok(mut g) = st.lock()
                        {
                            g.board_syncer = None;
                        }
                        out.println("[board] unlinked from Jira — the Jira cards stay on the board, nothing syncs any more");
                    }
                    "jira" if matches!(arg.split_whitespace().next(), Some("field" | "fields")) => {
                        // `/board jira field Task Type = Build and Development`
                        // sets a value new tickets get for a field the project
                        // requires; `… field Task Type =` forgets it.
                        let rest = arg.split_once(char::is_whitespace).map(|(_, r)| r.trim()).unwrap_or("");
                        let Some(mut link) = Board::load(&ws).jira else {
                            out.eprintln("[board] link Jira first: /board jira <PROJECT-KEY>");
                            return Ok(true);
                        };
                        if rest.is_empty() {
                            if link.fields.is_empty() {
                                out.println("[board] no field values set — new tickets take the project's required fields from your latest ticket of the same type (dates from today). Set one: /board jira field <name> = <value>");
                            } else {
                                for (k, v) in &link.fields {
                                    out.println(&format!("[board] {k} = {v}"));
                                }
                            }
                            return Ok(true);
                        }
                        let Some((name, value)) = rest.split_once('=') else {
                            out.eprintln("[board] /board jira field <name> = <value> (e.g. Task Type = Build and Development)");
                            return Ok(true);
                        };
                        let (name, value) = (name.trim().trim_matches('"').to_string(), value.trim().trim_matches('"').to_string());
                        if value.is_empty() {
                            link.fields.remove(&name);
                        } else {
                            link.fields.insert(name.clone(), value.clone());
                        }
                        let saved = Board::update(&ws, |b| {
                            if let Some(l) = b.jira.as_mut() {
                                l.fields = link.fields.clone();
                            }
                        });
                        match saved {
                            Err(e) => out.eprintln(&format!("[board] could not save: {e}")),
                            Ok(()) if value.is_empty() => out.println(&format!("[board] new tickets no longer set {name}")),
                            Ok(()) => out.println(&format!("[board] new Jira tickets get {name} = {value}")),
                        }
                    }
                    "jira" if matches!(
                        arg.split_whitespace().next(),
                        Some("assignee" | "for" | "all")
                    ) => {
                        let (word, name) = arg
                            .split_once(char::is_whitespace)
                            .map(|(w, n)| (w, n.trim().trim_matches('"').trim()))
                            .unwrap_or((arg, ""));
                        if word != "all" && name.is_empty() {
                            out.eprintln("[board] whose tickets? /board jira assignee <name or email as in Jira>, or `me`");
                            return Ok(true);
                        }
                        let Some(link) = Board::load(&ws).jira else {
                            out.eprintln("[board] link Jira first: /board jira <PROJECT-KEY>");
                            return Ok(true);
                        };
                        let manager = self.runtime.mcp_manager().clone();
                        let Some(server) = self.runtime.config().mcp.servers.iter().find(|s| s.name == link.server).cloned() else {
                            out.eprintln(&format!("[board] the MCP server `{}` isn't configured any more", link.server));
                            return Ok(true);
                        };
                        let mcp = dcode_ai_cli::jira_sync::ServerMcp::new(manager, server);
                        // `me` stays `me`: the pull asks Jira for
                        // `assignee = currentUser()`, whoever is signed in.
                        let assignee = if word == "all" {
                            None
                        } else if dcode_ai_cli::jira_sync::is_me(name) {
                            Some("me".to_string())
                        } else {
                            Some(name.to_string())
                        };
                        // Tickets pulled under the old filter go; the sync
                        // below brings back the ones that still match.
                        let mut dropped = 0;
                        if let Err(e) = Board::update(&ws, |b| {
                            if let Some(l) = b.jira.as_mut() {
                                l.assignee = assignee.clone();
                            }
                            dropped = b.drop_synced();
                        }) {
                            out.eprintln(&format!("[board] could not save: {e}"));
                            return Ok(true);
                        }
                        out.println(&format!(
                            "[board] Jira {}: {} (cleared {dropped} synced tickets, re-syncing…)",
                            link.project,
                            match &assignee {
                                Some(n) => format!("only tickets assigned to {n}"),
                                None => "all the project's open and recent tickets".into(),
                            }
                        ));
                        match dcode_ai_cli::jira_sync::sync(&mcp, &ws).await {
                            Ok(r) => out.println(&format!("[board] {}", r.summary())),
                            Err(e) => out.eprintln(&format!("[board] {e}")),
                        }
                    }
                    "jira" if matches!(arg.split_whitespace().next(), Some("sprint" | "move" | "comment" | "note" | "log" | "get" | "view" | "views")) => {
                        let (word, rest) = arg
                            .split_once(char::is_whitespace)
                            .map(|(w, r)| (w, r.trim()))
                            .unwrap_or((arg, ""));
                        let Some(link) = Board::load(&ws).jira else {
                            out.eprintln("[board] link Jira first: /board jira <PROJECT-KEY>");
                            return Ok(true);
                        };
                        let Some(server) = self.runtime.config().mcp.servers.iter().find(|s| s.name == link.server).cloned() else {
                            out.eprintln(&format!("[board] the MCP server `{}` isn't configured any more", link.server));
                            return Ok(true);
                        };
                        let mcp = dcode_ai_cli::jira_sync::ServerMcp::new(self.runtime.mcp_manager().clone(), server);
                        let (key, text) = rest
                            .split_once(char::is_whitespace)
                            .map(|(k, t)| (k.trim(), t.trim()))
                            .unwrap_or((rest, ""));
                        let key = key.to_ascii_uppercase();
                        let queued: Result<String, String> = match word {
                            "sprint" => {
                                let on = !matches!(rest, "off" | "no" | "false" | "all");
                                Board::update(&ws, |b| {
                                    if let Some(l) = b.jira.as_mut() {
                                        l.sprint = on;
                                    }
                                    b.drop_synced();
                                })
                                .map(|_| {
                                    if on {
                                        format!("Jira {}: open sprints only (re-syncing…)", link.project)
                                    } else {
                                        format!("Jira {}: open and recent tickets, any sprint (re-syncing…)", link.project)
                                    }
                                })
                                .map_err(|e| format!("could not save: {e}"))
                            }
                            "view" | "views" => {
                                // `view` lists · `view add <name> <JQL>` · `view rm <name>`
                                // · `view <name>|default` switches.
                                let (sub, more) = rest
                                    .split_once(char::is_whitespace)
                                    .map(|(a, b)| (a, b.trim()))
                                    .unwrap_or((rest, ""));
                                match sub {
                                    "" => {
                                        if link.views.is_empty() {
                                            out.println("[board] no saved views — /board jira view add <name> <JQL> (e.g. mine project in (BSI, OPS) AND assignee = currentUser())");
                                        }
                                        for v in &link.views {
                                            let on = link.view.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(&v.name));
                                            out.println(&format!("[board] {} {}: {}", if on { "●" } else { " " }, v.name, v.jql));
                                        }
                                        return Ok(true);
                                    }
                                    "add" | "save" => match more.split_once(char::is_whitespace) {
                                        Some((name, jql)) if !jql.trim().is_empty() => {
                                            let (name, jql) = (name.to_string(), jql.trim().to_string());
                                            Board::update(&ws, |b| {
                                                if let Some(l) = b.jira.as_mut() {
                                                    l.views.retain(|v| !v.name.eq_ignore_ascii_case(&name));
                                                    l.views.push(dcode_ai_common::board::JiraView { name: name.clone(), jql: jql.clone() });
                                                    l.view = Some(name.clone());
                                                }
                                                b.drop_synced();
                                            })
                                            .map(|_| format!("view {name} saved and shown (re-syncing…) · v on the board switches views"))
                                            .map_err(|e| format!("could not save: {e}"))
                                        }
                                        _ => Err("/board jira view add <name> <JQL>".into()),
                                    },
                                    "rm" | "remove" | "delete" => Board::update(&ws, |b| {
                                        let l = b.jira.as_mut()?;
                                        let before = l.views.len();
                                        l.views.retain(|v| !v.name.eq_ignore_ascii_case(more));
                                        if l.view.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(more)) {
                                            l.view = None;
                                            b.drop_synced();
                                        }
                                        Some(before)
                                    })
                                    .map_err(|e| format!("could not save: {e}"))
                                    .and_then(|n| match n {
                                        Some(n) if n > Board::load(&ws).jira.map_or(0, |l| l.views.len()) => Ok(format!("view {more} removed")),
                                        _ => Err(format!("no view {more}")),
                                    }),
                                    name => {
                                        let default = matches!(name, "default" | "none" | "off");
                                        if !default && !link.views.iter().any(|v| v.name.eq_ignore_ascii_case(name)) {
                                            Err(format!("no view {name} — /board jira view lists them"))
                                        } else {
                                            Board::update(&ws, |b| {
                                                if let Some(l) = b.jira.as_mut() {
                                                    l.view = (!default).then(|| name.to_string());
                                                }
                                                b.drop_synced();
                                            })
                                            .map(|_| format!("view: {} (re-syncing…)", if default { "default" } else { name }))
                                            .map_err(|e| format!("could not save: {e}"))
                                        }
                                    }
                                }
                            }
                            _ if key.is_empty() => Err(format!(
                                "/board jira {word} <KEY> {}",
                                match word {
                                    "move" => "[transition or status name]",
                                    "log" => "<time like 2h 30m> [what you did]",
                                    "get" => "[number|all]",
                                    _ => "[internal] <text>",
                                }
                            )),
                            "log" => match dcode_ai_cli::jira_sync::parse_worklog(text) {
                                None => Err(format!("/board jira log {key} <time like 2h 30m> [what you did]")),
                                Some(w) => {
                                    let shown = format!("{} {}", w.time, w.comment).trim().to_string();
                                    Board::update(&ws, |b| {
                                        let r = b.card_mut(&key).and_then(|c| c.remote.as_mut())?;
                                        r.worklogs.push(w);
                                        Some(())
                                    })
                                    .map_err(|e| format!("could not save: {e}"))
                                    .and_then(|r| r.ok_or(format!("{key} isn't a Jira ticket on the board")))
                                    .map(|_| format!("{key}: logging {shown}…"))
                                }
                            },
                            "get" => {
                                let b = Board::load(&ws);
                                match b.card(&key).and_then(|c| c.remote.as_ref()) {
                                    None => Err(format!("{key} isn't a Jira ticket on the board")),
                                    Some(r) if r.attachments.is_empty() => Err(format!("{key} has no attachments (that the board has read — /board sync)")),
                                    Some(r) if text.is_empty() && r.attachments.len() > 1 => {
                                        for (i, a) in r.attachments.iter().enumerate() {
                                            out.println(&format!("[board] {key} {}: {}", i + 1, a.filename));
                                        }
                                        out.println(&format!("[board] /board jira get {key} <number|all>"));
                                        return Ok(true);
                                    }
                                    Some(_) => Board::update(&ws, |b| {
                                        let r = b.card_mut(&key).and_then(|c| c.remote.as_mut())?;
                                        let ids: Vec<String> = if text.is_empty() || text.eq_ignore_ascii_case("all") {
                                            r.attachments.iter().map(|a| a.id.clone()).collect()
                                        } else {
                                            text.split([',', ' '])
                                                .filter_map(|n| n.trim().parse::<usize>().ok())
                                                .filter_map(|n| r.attachments.get(n.wrapping_sub(1)))
                                                .map(|a| a.id.clone())
                                                .collect()
                                        };
                                        let n = ids.len();
                                        for id in ids {
                                            if !r.downloads.contains(&id) {
                                                r.downloads.push(id);
                                            }
                                        }
                                        Some(n)
                                    })
                                    .map_err(|e| format!("could not save: {e}"))
                                    .and_then(|n| match n {
                                        Some(0) | None => Err("no such attachment".to_string()),
                                        Some(n) => Ok(format!("{key}: downloading {n} attachment(s) to .dcode-ai/attachments/{key}/…")),
                                    }),
                                }
                            }
                            "move" if text.is_empty() => {
                                // List where it can go.
                                let b = Board::load(&ws);
                                match b.card(&key).and_then(|c| c.remote.as_ref()) {
                                    None => Err(format!("{key} isn't a Jira ticket on the board")),
                                    Some(r) if r.transitions.is_empty() => Err(format!(
                                        "{key}: its moves aren't read yet — /board sync, then try again"
                                    )),
                                    Some(r) => {
                                        for (i, t) in r.transitions.iter().enumerate() {
                                            out.println(&format!("[board] {key} {}: {}", i + 1, t.label()));
                                        }
                                        out.println(&format!("[board] /board jira move {key} <number or name>"));
                                        return Ok(true);
                                    }
                                }
                            }
                            "move" => Board::update(&ws, |b| {
                                let r = b.card_mut(&key).and_then(|c| c.remote.as_mut())?;
                                let want = match text.parse::<usize>() {
                                    Ok(n) if n >= 1 && n <= r.transitions.len() => r.transitions[n - 1].id.clone(),
                                    _ => text.to_string(),
                                };
                                r.want_transition = want;
                                Some(())
                            })
                            .map_err(|e| format!("could not save: {e}"))
                            .and_then(|r| r.ok_or(format!("{key} isn't a Jira ticket on the board")))
                            .map(|_| format!("{key}: {text} — sending to Jira…")),
                            _ => {
                                // comment / note
                                let (internal, body) = match text.split_once(char::is_whitespace) {
                                    Some(("internal", b)) => (true, b.trim()),
                                    _ => (word == "note", text),
                                };
                                if body.is_empty() {
                                    Err(format!("/board jira {word} {key} [internal] <text>"))
                                } else {
                                    Board::update(&ws, |b| {
                                        let r = b.card_mut(&key).and_then(|c| c.remote.as_mut())?;
                                        r.outbox.push(dcode_ai_common::board::OutComment {
                                            body: body.to_string(),
                                            internal,
                                        });
                                        Some(())
                                    })
                                    .map_err(|e| format!("could not save: {e}"))
                                    .and_then(|r| r.ok_or(format!("{key} isn't a Jira ticket on the board")))
                                    .map(|_| format!("{key}: {} sending…", if internal { "internal note" } else { "comment" }))
                                }
                            }
                        };
                        match queued {
                            Err(e) => out.eprintln(&format!("[board] {e}")),
                            Ok(m) => {
                                out.println(&format!("[board] {m}"));
                                match dcode_ai_cli::jira_sync::sync(&mcp, &ws).await {
                                    Ok(r) => {
                                        out.println(&format!("[board] {}", r.summary()));
                                        for p in &r.downloaded {
                                            out.println(&format!("[board] saved {}", p.display()));
                                        }
                                    }
                                    Err(e) => out.eprintln(&format!("[board] {e}")),
                                }
                            }
                        }
                    }
                    "jira" if !arg.is_empty() => {
                        let mut words = arg.split_whitespace();
                        let project = words.next().unwrap_or_default().to_string();
                        let rest: Vec<&str> = words.collect();
                        let mut site = None;
                                let mut assignee_spec: Option<String> = None;
                        let mut sprint = false;
                        let mut i = 0;
                        while i < rest.len() {
                            match rest[i] {
                                "sprint" => {
                                    sprint = true;
                                    i += 1;
                                }
                                "me" => {
                                    assignee_spec = Some("me".into());
                                    i += 1;
                                }
                                "assignee" | "for" => {
                                    assignee_spec = Some(rest.get(i + 1).copied().unwrap_or("").to_string());
                                    i += 2;
                                }
                                other => {
                                    if site.is_none() {
                                        site = Some(other.to_string());
                                    }
                                    i += 1;
                                }
                            }
                        }
                        out.println(&format!("[board] linking Jira project {project}…"));
                        let manager = self.runtime.mcp_manager().clone();
                        let servers = self.runtime.config().mcp.servers.clone();
                        let server = match dcode_ai_cli::jira_sync::find_server(&manager, &servers).await {
                            Ok(s) => s,
                            Err(e) => {
                                out.eprintln(&format!("[board] {e}"));
                                return Ok(true);
                            }
                        };
                        let mcp = dcode_ai_cli::jira_sync::ServerMcp::new(manager.clone(), server.clone());
                        match dcode_ai_cli::jira_sync::connect(&mcp, &server.name, &project, site.as_deref()).await {
                            Ok(mut link) => {
                                let assignee = match assignee_spec.as_deref() {
                                    None => None,
                                    Some("") => {
                                        out.eprintln("[board] whose tickets? /board jira <PROJECT> assignee <name>|me");
                                        return Ok(true);
                                    }
                                    Some(n) if dcode_ai_cli::jira_sync::is_me(n) => Some("me".to_string()),
                                    Some(n) => Some(n.to_string()),
                                };
                                link.assignee = assignee.clone();
                                link.sprint = sprint;
                                // Keep the field values set before (a relink
                                // to change the filter shouldn't lose them).
                                let (p, s) = (link.project.clone(), link.site.clone());
                                if let Err(e) = Board::update(&ws, |b| {
                                    if let Some(old) = &b.jira
                                        && old.project == link.project
                                    {
                                        link.fields = old.fields.clone();
                                    }
                                    // Tickets pulled under another filter go
                                    // now; the sync below brings back yours.
                                    b.drop_synced();
                                    b.jira = Some(link);
                                }) {
                                    out.eprintln(&format!("[board] could not save: {e}"));
                                    return Ok(true);
                                }
                                out.println(&format!(
                                    "[board] linked to Jira {p} ({s}){}{} through `{}` — its tickets are coming in",
                                    assignee.map(|n| format!(", only tickets assigned to {n}")).unwrap_or_default(),
                                    if sprint { ", open sprints only" } else { "" },
                                    server.name
                                ));
                                match dcode_ai_cli::jira_sync::sync(&mcp, &ws).await {
                                    Ok(r) => out.println(&format!("[board] {}", r.summary())),
                                    Err(e) => out.eprintln(&format!("[board] {e}")),
                                }
                                self.start_board_sync(&out);
                                out.println("[board] /board shows them under Jira (t switches between Personal and Jira) · /board jira assignee <name>|me keeps only one person's tickets");
                            }
                            Err(e) => out.eprintln(&format!("[board] {e}")),
                        }
                    }
                    "jira" => {
                        let b = Board::load(&ws);
                        match &b.jira {
                            Some(l) => out.println(&format!(
                                "[board] linked to Jira {} ({}){}{} through `{}` · last sync {} · /board sync · /board jira assignee <name>|me|all · /board jira sprint on|off · /board jira move <KEY> [name] · /board jira comment <KEY> [internal] <text> · /board jira field <name> = <value> · /board jira off",
                                l.project,
                                l.site,
                                l.assignee.as_deref().map(|n| format!(", only tickets assigned to {n}")).unwrap_or_default(),
                                if l.sprint { ", open sprints only" } else { "" },
                                l.server,
                                l.last_sync.map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string()).unwrap_or_else(|| "never".into())
                            )),
                            None => out.println("[board] /board jira <PROJECT-KEY> [site] [me|assignee <name>] [sprint] links the board's Jira space to a project, through Atlassian's MCP server (dcode-ai mcp add atlassian --transport http https://mcp.atlassian.com/v1/mcp, then dcode-ai mcp login atlassian)"),
                        }
                    }
                    "sync" => {
                        let b = Board::load(&ws);
                        let Some(link) = b.jira.clone() else {
                            out.eprintln("[board] not linked to Jira — /board jira <PROJECT-KEY>");
                            return Ok(true);
                        };
                        let manager = self.runtime.mcp_manager().clone();
                        let Some(server) = self.runtime.config().mcp.servers.iter().find(|s| s.name == link.server).cloned() else {
                            out.eprintln(&format!("[board] the MCP server `{}` isn't configured any more", link.server));
                            return Ok(true);
                        };
                        let mcp = dcode_ai_cli::jira_sync::ServerMcp::new(manager, server);
                        match dcode_ai_cli::jira_sync::sync(&mcp, &ws).await {
                            Ok(r) => out.println(&format!("[board] synced with Jira {}: {}", link.project, r.summary())),
                            Err(e) => out.eprintln(&format!("[board] {e}")),
                        }
                    }
                    "plan" if !arg.is_empty() => {
                        self.run_preset(
                            "Plan this work on the project's task board (the kanban the user sees with /board). First read it with `board_view` so you don't duplicate cards. Then break the goal into milestones and cards with `board_add` (send them in one call): small, testable cards with clear titles and a short description of what done means; `blocked_by` for real ordering (#n for cards in the same call); priorities; and `gate: true` on cards a person should check before they count as done (risky changes, releases, anything user-facing). Don't start the work. End with a short summary of the milestones and which cards are ready to start.

Goal:
",
                            arg,
                            out,
                        )
                        .await?
                    }
                    "work" if !arg.is_empty() => {
                        use dcode_ai_common::board::Status;
                        let mut words = arg.split_whitespace();
                        let want = words.next().unwrap_or_default();
                        let worktree = words.any(|w| matches!(w, "worktree" | "isolated" | "wt"));
                        let b = Board::load(&ws);
                        let Some(card) = b.resolve_key(want).and_then(|k| b.card(&k).cloned()) else {
                            out.eprintln(&format!("[board] no card {want} on the board"));
                            return Ok(true);
                        };
                        let key = card.shown_key().to_string();
                        let branch = dcode_ai_cli::board_work::branch_name(&card);
                        if !worktree {
                            match dcode_ai_cli::board_work::switch_branch(&ws, &branch) {
                                Ok(m) => out.println(&format!("[board] {m}")),
                                Err(e) => {
                                    out.eprintln(&format!("[board] {e}"));
                                    return Ok(true);
                                }
                            }
                        }
                        // In progress (its ticket follows), on its branch.
                        let _ = Board::update(&ws, |b| {
                            if let Some(c) = b.card_mut(&card.key) {
                                if c.status != Status::InProgress {
                                    c.set_status(Status::InProgress, "you");
                                }
                                c.branch = Some(branch.clone());
                            }
                        });
                        self.kick_board_sync(&out);
                        let b = Board::load(&ws);
                        let full = b.card(&card.key).map(|c| b.render_card(c)).unwrap_or_default();
                        let prompt = dcode_ai_cli::board_work::work_prompt(
                            &key,
                            &branch,
                            worktree,
                            card.remote.is_some(),
                        );
                        out.println(&format!("[board] working {key} on {branch}…"));
                        self.run_preset(&prompt, &full, out).await?
                    }
                    "run" => {
                        let n: usize = arg.parse().unwrap_or(3).clamp(1, 8);
                        let prompt = format!(
                            "You are the head agent for the project's task board (/board). Run it:
1. `board_view` to see the board and which cards are ready (To do, every blocker done).
2. Take up to {n} ready cards, highest priority first. For each: `board_update` it to in_progress with assignee `agent`, then start a sub-agent with `spawn_subagent` (its own worktree) with the card's key, title and description as the task, and what done means. Start them in parallel (several spawn calls in one turn).
3. When a sub-agent finishes: note what it did on the card (`board_update` with `note` and its `branch`), and move the card to review. Cards without a review gate whose work is merged or verified can go to done; a card with a gate stays in review for a person.
4. Repeat with the cards that became ready, until nothing is ready or everything left waits for a person.
Stop and tell the user plainly what waits for them: cards in review behind a gate, branches to merge, and blocked cards. Don't take cards from Backlog.

Extra instructions from the user:
"
                        );
                        let extra = if arg.is_empty() || arg.parse::<usize>().is_ok() {
                            "none"
                        } else {
                            arg
                        };
                        self.run_preset(&prompt, extra, out).await?
                    }
                    _ => out.println(
                        "[board] /board opens the board · /board add [jira] <title> · /board list · /board work <KEY> [worktree] (branch, ticket in progress, the agent works it to a PR) · /board plan <goal> (the agent lays out cards) · /board run [n] (the head agent works the ready cards, n at a time) · /board jira <PROJECT> links Jira · /board jira assignee <name>|all · /board sync",
                    ),
                }
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
            "/goal" => {
                use dcode_ai_runtime::goal::{self as goal, Goal, GoalStatus};
                let cell = self.runtime.goal_cell();
                let arg = rest.trim();
                // `--budget <tokens>` anywhere in the arguments.
                let mut budget: Option<u64> = None;
                let mut words: Vec<&str> = Vec::new();
                let mut it = arg.split_whitespace();
                while let Some(w) = it.next() {
                    if w == "--budget" {
                        budget = it.next().and_then(|b| b.replace(['_', ','], "").parse().ok());
                    } else {
                        words.push(w);
                    }
                }
                let text = words.join(" ");
                let say = |s: String| {
                    if let ReplOutput::Tui(st) = &out
                        && let Ok(mut g) = st.lock()
                    {
                        g.push_block(DisplayBlock::System(s));
                        g.goal_label = goal_footer_label(&cell);
                        g.touch_transcript();
                    } else {
                        out.println(&s);
                    }
                };
                let tokens_now = self.runtime.tokens_used();
                match text.as_str() {
                    "" => {
                        let line = match cell.lock().ok().and_then(|g| g.clone()) {
                            Some(g) => format!(
                                "◎ goal ({}): {}\n  turns {} · tokens {}{}\n  /goal pause · resume · clear",
                                g.status.label(),
                                g.objective,
                                g.turns,
                                tokens_now.saturating_sub(g.tokens_at_start),
                                g.budget.map(|b| format!(" of {b}")).unwrap_or_default()
                            ),
                            None => "No goal. /goal <objective> [--budget <tokens>] keeps dcode-ai working across turns until it is done.".into(),
                        };
                        say(line);
                        return Ok(true);
                    }
                    "clear" => {
                        *cell.lock().unwrap_or_else(|e| e.into_inner()) = None;
                        say("◎ goal cleared".into());
                        return Ok(true);
                    }
                    "pause" => {
                        let line = goal::pause_active(&cell, "by you")
                            .unwrap_or_else(|| "No active goal to pause.".into());
                        say(line);
                        return Ok(true);
                    }
                    "resume" => {
                        let resumed = {
                            let mut guard = cell.lock().unwrap_or_else(|e| e.into_inner());
                            match guard.as_mut() {
                                Some(g) if g.status != GoalStatus::Active && g.status != GoalStatus::Complete => {
                                    g.status = GoalStatus::Active;
                                    g.announced = false;
                                    g.turns = 0;
                                    if budget.is_some() {
                                        g.budget = budget;
                                        g.tokens_at_start = tokens_now;
                                    }
                                    true
                                }
                                _ => false,
                            }
                        };
                        if !resumed {
                            say("No paused, blocked or budget-limited goal to resume.".into());
                            return Ok(true);
                        }
                        say("◎ goal resumed".into());
                    }
                    objective => {
                        if cell
                            .lock()
                            .ok()
                            .and_then(|g| g.as_ref().map(|g| g.status == GoalStatus::Active))
                            .unwrap_or(false)
                        {
                            say("A goal is already active — /goal clear first, or /goal to see it.".into());
                            return Ok(true);
                        }
                        let new = Goal::new(objective, budget, tokens_now);
                        let prompt = goal::start_prompt(&new);
                        *cell.lock().unwrap_or_else(|e| e.into_inner()) = Some(new);
                        say(format!(
                            "◎ goal set{}: {objective}",
                            budget.map(|b| format!(" (budget {b} tokens)")).unwrap_or_default()
                        ));
                        // The TUI's idle loop continues the goal after each
                        // turn; the plain prompt runs the loop here.
                        if let Some(tx) = &self.self_cmd_tx {
                            if let ReplOutput::Tui(st) = &out
                                && let Ok(mut g) = st.lock()
                            {
                                g.suppress_next_user_block = true;
                            }
                            let _ = tx.send(TuiCmd::Submit(prompt));
                            return Ok(true);
                        }
                        let mut prompt = prompt;
                        loop {
                            match self.runtime.run_turn(&prompt).await {
                                Ok(o) => println!("{o}"),
                                Err(e) => {
                                    eprintln!("error: {e}");
                                    if let Some(n) = goal::pause_active(&cell, "the turn failed") {
                                        println!("{n}");
                                    }
                                    break;
                                }
                            }
                            let (next, note) = goal::next_step(&cell, self.runtime.tokens_used());
                            if let Some(n) = note {
                                println!("{n}");
                            }
                            match next {
                                Some(p) => prompt = p,
                                None => break,
                            }
                        }
                        return Ok(true);
                    }
                }
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

impl Repl {
    /// A syncer for the board's Jira link, when it's linked and its MCP
    /// server is configured.
    pub(super) fn board_syncer(&self) -> Option<std::sync::Arc<dcode_ai_cli::jira_sync::Syncer>> {
        let ws = self.runtime.workspace_root().to_path_buf();
        let link = dcode_ai_common::board::Board::load(&ws).jira?;
        let server = self
            .runtime
            .config()
            .mcp
            .servers
            .iter()
            .find(|s| s.name == link.server && s.enabled)
            .cloned()?;
        let mcp =
            dcode_ai_cli::jira_sync::ServerMcp::new(self.runtime.mcp_manager().clone(), server);
        Some(dcode_ai_cli::jira_sync::Syncer::spawn(
            std::sync::Arc::new(mcp),
            ws,
        ))
    }

    /// Starts (or restarts) the background Jira sync for the TUI.
    pub(super) fn start_board_sync(&self, out: &ReplOutput<'_>) {
        if let ReplOutput::Tui(st) = out
            && let Ok(mut g) = st.lock()
        {
            g.board_syncer = self.board_syncer();
        }
    }

    /// Sync now (a Jira card was made or moved).
    pub(super) fn kick_board_sync(&self, out: &ReplOutput<'_>) {
        if let ReplOutput::Tui(st) = out
            && let Ok(g) = st.lock()
            && let Some(s) = &g.board_syncer
        {
            s.kick();
        }
    }
}
