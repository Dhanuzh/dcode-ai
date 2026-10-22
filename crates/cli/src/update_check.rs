//! Lightweight "update available" check (Codex parity).
//!
//! Mirrors Codex's `updates.rs`: a cached `version.json` in the dcode-ai home
//! dir records the latest published version and when it was last checked. On
//! startup we read the cache synchronously (so the banner can show a hint with
//! zero network latency) and, when the cache is older than the refresh
//! interval, spawn a background task to refresh it for the *next* run. The
//! network call never blocks startup.

use std::path::PathBuf;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// The version this binary was built as.
const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// GitHub "latest release" endpoint for the dcode-ai repo.
const RELEASES_URL: &str = "https://api.github.com/repos/Dhanuzh/dcode-ai/releases/latest";
/// Don't hit the network more than once per this many hours.
const CHECK_INTERVAL_HOURS: i64 = 6;

/// A newer version not yet shown to the user (from the cache at startup, or
/// found by the background check during the session).
static PENDING_UPGRADE: Mutex<Option<String>> = Mutex::new(None);

#[derive(Serialize, Deserialize, Clone)]
struct VersionInfo {
    latest_version: String,
    last_checked_at: DateTime<Utc>,
    /// The newer version the user was already told about (shown once).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    notified_version: Option<String>,
}

/// A version worth telling the user about: newer than this build and not
/// already shown.
fn worth_showing(info: &VersionInfo) -> Option<String> {
    (is_newer(&info.latest_version, CURRENT)
        && info.notified_version.as_deref() != Some(info.latest_version.as_str()))
    .then(|| info.latest_version.clone())
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

/// Read the cached latest version (no network) and, when the cache is stale,
/// check GitHub in the background; a newer version found then is reported
/// during the session. Call once, from an async (tokio) context.
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
            match refresh().await {
                Ok(Some(newer)) => set_pending(Some(newer)),
                Ok(None) => {}
                Err(e) => tracing::debug!("update check failed: {e}"),
            }
        });
    }
    let pending = cached.as_ref().and_then(worth_showing);
    set_pending(pending.clone());
    pending
}

fn set_pending(v: Option<String>) {
    if let Ok(mut p) = PENDING_UPGRADE.lock() {
        *p = v;
    }
}

/// The newer version to show, once: taking it records that the user was told,
/// so the hint doesn't come back for the same version.
pub fn take_pending_upgrade() -> Option<String> {
    let latest = PENDING_UPGRADE.lock().ok()?.take()?;
    if let Some(mut info) = read_cache() {
        info.notified_version = Some(latest.clone());
        write_cache(&info);
    }
    Some(latest)
}

/// Fetch the latest release; returns it when it is newer and not yet shown.
async fn refresh() -> anyhow::Result<Option<String>> {
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
    let info = VersionInfo {
        latest_version: release.tag_name.trim_start_matches('v').to_string(),
        last_checked_at: Utc::now(),
        notified_version: read_cache().and_then(|i| i.notified_version),
    };
    write_cache(&info);
    Ok(worth_showing(&info))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_newer_version_is_worth_showing_once() {
        let mut info = VersionInfo {
            latest_version: "999.0.0".into(),
            last_checked_at: Utc::now(),
            notified_version: None,
        };
        assert_eq!(worth_showing(&info).as_deref(), Some("999.0.0"));
        info.notified_version = Some("999.0.0".into());
        assert_eq!(worth_showing(&info), None);
        info.latest_version = "0.0.1".into();
        info.notified_version = None;
        assert_eq!(worth_showing(&info), None);
    }
}
