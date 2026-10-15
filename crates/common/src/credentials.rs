//! File-backed secret store: keeps provider API keys out of the shareable
//! config files.
//!
//! Secrets live in `~/.dcode-ai/credentials.toml`, created with `0600`
//! permissions on Unix. Entries are keyed by the provider's `api_key_env`
//! name (e.g. `OPENAI_API_KEY`), so resolution slots in right after the real
//! environment variable. Reads go through an in-process cache; `set`/`remove`
//! update the cache and rewrite the file atomically.
//!
//! `DCODE_AI_CREDENTIALS_PATH` overrides the location (used by tests).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

fn cache() -> &'static Mutex<Option<BTreeMap<String, String>>> {
    static CACHE: OnceLock<Mutex<Option<BTreeMap<String, String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

pub fn credentials_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("DCODE_AI_CREDENTIALS_PATH") {
        return Some(PathBuf::from(p));
    }
    crate::config::dcode_ai_home_dir().map(|home| home.join("credentials.toml"))
}

fn load_from_disk() -> BTreeMap<String, String> {
    let Some(path) = credentials_path() else {
        return BTreeMap::new();
    };
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return BTreeMap::new();
    };
    toml::from_str::<BTreeMap<String, String>>(&raw).unwrap_or_default()
}

fn with_cache<R>(f: impl FnOnce(&mut BTreeMap<String, String>) -> R) -> R {
    let mut guard = match cache().lock() {
        Ok(g) => g,
        Err(poisoned) => poisoned.into_inner(),
    };
    if guard.is_none() {
        *guard = Some(load_from_disk());
    }
    f(guard.as_mut().expect("cache initialized above"))
}

/// Look up a secret by its env-name key (e.g. `OPENAI_API_KEY`).
pub fn get(env_name: &str) -> Option<String> {
    with_cache(|map| map.get(env_name).cloned()).filter(|v| !v.trim().is_empty())
}

/// All stored secrets as (env-name, value) pairs. Used by output redaction so
/// stored keys can never leak through tool output verbatim.
pub fn all() -> Vec<(String, String)> {
    with_cache(|map| {
        map.iter()
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect()
    })
}

fn persist(map: &BTreeMap<String, String>) -> Result<PathBuf, String> {
    let path = credentials_path().ok_or_else(|| "cannot resolve HOME".to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let body = toml::to_string_pretty(map).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, &body).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Store a secret; returns the credentials file path.
pub fn set(env_name: &str, secret: &str) -> Result<PathBuf, String> {
    with_cache(|map| {
        map.insert(env_name.to_string(), secret.to_string());
        persist(map)
    })
}

/// Remove a secret; returns whether it existed.
pub fn remove(env_name: &str) -> Result<bool, String> {
    with_cache(|map| {
        let existed = map.remove(env_name).is_some();
        if existed {
            persist(map)?;
        }
        Ok(existed)
    })
}

/// Drop the in-process cache (next read reloads from disk). Test helper, but
/// harmless anywhere.
pub fn invalidate_cache() {
    if let Ok(mut guard) = cache().lock() {
        *guard = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The cache and env var are process-global, so all assertions live in one
    // test to avoid cross-test interference.
    #[test]
    fn set_get_remove_roundtrip_with_0600_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("credentials.toml");
        // SAFETY: single-threaded within this test; the only test touching it.
        unsafe { std::env::set_var("DCODE_AI_CREDENTIALS_PATH", &path) };
        invalidate_cache();

        assert_eq!(get("TEST_PROVIDER_KEY"), None);

        set("TEST_PROVIDER_KEY", "sk-secret").unwrap();
        assert_eq!(get("TEST_PROVIDER_KEY"), Some("sk-secret".to_string()));

        // Survives a cache drop (reads back from disk).
        invalidate_cache();
        assert_eq!(get("TEST_PROVIDER_KEY"), Some("sk-secret".to_string()));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "credentials file must be 0600");
        }

        // Empty values resolve as absent.
        set("TEST_EMPTY_KEY", "  ").unwrap();
        assert_eq!(get("TEST_EMPTY_KEY"), None);

        assert!(remove("TEST_PROVIDER_KEY").unwrap());
        assert_eq!(get("TEST_PROVIDER_KEY"), None);
        assert!(!remove("TEST_PROVIDER_KEY").unwrap());

        unsafe { std::env::remove_var("DCODE_AI_CREDENTIALS_PATH") };
        invalidate_cache();
    }
}
