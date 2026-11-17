//! Crash reports: a panic leaves a crash file in `~/.dcode-ai/crashes/`, and
//! `/bugreport` gathers what a bug report needs — version, system, the
//! newest crash, the log's tail, the config and the session's recent events
//! — into one text file, with secrets masked, to attach to an issue.

use std::path::{Path, PathBuf};

fn crashes_dir() -> Option<PathBuf> {
    dcode_ai_common::config::dcode_ai_home_dir().map(|d| d.join("crashes"))
}

/// Records panics (message, place, backtrace) in a crash file, then runs
/// the hook that was there before.
pub fn install_crash_hook() {
    use std::sync::Once;
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let _ = write_crash(&format!(
                "{info}\n\nthread: {}\n\nbacktrace:\n{}",
                std::thread::current().name().unwrap_or("?"),
                std::backtrace::Backtrace::force_capture()
            ));
            previous(info);
        }));
    });
}

fn write_crash(text: &str) -> std::io::Result<PathBuf> {
    let dir = crashes_dir().ok_or_else(|| std::io::Error::other("no home"))?;
    std::fs::create_dir_all(&dir)?;
    let p = dir.join(format!(
        "crash-{}.txt",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ));
    std::fs::write(
        &p,
        format!(
            "dcode-ai {} crashed\n\n{}",
            env!("CARGO_PKG_VERSION"),
            dcode_ai_common::redact::redact_secrets(text)
        ),
    )?;
    Ok(p)
}

/// The newest crash file, if any.
fn newest_crash() -> Option<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(crashes_dir()?)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "txt"))
        .collect();
    v.sort();
    v.pop()
}

/// The last `n` lines of a file.
fn tail(path: &Path, n: usize) -> String {
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::from("(none)\n");
    };
    let lines: Vec<&str> = text.lines().collect();
    let from = lines.len().saturating_sub(n);
    let mut out = lines[from..].join("\n");
    out.push('\n');
    out
}

/// Config with secret-looking values blanked, line by line.
fn masked_config(text: &str) -> String {
    text.lines()
        .map(|l| {
            let key = l
                .split('=')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            let secret = [
                "key", "token", "secret", "password", "auth", "cookie", "webhook",
            ]
            .iter()
            .any(|w| key.contains(w));
            if secret && l.contains('=') {
                format!("{} = \"***\"", l.split('=').next().unwrap_or("").trim_end())
            } else {
                dcode_ai_common::redact::redact_secrets(l)
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// What a report is made from.
pub struct Inputs<'a> {
    pub workspace: &'a Path,
    pub session_id: &'a str,
    pub event_log: &'a Path,
    pub model: &'a str,
}

/// Writes the report into `<workspace>/.dcode-ai/bugreports/`; returns it.
pub fn write_report(i: &Inputs) -> Result<PathBuf, String> {
    let home = dcode_ai_common::config::dcode_ai_home_dir().ok_or("no home folder")?;
    let mut r = String::new();
    r.push_str(&format!(
        "# dcode-ai bug report\n\nversion: {}\nos: {} {}\nwsl: {}\nterminal: {}\nmodel: {}\nsession: {}\nwhen: {}\n\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        std::env::var_os("WSL_DISTRO_NAME").is_some(),
        std::env::var("TERM_PROGRAM")
            .or_else(|_| std::env::var("TERM"))
            .unwrap_or_default(),
        i.model,
        i.session_id,
        chrono::Local::now().to_rfc3339(),
    ));
    r.push_str("## What happened\n\n(describe what you did and what went wrong)\n\n");
    r.push_str("## Newest crash\n\n```\n");
    match newest_crash() {
        Some(p) => r.push_str(&std::fs::read_to_string(&p).unwrap_or_default()),
        None => r.push_str("(no crash recorded)\n"),
    }
    r.push_str("```\n\n## Log (last 300 lines)\n\n```\n");
    r.push_str(&dcode_ai_common::redact::redact_secrets(&tail(
        &home.join("dcode-ai.log"),
        300,
    )));
    r.push_str("```\n\n## Config (secrets masked)\n\n```toml\n");
    for p in [
        home.join("config.toml"),
        i.workspace.join(".dcode-ai/config.toml"),
    ] {
        if let Ok(t) = std::fs::read_to_string(&p) {
            r.push_str(&format!("# {}\n{}\n", p.display(), masked_config(&t)));
        }
    }
    r.push_str("```\n\n## Session events (last 200)\n\n```\n");
    r.push_str(&dcode_ai_common::redact::redact_secrets(&tail(
        i.event_log,
        200,
    )));
    r.push_str("```\n");
    let dir = i.workspace.join(".dcode-ai/bugreports");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let p = dir.join(format!(
        "bugreport-{}.md",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ));
    std::fs::write(&p, r).map_err(|e| e.to_string())?;
    Ok(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_in_config_are_masked() {
        let c = "model = \"opus\"\napi_key = \"sk-ant-abc\"\nslack_webhook = \"https://x\"\n[ui]\ntheme = \"claude\"";
        let m = masked_config(c);
        assert!(m.contains("model = \"opus\""));
        assert!(m.contains("api_key = \"***\""));
        assert!(m.contains("slack_webhook = \"***\""));
        assert!(!m.contains("sk-ant-abc"));
        assert!(m.contains("theme = \"claude\""));
    }

    #[test]
    fn a_report_gathers_the_parts() {
        let d = tempfile::tempdir().unwrap();
        let log = d.path().join("s.events.jsonl");
        std::fs::write(&log, "{\"e\":1}\n{\"e\":2}\n").unwrap();
        let p = write_report(&Inputs {
            workspace: d.path(),
            session_id: "session-1",
            event_log: &log,
            model: "m",
        })
        .unwrap();
        let t = std::fs::read_to_string(p).unwrap();
        assert!(t.contains("version: "));
        assert!(t.contains("{\"e\":2}"));
        assert!(t.contains("## What happened"));
    }
}
