//! `dcode-ai plugin …` and the `/plugins` overview.

use std::path::PathBuf;

use clap::Subcommand;
use dcode_ai_common::plugins::{installed_in, plugins_dir};
use dcode_ai_core::plugin_installer;

#[derive(Debug, Subcommand)]
pub enum PluginCommand {
    /// List installed plugins and what they add
    List {
        #[arg(long)]
        json: bool,
    },
    /// Install from `name@marketplace`, a directory, `owner/repo`, or git URL
    Install {
        source: String,
        /// Plugin to install when the source is a marketplace
        #[arg(long, short = 'p')]
        plugin: Option<String>,
    },
    /// Uninstall a plugin
    Remove { name: String },
    /// Enable a disabled plugin
    Enable { name: String },
    /// Disable a plugin without uninstalling it
    Disable { name: String },
    /// Re-install from the recorded source (all plugins when no name is given)
    Update { name: Option<String> },
    /// Search plugins offered by your marketplaces
    Search { query: Option<String> },
    /// Manage plugin marketplaces (repos with .claude-plugin/marketplace.json)
    #[command(subcommand)]
    Marketplace(MarketCommand),
}

#[derive(Debug, Subcommand)]
pub enum MarketCommand {
    /// Add a marketplace: owner/repo, git URL, or directory
    Add { source: String },
    /// List known marketplaces
    List,
    /// Pull the latest listing (all when no name is given)
    Update { name: Option<String> },
    /// Forget a marketplace (installed plugins stay)
    Remove { name: String },
}

fn markets_dir() -> anyhow::Result<PathBuf> {
    plugin_installer::marketplaces_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))
}

fn run_market(cmd: MarketCommand) -> anyhow::Result<()> {
    let dir = markets_dir()?;
    match cmd {
        MarketCommand::Add { source } => {
            let name = plugin_installer::add_market(&dir, &source).map_err(anyhow::Error::msg)?;
            let count = plugin_installer::all_entries(&dir)
                .0
                .iter()
                .filter(|(m, _)| *m == name)
                .count();
            println!(
                "Added marketplace `{name}` ({count} plugins). Install with: dcode-ai plugin install <plugin>@{name}"
            );
        }
        MarketCommand::List => {
            let known = plugin_installer::KnownMarkets::load_from(&dir);
            if known.markets.is_empty() {
                println!(
                    "No marketplaces. Add one: dcode-ai plugin marketplace add {}",
                    plugin_installer::DEFAULT_MARKETPLACE
                );
            }
            let (entries, errors) = plugin_installer::all_entries(&dir);
            for m in &known.markets {
                let n = entries.iter().filter(|(name, _)| *name == m.name).count();
                println!(
                    "{:<28} {n:>4} plugins  {}  ({})",
                    m.name,
                    m.source,
                    m.updated_at.as_deref().unwrap_or("never updated")
                );
            }
            for e in errors {
                eprintln!("! {e}");
            }
        }
        MarketCommand::Update { name } => {
            let names: Vec<String> = match name {
                Some(n) => vec![n],
                None => plugin_installer::KnownMarkets::load_from(&dir)
                    .markets
                    .into_iter()
                    .map(|m| m.name)
                    .collect(),
            };
            for n in names {
                match plugin_installer::update_market(&dir, &n) {
                    Ok(()) => println!("Updated marketplace `{n}`"),
                    Err(e) => eprintln!("! {e}"),
                }
            }
        }
        MarketCommand::Remove { name } => {
            plugin_installer::remove_market(&dir, &name).map_err(anyhow::Error::msg)?;
            println!("Removed marketplace `{name}`");
        }
    }
    Ok(())
}

fn dir() -> anyhow::Result<PathBuf> {
    plugins_dir().ok_or_else(|| anyhow::anyhow!("no home directory"))
}

pub use crate::tui::plugin_summary::contributions;

/// Human-readable plugin list (shared by the CLI and `/plugins`).
pub fn overview_lines() -> Vec<String> {
    let Some(dir) = plugins_dir() else {
        return vec!["No home directory.".into()];
    };
    let plugins = installed_in(&dir);
    if plugins.is_empty() {
        return vec![
            "No plugins installed.".into(),
            String::new(),
            "Install one (Claude Code plugin layout):".into(),
            "  dcode-ai plugin install <dir | owner/repo | git url> [--plugin name]".into(),
        ];
    }
    let mut lines = Vec::new();
    for p in &plugins {
        let dot = if p.enabled { "●" } else { "○" };
        let version = p
            .manifest
            .version
            .as_deref()
            .map(|v| format!(" v{v}"))
            .unwrap_or_default();
        lines.push(format!(
            "{dot} {}{version}  {}",
            p.name(),
            if p.enabled { "" } else { "(disabled)" }
        ));
        if let Some(d) = &p.manifest.description {
            lines.push(format!("    {d}"));
        }
        lines.push(format!("    {}", contributions(p)));
    }
    lines.push(String::new());
    lines.push(format!(
        "Commands run as /<plugin>:<command>. Manage: /plugins enable|disable <name>, or dcode-ai plugin …  ({})",
        dir.display()
    ));
    lines
}

pub fn run(command: Option<PluginCommand>) -> anyhow::Result<()> {
    let dir = dir()?;
    match command.unwrap_or(PluginCommand::List { json: false }) {
        PluginCommand::List { json: true } => {
            let rows: Vec<_> = installed_in(&dir)
                .iter()
                .map(|p| {
                    serde_json::json!({
                        "name": p.name(),
                        "version": p.manifest.version,
                        "description": p.manifest.description,
                        "enabled": p.enabled,
                        "path": p.root,
                        "adds": contributions(p),
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&rows)?);
        }
        PluginCommand::List { json: false } => {
            for l in overview_lines() {
                println!("{l}");
            }
        }
        PluginCommand::Install { source, plugin } => {
            let entry = plugin_installer::install(&dir, &source, plugin.as_deref())
                .map_err(anyhow::Error::msg)?;
            let installed = installed_in(&dir);
            let adds = installed
                .iter()
                .find(|p| p.name() == entry.name)
                .map(contributions)
                .unwrap_or_default();
            println!(
                "Installed plugin `{}`{} — {adds}",
                entry.name,
                entry
                    .version
                    .as_deref()
                    .map(|v| format!(" v{v}"))
                    .unwrap_or_default()
            );
            println!("Restart dcode-ai sessions to load it.");
        }
        PluginCommand::Remove { name } => {
            plugin_installer::remove(&dir, &name).map_err(anyhow::Error::msg)?;
            println!("Removed plugin `{name}`");
        }
        PluginCommand::Enable { name } => {
            plugin_installer::set_enabled(&dir, &name, true).map_err(anyhow::Error::msg)?;
            println!("Enabled plugin `{name}`");
        }
        PluginCommand::Disable { name } => {
            plugin_installer::set_enabled(&dir, &name, false).map_err(anyhow::Error::msg)?;
            println!("Disabled plugin `{name}`");
        }
        PluginCommand::Marketplace(cmd) => run_market(cmd)?,
        PluginCommand::Search { query } => {
            let (entries, errors) = plugin_installer::all_entries(&markets_dir()?);
            let installed: Vec<String> = installed_in(&dir)
                .iter()
                .map(|p| p.name().to_string())
                .collect();
            let q = query.unwrap_or_default().to_ascii_lowercase();
            let mut shown = 0;
            for (market, e) in &entries {
                let hay = format!(
                    "{} {} {}",
                    e.name,
                    e.description,
                    e.category.as_deref().unwrap_or("")
                )
                .to_ascii_lowercase();
                if !q.is_empty() && !hay.contains(&q) {
                    continue;
                }
                let mark = if installed.contains(&e.name) {
                    "●"
                } else {
                    "○"
                };
                println!(
                    "{mark} {}@{market}  {}",
                    e.name,
                    crate::tui::render_helpers::truncate_chars(&e.description, 90)
                );
                shown += 1;
            }
            if entries.is_empty() {
                println!(
                    "No marketplaces yet. Add one: dcode-ai plugin marketplace add {}",
                    plugin_installer::DEFAULT_MARKETPLACE
                );
            } else if shown == 0 {
                println!("No plugins match.");
            }
            for e in errors {
                eprintln!("! {e}");
            }
        }
        PluginCommand::Update { name } => {
            let updated =
                plugin_installer::update(&dir, name.as_deref()).map_err(anyhow::Error::msg)?;
            if updated.is_empty() {
                println!("No plugins installed.");
            }
            for p in updated {
                println!(
                    "Updated `{}`{}",
                    p.name,
                    p.commit
                        .as_deref()
                        .map(|c| format!(" @ {}", &c[..c.len().min(8)]))
                        .unwrap_or_default()
                );
            }
        }
    }
    Ok(())
}
