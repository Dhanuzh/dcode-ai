//! Who you are to the others on a session: the name shown in the office,
//! on `dcode-ai join` and on the `/watch` page.

use std::path::Path;

/// Your name: git `user.name` in `workspace`, else `$USER` / `$USERNAME`,
/// else "a teammate".
pub fn my_name(workspace: &Path) -> String {
    std::process::Command::new("git")
        .args(["config", "user.name"])
        .current_dir(workspace)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|n| !n.is_empty())
        .or_else(|| std::env::var("USER").ok().filter(|n| !n.is_empty()))
        .or_else(|| std::env::var("USERNAME").ok().filter(|n| !n.is_empty()))
        .unwrap_or_else(|| "a teammate".into())
}

/// A fresh id for one person on one session (`<prefix>-<random>`).
pub fn new_person_id(prefix: &str) -> String {
    use rand::Rng;
    let n: u64 = rand::thread_rng().r#gen();
    format!("{prefix}-{n:016x}")
}
