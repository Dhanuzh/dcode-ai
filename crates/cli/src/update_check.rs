//! Lightweight "update available" check (Codex parity).
//!
//! Mirrors Codex's `updates.rs`: a cached `version.json` in the dcode-ai home
//! dir records the latest published version and when it was last checked. On
//! startup we read the cache synchronously (so the banner can show a hint with
//! zero network latency) and, when the cache is older than the refresh
//! interval, spawn a background task to refresh it for the *next* run. The
//! network call never blocks startup.

use std::path::PathBuf;
use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The version this binary was built as.
const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// GitHub "latest release" endpoint for the dcode-ai repo.
const RELEASES_URL: &str = "https://api.github.com/repos/Dhanuzh/dcode-ai/releases/latest";
/// Don't hit the network more than once per this many hours.
const CHECK_INTERVAL_HOURS: i64 = 20;

/// Cached "upgrade available" version for this run (set once at startup).
static PENDING_UPGRADE: OnceLock<Option<String>> = OnceLock::new();

#[derive(Serialize, Deserialize, Clone)]
struct VersionInfo {
    latest_version: String,
    last_checked_at: DateTime<Utc>,
}

fn version_file() -> Option<PathBuf> {
    dcode_ai_common::config::dcode_ai_home_dir().map(|d| d.join("version.json"))
}

fn read_cache() -> Option<VersionInfo> {
    let contents = std::fs::read_to_string(version_file()?).ok()?;
    serde_json::from_str(&contents).ok()
}

fn write_cache(info: &VersionInfo) {
    let Some(file) = version_file() else { return };
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(info) {
        let _ = std::fs::write(file, json);
    }
}

/// Parse `1.2.3` / `v1.2.3` into a comparable `(major, minor, patch)` tuple.
/// Unparseable components become 0, so a malformed tag never reads as "newer".
fn parse_ver(v: &str) -> (u64, u64, u64) {
    let trimmed = v.trim().trim_start_matches('v');
    let mut parts = trimmed
        .split(['.', '-', '+'])
        .filter_map(|p| p.parse::<u64>().ok());
    (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    )
}

fn is_newer(latest: &str, current: &str) -> bool {
    parse_ver(latest) > parse_ver(current)
}

/// Read the cached latest version (no network). If it's newer than the running
/// build, return it. When the cache is stale or absent, spawn a background
/// refresh for the next run. Call once, from an async (tokio) context.
pub fn init_and_pending_upgrade() -> Option<String> {
    let cached = read_cache();
    let stale = match &cached {
        None => true,
        Some(info) => {
            info.last_checked_at < Utc::now() - chrono::Duration::hours(CHECK_INTERVAL_HOURS)
        }
    };
    if stale {
        tokio::spawn(async {
            if let Err(e) = refresh().await {
                tracing::debug!("update check failed: {e}");
            }
        });
    }
    let pending = cached
        .and_then(|info| is_newer(&info.latest_version, CURRENT).then_some(info.latest_version));
    let _ = PENDING_UPGRADE.set(pending.clone());
    pending
}

/// The cached "upgrade available" version for this run, if any. No network —
/// safe to call from rendering code (e.g. the startup banner).
pub fn pending_upgrade() -> Option<String> {
    PENDING_UPGRADE.get().cloned().flatten()
}

async fn refresh() -> anyhow::Result<()> {
    #[derive(Deserialize)]
    struct Release {
        tag_name: String,
    }
    let client = reqwest::Client::builder()
        .user_agent(concat!("dcode-ai/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let release: Release = client
        .get(RELEASES_URL)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    write_cache(&VersionInfo {
        latest_version: release.tag_name.trim_start_matches('v').to_string(),
        last_checked_at: Utc::now(),
    });
    Ok(())
}
