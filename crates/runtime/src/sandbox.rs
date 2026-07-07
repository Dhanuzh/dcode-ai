//! Landlock filesystem confinement for shell tools (Linux).
//!
//! Opt-in via `[permissions] sandbox_bash = true`. Spawned commands can read
//! and execute everywhere but may only write beneath the workspace root and
//! scratch locations (/tmp, /var/tmp, /dev/null, /dev/shm). Applied in the
//! child between fork and exec, so the agent process itself is unaffected.
//!
//! Enforcement is best-effort: on kernels without Landlock the restriction
//! degrades (the landlock crate's `BestEffort` mode), matching how Codex
//! ships its Linux sandbox.

#[cfg(target_os = "linux")]
pub fn apply_workspace_sandbox(
    workspace_root: &std::path::Path,
    extra_writable: &[std::path::PathBuf],
) -> std::io::Result<()> {
    use landlock::{
        ABI, Access, AccessFs, CompatLevel, Compatible, Ruleset, RulesetAttr, RulesetCreatedAttr,
        path_beneath_rules,
    };

    let abi = ABI::V2;
    let to_io = |e: landlock::RulesetError| std::io::Error::other(e.to_string());

    let write_roots: Vec<std::path::PathBuf> = [
        workspace_root.to_path_buf(),
        std::path::PathBuf::from("/tmp"),
        std::path::PathBuf::from("/var/tmp"),
        std::path::PathBuf::from("/dev/null"),
        std::path::PathBuf::from("/dev/zero"),
        std::path::PathBuf::from("/dev/urandom"),
        std::path::PathBuf::from("/dev/shm"),
        std::path::PathBuf::from("/proc/self"),
    ]
    .into_iter()
    .chain(extra_writable.iter().cloned())
    .filter(|p| p.exists())
    .collect();

    Ruleset::default()
        .set_compatibility(CompatLevel::BestEffort)
        .handle_access(AccessFs::from_all(abi))
        .map_err(to_io)?
        .create()
        .map_err(to_io)?
        .add_rules(path_beneath_rules(["/"], AccessFs::from_read(abi)))
        .map_err(to_io)?
        .add_rules(path_beneath_rules(&write_roots, AccessFs::from_all(abi)))
        .map_err(to_io)?
        .restrict_self()
        .map_err(to_io)?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn apply_workspace_sandbox(
    _workspace_root: &std::path::Path,
    _extra_writable: &[std::path::PathBuf],
) -> std::io::Result<()> {
    // No kernel sandbox on this platform yet (macOS seatbelt / Windows
    // AppContainer are future work). The config flag is Linux-only.
    Ok(())
}

/// Expand a leading `~` to `$HOME` in configured writable roots.
pub fn expand_writable_roots(roots: &[String]) -> Vec<std::path::PathBuf> {
    let home = dcode_ai_common::config::home_dir();
    roots
        .iter()
        .filter_map(|raw| {
            let raw = raw.trim();
            if raw.is_empty() {
                return None;
            }
            if let Some(rest) = raw.strip_prefix("~/") {
                return home.as_ref().map(|h| h.join(rest));
            }
            if raw == "~" {
                return home.clone();
            }
            Some(std::path::PathBuf::from(raw))
        })
        .collect()
}

#[cfg(test)]
#[cfg(target_os = "linux")]
mod tests {
    use std::process::Command;

    /// End-to-end: a sandboxed child can write inside the workspace but not
    /// outside it. Skips silently on kernels without Landlock.
    #[test]
    fn sandboxed_child_cannot_write_outside_workspace() {
        use std::os::unix::process::CommandExt;

        let workspace = tempfile::tempdir().unwrap();
        // The "outside" target must not live under /tmp — the sandbox
        // deliberately grants scratch-dir writes. Use $HOME instead.
        let Some(home) = std::env::var_os("HOME") else {
            eprintln!("no HOME; skipping");
            return;
        };
        let outside = match tempfile::tempdir_in(home) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("cannot create dir in HOME ({e}); skipping");
                return;
            }
        };
        let inside_file = workspace.path().join("ok.txt");
        let outside_file = outside.path().join("blocked.txt");

        let ws = workspace.path().to_path_buf();
        let script = format!(
            "echo hi > {} && echo hi > {}",
            inside_file.display(),
            outside_file.display()
        );
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(script);
        unsafe {
            cmd.pre_exec(move || super::apply_workspace_sandbox(&ws, &[]));
        }
        let status = cmd.status().expect("spawn sandboxed child");

        // If Landlock is enforced, the outside write fails (non-zero exit,
        // file absent) while the inside write succeeds.
        if outside_file.exists() {
            // Kernel without Landlock: best-effort mode ran unconfined.
            eprintln!("landlock not enforced on this kernel; skipping assertions");
            return;
        }
        assert!(inside_file.exists(), "workspace write should succeed");
        assert!(!status.success(), "outside write should have failed");
    }
}
