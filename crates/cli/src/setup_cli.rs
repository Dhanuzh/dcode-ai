//! `dcode-ai setup` and `/setup`: install role and language starter packs.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use clap::Args;
use dcode_ai_common::mcp_json::{self, McpScope, NewServer};
use dcode_ai_common::packs::{self, Pack, PackKind, PackRecord, SetupState, TeamFile};
use dcode_ai_common::plugins::{Registry, plugins_dir};
use dcode_ai_core::plugin_installer;

#[derive(Debug, Args)]
pub struct SetupArgs {
    /// Pack ids to install (`dcode-ai setup --list` shows them)
    pub packs: Vec<String>,
    /// List the packs and what each adds
    #[arg(long)]
    pub list: bool,
    /// Show the languages found in this repo
    #[arg(long)]
    pub detect: bool,
    /// Undo a pack: remove what setup installed for it
    #[arg(long, value_name = "PACK")]
    pub remove: Option<String>,
    /// Write the installed pack ids to a team file
    #[arg(long, value_name = "FILE")]
    pub save: Option<PathBuf>,
    /// Install the packs listed in a team file
    #[arg(long, value_name = "FILE")]
    pub from: Option<PathBuf>,
    /// Where MCP servers go: project (.mcp.json) or user
    #[arg(long, default_value = "project")]
    pub scope: String,
    /// Don't ask; accept every MCP server the packs offer
    #[arg(long, short = 'y')]
    pub yes: bool,
    /// Show what would be installed without changing anything
    #[arg(long)]
    pub dry_run: bool,
}

/// Lines describing every pack, with detected languages and installed packs
/// marked.
pub fn list_lines(workspace_root: &Path) -> Vec<String> {
    let (all, errors) = packs::load(workspace_root);
    let detected: Vec<String> = packs::detect(&all, workspace_root)
        .iter()
        .map(|p| p.id.clone())
        .collect();
    let state = SetupState::load();
    let mut lines = Vec::new();
    for (kind, title) in [(PackKind::Role, "Roles"), (PackKind::Language, "Languages")] {
        lines.push(title.to_string());
        for p in all.iter().filter(|p| p.kind == kind) {
            let mark = if state.packs.contains_key(&p.id) {
                "●"
            } else {
                "○"
            };
            let found = if detected.contains(&p.id) {
                "  (found in this repo)"
            } else {
                ""
            };
            lines.push(format!("  {mark} {:<11} {}{found}", p.id, p.name));
            lines.push(format!("      {}", p.description));
            let names: Vec<&str> = p.plugins.iter().map(|r| packs::plugin_name(r)).collect();
            lines.push(format!("      plugins: {}", names.join(", ")));
            if !p.mcp.is_empty() {
                let names: Vec<&str> = p.mcp.iter().map(|m| m.name.as_str()).collect();
                lines.push(format!("      mcp: {}", names.join(", ")));
            }
        }
        lines.push(String::new());
    }
    lines
        .push("Install: dcode-ai setup <id>… (or /setup <id>… in a session). ● = installed".into());
    for e in errors {
        lines.push(format!("! {e}"));
    }
    lines
}

/// Resolve ids against the known packs; unknown ids are an error.
fn pick<'a>(all: &'a [Pack], ids: &[String]) -> Result<Vec<&'a Pack>, String> {
    let mut out: Vec<&Pack> = Vec::new();
    for id in ids {
        let id = id.trim().to_ascii_lowercase();
        let p = all
            .iter()
            .find(|p| p.id == id)
            .ok_or_else(|| format!("unknown pack `{id}` (dcode-ai setup --list)"))?;
        if !out.iter().any(|q| q.id == p.id) {
            out.push(p);
        }
    }
    Ok(out)
}

/// Install `chosen` packs. `ask` decides each MCP server; `say` reports.
pub fn install(
    workspace_root: &Path,
    chosen: &[&Pack],
    scope: McpScope,
    dry_run: bool,
    ask: &mut dyn FnMut(&str) -> bool,
    say: &mut dyn FnMut(String),
) -> Result<(), String> {
    let pdir = plugins_dir().ok_or("no home directory")?;
    let mdir = plugin_installer::marketplaces_dir().ok_or("no home directory")?;
    let mut state = SetupState::load();
    let mut missing_tools: Vec<(String, String)> = Vec::new();

    for pack in chosen {
        say(format!("⏺ {} ({})", pack.name, pack.id));
        let mut record = state.packs.get(&pack.id).cloned().unwrap_or_default();
        for reference in &pack.plugins {
            let name = packs::plugin_name(reference);
            let market = reference.split_once('@').map(|(_, m)| m);
            let installed = Registry::load_from(&pdir).get(name).is_some();
            if installed {
                // Shared with another pack: record it here too so removing
                // either pack keeps it for the other.
                let by_setup = state
                    .packs
                    .values()
                    .any(|r| r.plugins.iter().any(|p| p == name));
                if by_setup && !record.plugins.iter().any(|p| p == name) {
                    record.plugins.push(name.to_string());
                }
                say(format!("  ⎿ {name}: already installed"));
                continue;
            }
            if dry_run {
                say(format!("  ⎿ {name}: would install ({reference})"));
                continue;
            }
            if let Some(m) = market
                && plugin_installer::KnownMarkets::load_from(&mdir)
                    .get(m)
                    .is_none()
            {
                if m == "claude-plugins-official" {
                    say("  ⎿ adding the official marketplace…".into());
                    if let Err(e) =
                        plugin_installer::add_market(&mdir, plugin_installer::DEFAULT_MARKETPLACE)
                    {
                        say(format!("  ⎿ {name}: marketplace failed: {e}"));
                        continue;
                    }
                } else {
                    say(format!(
                        "  ⎿ {name}: unknown marketplace `{m}` (dcode-ai plugin marketplace add …)"
                    ));
                    continue;
                }
            }
            match plugin_installer::install(&pdir, reference, None) {
                Ok(p) => {
                    say(format!("  ⎿ {}: installed", p.name));
                    record.plugins.push(p.name);
                }
                Err(e) => say(format!("  ⎿ {name}: {e}")),
            }
        }
        for m in &pack.mcp {
            let exists = mcp_json::load_scope(scope, workspace_root)
                .iter()
                .any(|s| s.name == m.name);
            if exists {
                say(format!("  ⎿ mcp {}: already configured", m.name));
                continue;
            }
            let target = m.url.clone().unwrap_or_else(|| {
                format!(
                    "{} {}",
                    m.command.clone().unwrap_or_default(),
                    m.args.join(" ")
                )
            });
            if dry_run {
                say(format!("  ⎿ mcp {}: would add ({})", m.name, target.trim()));
                continue;
            }
            let note = m
                .note
                .as_deref()
                .map(|n| format!(" — {n}"))
                .unwrap_or_default();
            if !ask(&format!(
                "Add MCP server `{}` ({}){note}?",
                m.name,
                target.trim()
            )) {
                say(format!("  ⎿ mcp {}: skipped", m.name));
                continue;
            }
            let server = NewServer {
                command: m.command.clone(),
                args: m.args.clone(),
                url: m.url.clone(),
                ..Default::default()
            };
            match mcp_json::add_server(scope, workspace_root, &m.name, &server) {
                Ok(path) => {
                    say(format!("  ⎿ mcp {}: added to {}", m.name, path.display()));
                    record.mcp.push((scope.label().to_string(), m.name.clone()));
                }
                Err(e) => say(format!("  ⎿ mcp {}: {e}", m.name)),
            }
        }
        for t in &pack.tools {
            if !packs::on_path(&t.command) && !missing_tools.iter().any(|(c, _)| *c == t.command) {
                missing_tools.push((t.command.clone(), t.install.clone()));
            }
        }
        if let Some(n) = &pack.notes {
            say(format!("  ⎿ {n}"));
        }
        if !dry_run {
            state.packs.insert(pack.id.clone(), record);
            state.save()?;
        }
    }
    if !missing_tools.is_empty() {
        say(String::new());
        say("Not on PATH yet (install yourself):".into());
        for (cmd, how) in &missing_tools {
            say(format!("  {cmd:<28} {how}"));
        }
    }
    Ok(())
}

/// Remove what setup installed for `id`, keeping plugins another pack uses.
pub fn remove(workspace_root: &Path, id: &str, say: &mut dyn FnMut(String)) -> Result<(), String> {
    let mut state = SetupState::load();
    let record: PackRecord = state
        .packs
        .get(id)
        .cloned()
        .ok_or_else(|| format!("pack `{id}` is not installed"))?;
    let pdir = plugins_dir().ok_or("no home directory")?;
    for name in &record.plugins {
        if state.plugin_used_elsewhere(name, id) {
            say(format!("  ⎿ {name}: kept (another pack uses it)"));
            continue;
        }
        match plugin_installer::remove(&pdir, name) {
            Ok(()) => say(format!("  ⎿ {name}: removed")),
            Err(e) => say(format!("  ⎿ {name}: {e}")),
        }
    }
    for (scope, name) in &record.mcp {
        if let Some(s) = McpScope::parse(scope) {
            match mcp_json::remove_server(s, workspace_root, name) {
                Ok(true) => say(format!("  ⎿ mcp {name}: removed")),
                Ok(false) => {}
                Err(e) => say(format!("  ⎿ mcp {name}: {e}")),
            }
        }
    }
    state.packs.remove(id);
    state.save()?;
    say(format!("Removed pack `{id}`."));
    Ok(())
}

fn read_line(prompt: &str) -> String {
    print!("{prompt}");
    let _ = io::stdout().flush();
    let mut line = String::new();
    let _ = io::stdin().lock().read_line(&mut line);
    line.trim().to_string()
}

fn yes_no(prompt: &str, default_yes: bool) -> bool {
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    match read_line(&format!("{prompt} {hint} "))
        .to_ascii_lowercase()
        .as_str()
    {
        "" => default_yes,
        a => a.starts_with('y'),
    }
}

/// Numbered choice: returns the ids picked (`1 3`, `1,3`, ids, or Enter for
/// the preselected ones).
fn choose(title: &str, options: &[&Pack], preselected: &[String]) -> Vec<String> {
    println!("\n{title}");
    for (i, p) in options.iter().enumerate() {
        let mark = if preselected.contains(&p.id) {
            "◉"
        } else {
            "○"
        };
        println!("  {mark} {:>2}. {:<24} {}", i + 1, p.name, p.description);
    }
    let def = if preselected.is_empty() {
        "none".to_string()
    } else {
        preselected.join(" ")
    };
    let answer = read_line(&format!(
        "Numbers or ids, space-separated (Enter = {def}): "
    ));
    if answer.is_empty() {
        return preselected.to_vec();
    }
    answer
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .filter_map(|tok| match tok.parse::<usize>() {
            Ok(n) => options.get(n.wrapping_sub(1)).map(|p| p.id.clone()),
            Err(_) => options
                .iter()
                .find(|p| p.id.eq_ignore_ascii_case(tok))
                .map(|p| p.id.clone()),
        })
        .collect()
}

fn wizard(workspace_root: &Path, all: &[Pack]) -> Option<(Vec<String>, McpScope)> {
    println!("dcode-ai starter packs — plugins for what you do. Optional; Ctrl+C to skip.");
    let roles: Vec<&Pack> = all.iter().filter(|p| p.kind == PackKind::Role).collect();
    let langs: Vec<&Pack> = all
        .iter()
        .filter(|p| p.kind == PackKind::Language)
        .collect();
    let detected: Vec<String> = packs::detect(all, workspace_root)
        .iter()
        .map(|p| p.id.clone())
        .collect();
    let mut ids = choose("What do you work on?", &roles, &[]);
    if !detected.is_empty() {
        println!("\nFound in this repo: {}", detected.join(", "));
    }
    ids.extend(choose("Which languages?", &langs, &detected));
    if ids.is_empty() {
        println!("Nothing selected.");
        return None;
    }
    let has_mcp = all.iter().any(|p| ids.contains(&p.id) && !p.mcp.is_empty());
    let scope = if has_mcp
        && !yes_no(
            "MCP servers for this project only (no = all projects)?",
            true,
        ) {
        McpScope::User
    } else {
        McpScope::Project
    };
    println!("\nWill set up: {}", ids.join(", "));
    yes_no("Continue?", true).then_some((ids, scope))
}

pub fn run(workspace_root: &Path, args: SetupArgs) -> anyhow::Result<()> {
    let workspace_root = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    let say = &mut |l: String| println!("{l}");

    if args.list {
        for l in list_lines(&workspace_root) {
            println!("{l}");
        }
        return Ok(());
    }
    let (mut all, errors) = packs::load(&workspace_root);
    for e in &errors {
        eprintln!("! {e}");
    }
    if args.detect {
        let found = packs::detect(&all, &workspace_root);
        if found.is_empty() {
            println!("No known languages found in {}", workspace_root.display());
        }
        for p in found {
            println!("{:<11} {}", p.id, p.name);
        }
        return Ok(());
    }
    if let Some(id) = &args.remove {
        return remove(&workspace_root, id, say).map_err(anyhow::Error::msg);
    }
    if let Some(path) = &args.save {
        let state = SetupState::load();
        if state.packs.is_empty() {
            anyhow::bail!("no packs installed yet");
        }
        // Custom packs travel with the file so teammates get them too.
        let builtin: Vec<String> = packs::builtin().into_iter().map(|p| p.id).collect();
        let defs = all
            .iter()
            .filter(|p| state.packs.contains_key(&p.id) && !builtin.contains(&p.id))
            .cloned()
            .collect();
        let team = TeamFile {
            packs: state.packs.keys().cloned().collect(),
            defs,
        };
        team.write(path).map_err(anyhow::Error::msg)?;
        println!("Wrote {} ({})", path.display(), team.packs.join(", "));
        return Ok(());
    }

    let scope = McpScope::parse(&args.scope)
        .filter(|s| *s != McpScope::Local)
        .ok_or_else(|| anyhow::anyhow!("--scope must be project or user"))?;
    let (ids, scope) = if let Some(path) = &args.from {
        let team = TeamFile::read(path).map_err(anyhow::Error::msg)?;
        for d in team.defs {
            match all.iter_mut().find(|p| p.id == d.id) {
                Some(slot) => *slot = d,
                None => all.push(d),
            }
        }
        (team.packs, scope)
    } else if !args.packs.is_empty() {
        (args.packs.clone(), scope)
    } else {
        match wizard(&workspace_root, &all) {
            Some(v) => v,
            None => return Ok(()),
        }
    };
    let chosen = pick(&all, &ids).map_err(anyhow::Error::msg)?;
    let yes = args.yes;
    let ask = &mut |q: &str| yes || yes_no(q, false);
    install(&workspace_root, &chosen, scope, args.dry_run, ask, say).map_err(anyhow::Error::msg)?;
    if !args.dry_run {
        println!(
            "\nDone. New sessions load the plugins; `dcode-ai setup --list` shows what's installed."
        );
    }
    Ok(())
}

/// `/setup [ids…] [--remove id]` inside a session: returns the lines to show.
/// MCP servers are only added with `--yes` (there is no prompt here).
pub fn slash(workspace_root: &Path, rest: &str) -> Vec<String> {
    let mut words: Vec<String> = rest.split_whitespace().map(String::from).collect();
    let mut lines = Vec::new();
    if words.is_empty() {
        return list_lines(workspace_root);
    }
    if words[0] == "--remove" {
        let Some(id) = words.get(1) else {
            return vec!["usage: /setup --remove <pack>".into()];
        };
        if let Err(e) = remove(workspace_root, id, &mut |l| lines.push(l)) {
            lines.push(format!("[setup] {e}"));
        }
        return lines;
    }
    let yes = words.iter().any(|w| w == "--yes" || w == "-y");
    words.retain(|w| !w.starts_with('-'));
    let (all, _) = packs::load(workspace_root);
    let chosen = match pick(&all, &words) {
        Ok(c) => c,
        Err(e) => return vec![format!("[setup] {e}")],
    };
    let ask = &mut |_: &str| yes;
    let result = install(
        workspace_root,
        &chosen,
        McpScope::Project,
        false,
        ask,
        &mut |l| lines.push(l),
    );
    if let Err(e) = result {
        lines.push(format!("[setup] {e}"));
    }
    if !yes && chosen.iter().any(|p| !p.mcp.is_empty()) {
        lines.push(
            "MCP servers were skipped; add them with /setup … --yes or dcode-ai setup".into(),
        );
    }
    lines
}
