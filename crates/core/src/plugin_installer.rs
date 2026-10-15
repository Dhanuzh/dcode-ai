//! Installing, updating and removing plugins (`dcode-ai plugin …`), and the
//! marketplaces they come from.
//!
//! Plugin sources: a local directory, `owner/repo` (GitHub), any git URL, or
//! `name@marketplace`. A *marketplace* is a repo or directory with
//! `.claude-plugin/marketplace.json` (Claude Code's format) listing plugins
//! by relative path, `github` repo, git `url`, or `git-subdir`. Known
//! marketplaces are cloned under `~/.dcode-ai/marketplaces/`.

use std::path::{Path, PathBuf};

use dcode_ai_common::plugins::{InstalledPlugin, Registry, read_manifest, validate_name};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::skill_installer::git_head_commit;

/// The marketplace offered when none is configured.
pub const DEFAULT_MARKETPLACE: &str = "anthropics/claude-plugins-official";

/// Where a source lives.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Source {
    Local(PathBuf),
    Git(String),
}

fn resolve_source(source: &str) -> Result<Source, String> {
    let s = source.trim();
    let expanded = match s.strip_prefix("~/") {
        Some(rest) => dcode_ai_common::config::home_dir()
            .map(|h| h.join(rest))
            .unwrap_or_else(|| PathBuf::from(s)),
        None => PathBuf::from(s),
    };
    if expanded.is_dir() {
        return Ok(Source::Local(expanded));
    }
    if s.starts_with("https://") || s.starts_with("http://") || s.starts_with("git@") {
        return Ok(Source::Git(s.to_string()));
    }
    let parts: Vec<&str> = s.split('/').collect();
    if parts.len() == 2 && parts.iter().all(|p| !p.is_empty() && !p.contains(':')) {
        return Ok(Source::Git(format!("https://github.com/{s}.git")));
    }
    Err(format!(
        "cannot find source `{s}` (a directory, owner/repo, or git URL)"
    ))
}

/// Shallow clone of `url` (optionally at branch/tag `git_ref`) into `dest`.
fn git_clone(url: &str, git_ref: Option<&str>, dest: &Path) -> Result<(), String> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(["clone", "--quiet", "--depth", "1"]);
    if let Some(r) = git_ref.filter(|r| !r.is_empty()) {
        cmd.args(["--branch", r]);
    }
    cmd.arg(url).arg(dest);
    let out = cmd
        .env("GIT_TERMINAL_PROMPT", "0")
        .output()
        .map_err(|e| format!("git not available: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "git clone {url} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// A fetched tree, kept alive with any temp clone.
struct Fetched {
    root: PathBuf,
    commit: Option<String>,
    _tmp: Option<tempfile::TempDir>,
}

fn fetch(source: &Source) -> Result<Fetched, String> {
    match source {
        Source::Local(path) => Ok(Fetched {
            root: path.clone(),
            commit: git_head_commit(path).ok(),
            _tmp: None,
        }),
        Source::Git(url) => fetch_git(url, None, None),
    }
}

fn fetch_git(url: &str, git_ref: Option<&str>, subdir: Option<&str>) -> Result<Fetched, String> {
    let tmp = tempfile::tempdir().map_err(|e| format!("temp dir: {e}"))?;
    let checkout = tmp.path().join("src");
    git_clone(url, git_ref, &checkout)?;
    let commit = git_head_commit(&checkout).ok();
    let root = match subdir {
        Some(sub) => contained(&checkout, sub)?,
        None => checkout,
    };
    Ok(Fetched {
        root,
        commit,
        _tmp: Some(tmp),
    })
}

/// `base/rel`, refusing paths that escape `base`.
fn contained(base: &Path, rel: &str) -> Result<PathBuf, String> {
    let path = base.join(rel.trim_start_matches("./"));
    let canon = path.canonicalize().map_err(|e| format!("{rel}: {e}"))?;
    let base = base.canonicalize().map_err(|e| e.to_string())?;
    if !canon.starts_with(&base) {
        return Err(format!("path `{rel}` escapes its repository"));
    }
    Ok(canon)
}

// ── Marketplace entries ──────────────────────────────────────────────────

/// One plugin listed by a marketplace.
#[derive(Debug, Clone, PartialEq)]
pub struct MarketEntry {
    pub name: String,
    pub description: String,
    pub category: Option<String>,
    pub author: Option<String>,
    pub version: Option<String>,
    pub homepage: Option<String>,
    pub source: Value,
    /// `false`: the plugin may have no manifest; the entry describes it.
    pub strict: bool,
    pub raw: Value,
}

impl MarketEntry {
    fn from_json(v: &Value) -> Option<Self> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        Some(Self {
            name: s("name")?,
            description: s("description").unwrap_or_default(),
            category: s("category"),
            author: v
                .get("author")
                .and_then(|a| a.get("name").and_then(Value::as_str).or(a.as_str()))
                .map(str::to_string),
            version: s("version"),
            homepage: s("homepage"),
            source: v.get("source").cloned().unwrap_or(Value::Null),
            strict: v.get("strict").and_then(Value::as_bool).unwrap_or(true),
            raw: v.clone(),
        })
    }

    /// Manifest for plugins installed without one (`strict: false`).
    fn synthesized_manifest(&self) -> Value {
        let mut m = json!({ "name": self.name, "description": self.description });
        if let Some(v) = &self.version {
            m["version"] = json!(v);
        }
        for key in [
            "commands",
            "skills",
            "agents",
            "hooks",
            "mcpServers",
            "lspServers",
        ] {
            if let Some(v) = self.raw.get(key) {
                m[key] = v.clone();
            }
        }
        m
    }
}

/// `(marketplace name, entries)` of the marketplace rooted at `root`.
pub fn read_marketplace(root: &Path) -> Result<(String, Vec<MarketEntry>), String> {
    let path = root.join(".claude-plugin/marketplace.json");
    let text = std::fs::read_to_string(&path)
        .map_err(|_| format!("{} has no .claude-plugin/marketplace.json", root.display()))?;
    let doc: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = doc
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{}: `name` is required", path.display()))?
        .to_string();
    validate_name(&name)?;
    let entries = doc
        .get("plugins")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(MarketEntry::from_json).collect())
        .unwrap_or_default();
    Ok((name, entries))
}

fn materialize(market_root: &Path, entry: &MarketEntry) -> Result<Fetched, String> {
    match &entry.source {
        Value::String(rel) => Ok(Fetched {
            root: contained(market_root, rel)?,
            commit: git_head_commit(market_root).ok(),
            _tmp: None,
        }),
        Value::Object(obj) => {
            let s = |k: &str| obj.get(k).and_then(Value::as_str);
            let git_ref = s("ref");
            match s("source") {
                Some("github") => {
                    let repo = s("repo").ok_or("github source needs `repo`")?;
                    fetch_git(&format!("https://github.com/{repo}.git"), git_ref, None)
                }
                Some("url") | Some("git") => {
                    fetch_git(s("url").ok_or("url source needs `url`")?, git_ref, None)
                }
                Some("git-subdir") => fetch_git(
                    s("url").ok_or("git-subdir source needs `url`")?,
                    git_ref,
                    Some(s("path").ok_or("git-subdir source needs `path`")?),
                ),
                other => Err(format!(
                    "plugin `{}`: unsupported source type {}",
                    entry.name,
                    other.unwrap_or("(missing)")
                )),
            }
        }
        _ => Ok(Fetched {
            root: market_root.to_path_buf(),
            commit: git_head_commit(market_root).ok(),
            _tmp: None,
        }),
    }
}

/// Copy a plugin tree, skipping VCS metadata and symlinks.
fn copy_tree(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    for entry in std::fs::read_dir(src).map_err(|e| format!("{}: {e}", src.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let ty = entry.file_type().map_err(|e| e.to_string())?;
        let (from, to) = (entry.path(), dst.join(&name));
        if ty.is_dir() {
            copy_tree(&from, &to)?;
        } else if ty.is_file() {
            std::fs::copy(&from, &to).map_err(|e| format!("{}: {e}", from.display()))?;
        }
    }
    Ok(())
}

/// Copy `root` into `plugins_dir/<name>` and record it. `entry` supplies a
/// manifest when the tree has none and the marketplace allows that.
fn install_tree(
    plugins_dir: &Path,
    root: &Path,
    source_label: &str,
    commit: Option<String>,
    entry: Option<&MarketEntry>,
) -> Result<InstalledPlugin, String> {
    let staging_name = format!(".installing-{}", std::process::id());
    let staging = plugins_dir.join(&staging_name);
    let _ = std::fs::remove_dir_all(&staging);
    copy_tree(root, &staging)?;
    let manifest = match read_manifest(&staging) {
        Ok(m) => m,
        Err(e) => match entry {
            Some(entry) => {
                let dir = staging.join(".dcode-plugin");
                std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
                let text = serde_json::to_string_pretty(&entry.synthesized_manifest())
                    .map_err(|e| e.to_string())?;
                std::fs::write(dir.join("plugin.json"), text).map_err(|e| e.to_string())?;
                read_manifest(&staging)?
            }
            None => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(e);
            }
        },
    };
    validate_name(&manifest.name)?;
    let target = plugins_dir.join(&manifest.name);
    if target.exists() {
        std::fs::remove_dir_all(&target).map_err(|e| format!("{}: {e}", target.display()))?;
    }
    std::fs::rename(&staging, &target).map_err(|e| format!("{}: {e}", target.display()))?;

    let mut registry = Registry::load_from(plugins_dir);
    let enabled = registry.get(&manifest.name).is_none_or(|p| p.enabled);
    let installed = InstalledPlugin {
        name: manifest.name.clone(),
        source: source_label.to_string(),
        version: manifest
            .version
            .clone()
            .or_else(|| entry.and_then(|e| e.version.clone())),
        commit,
        enabled,
    };
    registry.upsert(installed.clone());
    registry.save_to(plugins_dir)?;
    Ok(installed)
}

/// Install a plugin into `plugins_dir`, replacing an existing copy.
///
/// `source` is a directory, `owner/repo`, git URL, or `name@marketplace`
/// (a known marketplace); `pick` chooses a plugin when the source is itself
/// a marketplace.
pub fn install(
    plugins_dir: &Path,
    source: &str,
    pick: Option<&str>,
) -> Result<InstalledPlugin, String> {
    if let Some(markets) = marketplaces_dir()
        && let Some((name, market)) = parse_market_ref(source)
    {
        return install_from_market(plugins_dir, &markets, market, name);
    }
    install_from_source(plugins_dir, source, pick)
}

/// `name@market` or the recorded `marketplace:market/name`.
fn parse_market_ref(source: &str) -> Option<(&str, &str)> {
    if let Some(rest) = source.strip_prefix("marketplace:") {
        let (market, name) = rest.split_once('/')?;
        return Some((name, market));
    }
    let (name, market) = source.split_once('@')?;
    let simple = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
    };
    (simple(name) && simple(market)).then_some((name, market))
}

fn install_from_source(
    plugins_dir: &Path,
    source: &str,
    pick: Option<&str>,
) -> Result<InstalledPlugin, String> {
    let fetched = fetch(&resolve_source(source)?)?;
    let direct = read_manifest(&fetched.root);
    let use_direct = match (&direct, pick) {
        (Ok(m), Some(p)) => m.name == p,
        (Ok(_), None) => true,
        (Err(_), _) => false,
    };
    if use_direct {
        return install_tree(
            plugins_dir,
            &fetched.root,
            source,
            fetched.commit.clone(),
            None,
        );
    }
    let (_, entries) = read_marketplace(&fetched.root).map_err(|_| match direct {
        Err(e) => e,
        Ok(m) => format!(
            "source is plugin `{}`, not `{}`",
            m.name,
            pick.unwrap_or("")
        ),
    })?;
    let names = || {
        entries
            .iter()
            .map(|e| e.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let entry = match pick {
        Some(name) => entries
            .iter()
            .find(|e| e.name == name)
            .ok_or_else(|| format!("marketplace has no plugin `{name}`; available: {}", names()))?,
        None if entries.len() == 1 => &entries[0],
        None => {
            return Err(format!(
                "source is a marketplace; pick a plugin with --plugin <name>: {}",
                names()
            ));
        }
    };
    let tree = materialize(&fetched.root, entry)?;
    install_tree(
        plugins_dir,
        &tree.root,
        source,
        tree.commit.clone().or(fetched.commit.clone()),
        Some(entry),
    )
}

/// Install `name` from the known marketplace `market`.
pub fn install_from_market(
    plugins_dir: &Path,
    markets_dir: &Path,
    market: &str,
    name: &str,
) -> Result<InstalledPlugin, String> {
    let known = KnownMarkets::load_from(markets_dir);
    let m = known.get(market).ok_or_else(|| {
        format!("unknown marketplace `{market}` (dcode-ai plugin marketplace add …)")
    })?;
    let root = m.root(markets_dir);
    let (_, entries) = read_marketplace(&root)?;
    let entry = entries
        .iter()
        .find(|e| e.name == name)
        .ok_or_else(|| format!("marketplace `{market}` has no plugin `{name}`"))?;
    let tree = materialize(&root, entry)?;
    install_tree(
        plugins_dir,
        &tree.root,
        &format!("marketplace:{market}/{name}"),
        tree.commit.clone(),
        Some(entry),
    )
}

/// Remove an installed plugin's files and registry entry.
pub fn remove(plugins_dir: &Path, name: &str) -> Result<(), String> {
    validate_name(name)?;
    let mut registry = Registry::load_from(plugins_dir);
    if !registry.remove(name) {
        return Err(format!("plugin `{name}` is not installed"));
    }
    let dir = plugins_dir.join(name);
    if dir.exists() {
        std::fs::remove_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    registry.save_to(plugins_dir)
}

/// Enable or disable an installed plugin.
pub fn set_enabled(plugins_dir: &Path, name: &str, enabled: bool) -> Result<(), String> {
    let mut registry = Registry::load_from(plugins_dir);
    if !registry.set_enabled(name, enabled) {
        return Err(format!("plugin `{name}` is not installed"));
    }
    registry.save_to(plugins_dir)
}

/// Re-install plugins from their recorded sources (all when `name` is None).
/// Marketplace plugins refresh their marketplace first.
pub fn update(plugins_dir: &Path, name: Option<&str>) -> Result<Vec<InstalledPlugin>, String> {
    let registry = Registry::load_from(plugins_dir);
    let targets: Vec<InstalledPlugin> = registry
        .plugins
        .iter()
        .filter(|p| name.is_none_or(|n| p.name == n))
        .cloned()
        .collect();
    if let Some(n) = name
        && targets.is_empty()
    {
        return Err(format!("plugin `{n}` is not installed"));
    }
    let markets = marketplaces_dir();
    let mut refreshed: Vec<String> = Vec::new();
    targets
        .iter()
        .map(|p| {
            if let (Some(dir), Some((_, market))) = (&markets, parse_market_ref(&p.source))
                && !refreshed.iter().any(|m| m == market)
            {
                let _ = update_market(dir, market);
                refreshed.push(market.to_string());
            }
            install(plugins_dir, &p.source, Some(&p.name))
        })
        .collect()
}

// ── Known marketplaces ───────────────────────────────────────────────────

/// `~/.dcode-ai/marketplaces`.
pub fn marketplaces_dir() -> Option<PathBuf> {
    dcode_ai_common::config::dcode_ai_home_dir().map(|h| h.join("marketplaces"))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KnownMarket {
    pub name: String,
    /// What `marketplace add` was given.
    pub source: String,
    /// Local directory marketplaces are read in place; git ones are cloned
    /// into `<marketplaces>/<name>`.
    #[serde(default)]
    pub local_path: Option<PathBuf>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

impl KnownMarket {
    pub fn root(&self, markets_dir: &Path) -> PathBuf {
        self.local_path
            .clone()
            .unwrap_or_else(|| markets_dir.join(&self.name))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KnownMarkets {
    #[serde(default)]
    pub markets: Vec<KnownMarket>,
}

impl KnownMarkets {
    pub fn load_from(dir: &Path) -> Self {
        std::fs::read_to_string(dir.join("known.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    fn save_to(&self, dir: &Path) -> Result<(), String> {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("known.json"), text + "\n").map_err(|e| e.to_string())
    }

    pub fn get(&self, name: &str) -> Option<&KnownMarket> {
        self.markets.iter().find(|m| m.name == name)
    }
}

fn now_label() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M UTC").to_string()
}

/// Add (or re-add) a marketplace; returns its name.
pub fn add_market(markets_dir: &Path, source: &str) -> Result<String, String> {
    std::fs::create_dir_all(markets_dir).map_err(|e| e.to_string())?;
    let (name, local_path) = match resolve_source(source)? {
        Source::Local(path) => {
            let path = path.canonicalize().map_err(|e| e.to_string())?;
            (read_marketplace(&path)?.0, Some(path))
        }
        Source::Git(url) => {
            let staging = markets_dir.join(format!(".adding-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&staging);
            git_clone(&url, None, &staging)?;
            let name = match read_marketplace(&staging) {
                Ok((n, _)) => n,
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&staging);
                    return Err(e);
                }
            };
            let target = markets_dir.join(&name);
            if target.exists() {
                std::fs::remove_dir_all(&target).map_err(|e| e.to_string())?;
            }
            std::fs::rename(&staging, &target).map_err(|e| e.to_string())?;
            (name, None)
        }
    };
    let mut known = KnownMarkets::load_from(markets_dir);
    known.markets.retain(|m| m.name != name);
    known.markets.push(KnownMarket {
        name: name.clone(),
        source: source.to_string(),
        local_path,
        updated_at: Some(now_label()),
    });
    known.markets.sort_by(|a, b| a.name.cmp(&b.name));
    known.save_to(markets_dir)?;
    Ok(name)
}

/// Pull the latest marketplace listing.
pub fn update_market(markets_dir: &Path, name: &str) -> Result<(), String> {
    let mut known = KnownMarkets::load_from(markets_dir);
    let market = known
        .markets
        .iter_mut()
        .find(|m| m.name == name)
        .ok_or_else(|| format!("unknown marketplace `{name}`"))?;
    if market.local_path.is_none() {
        let root = markets_dir.join(name);
        let out = std::process::Command::new("git")
            .args(["-C"])
            .arg(&root)
            .args(["pull", "--quiet", "--ff-only"])
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .map_err(|e| format!("git not available: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "updating `{name}` failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
    }
    market.updated_at = Some(now_label());
    known.save_to(markets_dir)
}

/// Forget a marketplace (and delete its clone). Installed plugins stay.
pub fn remove_market(markets_dir: &Path, name: &str) -> Result<(), String> {
    let mut known = KnownMarkets::load_from(markets_dir);
    let Some(pos) = known.markets.iter().position(|m| m.name == name) else {
        return Err(format!("unknown marketplace `{name}`"));
    };
    let market = known.markets.remove(pos);
    if market.local_path.is_none() {
        let root = markets_dir.join(&market.name);
        if root.exists() {
            std::fs::remove_dir_all(&root).map_err(|e| e.to_string())?;
        }
    }
    known.save_to(markets_dir)
}

/// Every plugin offered by known marketplaces, `(marketplace, entry)`, plus
/// a message per marketplace that could not be read.
pub fn all_entries(markets_dir: &Path) -> (Vec<(String, MarketEntry)>, Vec<String>) {
    let mut out = Vec::new();
    let mut errors = Vec::new();
    for m in KnownMarkets::load_from(markets_dir).markets {
        match read_marketplace(&m.root(markets_dir)) {
            Ok((_, entries)) => out.extend(entries.into_iter().map(|e| (m.name.clone(), e))),
            Err(e) => errors.push(format!("marketplace `{}`: {e}", m.name)),
        }
    }
    (out, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_plugin(root: &Path, name: &str) {
        std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        std::fs::write(
            root.join(".claude-plugin/plugin.json"),
            format!(r#"{{"name":"{name}","version":"0.1.0"}}"#),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("commands")).unwrap();
        std::fs::write(root.join("commands/hi.md"), "hi").unwrap();
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::write(root.join(".git/HEAD"), "x").unwrap();
    }

    fn make_market(root: &Path) {
        make_plugin(&root.join("plugins/alpha"), "alpha");
        make_plugin(&root.join("plugins/beta"), "beta");
        // A manifest-less plugin described by its entry.
        std::fs::create_dir_all(root.join("plugins/loose/skills/tidy")).unwrap();
        std::fs::write(
            root.join("plugins/loose/skills/tidy/SKILL.md"),
            "---\nname: tidy\n---\nTidy up.",
        )
        .unwrap();
        std::fs::create_dir_all(root.join(".claude-plugin")).unwrap();
        std::fs::write(
            root.join(".claude-plugin/marketplace.json"),
            r#"{"name":"m","plugins":[
                {"name":"alpha","source":"./plugins/alpha","category":"dev","author":{"name":"A"}},
                {"name":"beta","source":"./plugins/beta","description":"Beta tools"},
                {"name":"loose","source":"./plugins/loose","strict":false,"version":"2.0.0","description":"No manifest"},
                {"name":"evil","source":"../../etc"},
                {"name":"weird","source":{"source":"npm","package":"x"}}]}"#,
        )
        .unwrap();
    }

    #[test]
    fn installs_disables_updates_and_removes_a_local_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src-demo");
        make_plugin(&src, "demo");
        let plugins = dir.path().join("plugins");

        let entry = install(&plugins, src.to_str().unwrap(), None).unwrap();
        assert_eq!(entry.name, "demo");
        assert_eq!(entry.version.as_deref(), Some("0.1.0"));
        assert!(plugins.join("demo/commands/hi.md").exists());
        assert!(!plugins.join("demo/.git").exists(), ".git is not copied");

        set_enabled(&plugins, "demo", false).unwrap();
        // Updating keeps the user's enabled choice.
        let updated = update(&plugins, Some("demo")).unwrap();
        assert!(!updated[0].enabled);

        remove(&plugins, "demo").unwrap();
        assert!(!plugins.join("demo").exists());
        assert!(remove(&plugins, "demo").is_err());
    }

    #[test]
    fn marketplace_source_installs_the_named_plugin() {
        let dir = tempfile::tempdir().unwrap();
        let market = dir.path().join("market");
        make_market(&market);
        let plugins = dir.path().join("plugins");
        let src = market.to_str().unwrap();

        let err = install(&plugins, src, None).unwrap_err();
        assert!(err.contains("--plugin"), "{err}");
        assert_eq!(install(&plugins, src, Some("beta")).unwrap().name, "beta");
        assert!(
            install(&plugins, src, Some("gamma"))
                .unwrap_err()
                .contains("alpha")
        );
        assert!(install(&plugins, src, Some("evil")).is_err());
        assert!(
            install(&plugins, src, Some("weird"))
                .unwrap_err()
                .contains("npm")
        );
    }

    #[test]
    fn known_marketplace_lists_and_installs_including_strict_false() {
        let dir = tempfile::tempdir().unwrap();
        let market = dir.path().join("market");
        make_market(&market);
        let markets = dir.path().join("markets");
        let plugins = dir.path().join("plugins");

        assert_eq!(add_market(&markets, market.to_str().unwrap()).unwrap(), "m");
        let (entries, errors) = all_entries(&markets);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(entries.len(), 5);
        let alpha = &entries.iter().find(|(_, e)| e.name == "alpha").unwrap().1;
        assert_eq!(alpha.category.as_deref(), Some("dev"));
        assert_eq!(alpha.author.as_deref(), Some("A"));

        let loose = install_from_market(&plugins, &markets, "m", "loose").unwrap();
        assert_eq!(loose.source, "marketplace:m/loose");
        assert_eq!(loose.version.as_deref(), Some("2.0.0"));
        let manifest = read_manifest(&plugins.join("loose")).unwrap();
        assert_eq!(manifest.description.as_deref(), Some("No manifest"));
        assert!(plugins.join("loose/skills/tidy/SKILL.md").exists());

        update_market(&markets, "m").unwrap();
        remove_market(&markets, "m").unwrap();
        assert!(market.exists(), "local marketplaces are never deleted");
        assert!(all_entries(&markets).0.is_empty());
    }

    #[test]
    fn sources_and_market_refs_resolve() {
        assert_eq!(
            resolve_source("anthropics/claude-plugins").unwrap(),
            Source::Git("https://github.com/anthropics/claude-plugins.git".into())
        );
        assert!(matches!(
            resolve_source("https://gitlab.com/a/b.git").unwrap(),
            Source::Git(_)
        ));
        assert!(resolve_source("not a source").is_err());
        assert_eq!(
            parse_market_ref("code-review@official"),
            Some(("code-review", "official"))
        );
        assert_eq!(
            parse_market_ref("marketplace:m/loose"),
            Some(("loose", "m"))
        );
        assert_eq!(parse_market_ref("git@github.com:a/b.git"), None);
    }
}
