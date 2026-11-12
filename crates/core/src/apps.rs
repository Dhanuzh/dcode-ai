//! Desktop apps built on Chromium — Electron (VS Code, Slack, Discord,
//! Postman, Notion) and WebView2 (new Teams, new Outlook) — driven like the
//! browser: started with a debug port, attached over the Chrome DevTools
//! Protocol, and reached through the `browser` tool's `target`.
//!
//! `/app open <name|path>` starts one; `/app list`, `/app close <name>`.
//! (Native apps without a web view need the accessibility tool — later.)

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

use tokio::sync::Mutex;

use crate::browser::{Page, attach, port_alive};

/// How an app takes the debug port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `--remote-debugging-port=<p>` on the command line.
    Electron,
    /// `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS=--remote-debugging-port=<p>`.
    WebView2,
}

/// A known app: its name, where it usually is (relative to
/// `%LOCALAPPDATA%`, or absolute), and how it takes the port.
pub struct Known {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub paths: &'static [&'static str],
    pub kind: Kind,
}

pub const KNOWN: &[Known] = &[
    Known {
        name: "vscode",
        aliases: &["code", "vs code", "visual studio code"],
        paths: &["Programs\\Microsoft VS Code\\Code.exe"],
        kind: Kind::Electron,
    },
    Known {
        name: "slack",
        aliases: &[],
        paths: &["slack\\slack.exe"],
        kind: Kind::Electron,
    },
    Known {
        name: "discord",
        aliases: &[],
        paths: &["Discord\\Update.exe"],
        kind: Kind::Electron,
    },
    Known {
        name: "postman",
        aliases: &[],
        paths: &["Postman\\Postman.exe"],
        kind: Kind::Electron,
    },
    Known {
        name: "notion",
        aliases: &[],
        paths: &["Programs\\Notion\\Notion.exe"],
        kind: Kind::Electron,
    },
    Known {
        name: "teams",
        aliases: &["ms-teams", "microsoft teams"],
        paths: &["Microsoft\\WindowsApps\\ms-teams.exe"],
        kind: Kind::WebView2,
    },
    Known {
        name: "outlook",
        aliases: &["olk", "new outlook"],
        paths: &["Microsoft\\WindowsApps\\olk.exe"],
        kind: Kind::WebView2,
    },
];

/// The known app `name` names (by name or alias, any case).
pub fn known(name: &str) -> Option<&'static Known> {
    let n = name.trim().to_ascii_lowercase();
    KNOWN
        .iter()
        .find(|k| k.name == n || k.aliases.contains(&n.as_str()))
}

/// Connected apps by name.
pub fn apps() -> &'static Mutex<HashMap<String, Page>> {
    static APPS: OnceLock<Mutex<HashMap<String, Page>>> = OnceLock::new();
    APPS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn is_wsl() -> bool {
    std::env::var_os("WSL_DISTRO_NAME").is_some()
}

fn output_of(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd)
        .args(args)
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (out.status.success() && !s.is_empty()).then_some(s)
}

/// `%LOCALAPPDATA%` as a path this process can open (WSL: `/mnt/c/…`).
fn local_app_data() -> Option<PathBuf> {
    if cfg!(windows) {
        return std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    }
    if is_wsl() {
        let win = output_of("cmd.exe", &["/c", "echo %LOCALAPPDATA%"])?;
        return output_of("wslpath", &["-u", &win]).map(PathBuf::from);
    }
    None
}

/// Where the known app is installed here.
fn find_known(k: &Known) -> Option<PathBuf> {
    let base = local_app_data()?;
    k.paths
        .iter()
        .map(|p| base.join(p.replace('\\', std::path::MAIN_SEPARATOR_STR)))
        .find(|p| p.is_file())
}

/// Whether a Windows process with this image name runs (`tasklist`).
fn running(exe: &Path) -> bool {
    let Some(name) = exe.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let tasklist = if cfg!(windows) {
        "tasklist"
    } else {
        "tasklist.exe"
    };
    output_of(tasklist, &["/FI", &format!("IMAGENAME eq {name}"), "/NH"])
        .is_some_and(|o| o.to_ascii_lowercase().contains(&name.to_ascii_lowercase()))
}

/// The first free debug port from 9340.
async fn free_port() -> u16 {
    let taken: Vec<u16> = apps().lock().await.values().map(|p| p.port).collect();
    for port in 9340..9400 {
        if !taken.contains(&port) && !port_alive(port).await {
            return port;
        }
    }
    9399
}

/// The command that starts `exe` with debugging on `port`.
pub fn launch_command(exe: &Path, kind: Kind, port: u16) -> std::process::Command {
    let mut cmd = std::process::Command::new(exe);
    let flag = format!("--remote-debugging-port={port}");
    match kind {
        Kind::Electron => {
            // Discord starts through its updater.
            if exe
                .file_name()
                .is_some_and(|n| n.eq_ignore_ascii_case("Update.exe"))
            {
                cmd.args([
                    "--processStart",
                    "Discord.exe",
                    "--process-start-args",
                    &flag,
                ]);
            } else {
                cmd.arg(&flag);
            }
        }
        Kind::WebView2 => {
            cmd.env("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS", &flag);
            // From WSL, the variable must be passed on to Windows.
            if is_wsl() {
                let wslenv = std::env::var("WSLENV").unwrap_or_default();
                cmd.env(
                    "WSLENV",
                    if wslenv.is_empty() {
                        "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS/w".to_string()
                    } else {
                        format!("{wslenv}:WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS/w")
                    },
                );
            }
        }
    }
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    cmd
}

/// A profile folder for a separate copy of `name` (`--new`), as the app
/// expects it (a Windows path from WSL).
fn separate_profile(name: &str) -> Option<String> {
    let base = local_app_data()?;
    let dir = base
        .join("dcode-ai")
        .join("apps")
        .join(format!("{name}-profile"));
    std::fs::create_dir_all(&dir).ok()?;
    if is_wsl() {
        return output_of("wslpath", &["-w", &dir.to_string_lossy()]);
    }
    Some(dir.to_string_lossy().into_owned())
}

/// `/app open <name|path> [--new]`: start the app with a debug port and
/// connect. `new`: a separate copy with its own profile, so an app that's
/// already open can stay open (Electron apps only; VS Code keeps your
/// extensions, not your settings).
pub async fn open(what: &str, new: bool) -> Result<String, String> {
    let what = what.trim();
    let (name, exe, kind) = match known(what) {
        Some(k) => {
            let exe = find_known(k).ok_or_else(|| {
                format!(
                    "{} isn't installed where it usually is — give its path: /app open <path to .exe>",
                    k.name
                )
            })?;
            (k.name.to_string(), exe, k.kind)
        }
        None => {
            let exe = PathBuf::from(what);
            if !exe.is_file() {
                return Err(format!(
                    "no app `{what}` — one of: {}, or a path to an Electron/WebView2 app",
                    KNOWN.iter().map(|k| k.name).collect::<Vec<_>>().join(", ")
                ));
            }
            let name = exe
                .file_stem()
                .map(|s| s.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_else(|| "app".into());
            (name, exe, Kind::Electron)
        }
    };
    if apps().lock().await.get(&name).is_some_and(|p| p.is_alive()) {
        return Ok(format!(
            "{name} is already connected — the agent reaches it with target \"{name}\""
        ));
    }
    if new && kind == Kind::WebView2 {
        return Err(format!(
            "{name} can't run a second copy — close it (quit from its tray icon too), then /app open {name}"
        ));
    }
    // A running single-instance app hands the new start to itself and the
    // debug flag is lost — unless this is a separate copy with its own
    // profile.
    if !new && running(&exe) {
        return Err(format!(
            "{name} is already running — close it (quit from its tray icon too) and /app open {name} again, or /app open {name} --new for a separate copy beside it"
        ));
    }
    let port = free_port().await;
    let mut cmd = launch_command(&exe, kind, port);
    if new {
        let profile = separate_profile(&name)
            .ok_or("could not make a profile folder for the separate copy")?;
        cmd.arg(format!("--user-data-dir={profile}"));
        if name == "vscode" {
            cmd.arg("--new-window");
        }
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", exe.display()))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    for _ in 0..120 {
        if port_alive(port).await {
            let page = attach(port).await?;
            apps().lock().await.insert(name.clone(), page);
            return Ok(format!(
                "{name} started with debugging on port {port} — the agent reaches it with the browser tool's target \"{name}\""
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    Err(format!(
        "{name} started but its debug port {port} didn't answer{}",
        if is_wsl() {
            " — on WSL this needs mirrored networking (networkingMode=mirrored in .wslconfig)"
        } else {
            ""
        }
    ))
}

/// `/app connect <name> <port>`: attach to an app you started yourself
/// with `--remote-debugging-port=<port>`.
pub async fn connect(name: &str, port: u16) -> Result<String, String> {
    let name = name.trim().to_ascii_lowercase();
    let page = attach(port)
        .await
        .map_err(|e| format!("{e} — start the app with --remote-debugging-port={port} first"))?;
    apps().lock().await.insert(name.clone(), page);
    Ok(format!(
        "connected to {name} on port {port} — the agent reaches it with the browser tool's target \"{name}\""
    ))
}

/// `/app list`.
pub async fn list() -> Vec<String> {
    let map = apps().lock().await;
    let mut out: Vec<String> = map
        .iter()
        .map(|(n, p)| {
            format!(
                "{n} · port {} · {}",
                p.port,
                if p.is_alive() { "connected" } else { "gone" }
            )
        })
        .collect();
    out.sort();
    out
}

/// `/app close <name>`: disconnect (the app stays open).
pub async fn close(name: &str) -> Result<String, String> {
    match apps().lock().await.remove(name.trim()) {
        Some(_) => Ok(format!("disconnected from {name} (the app stays open)")),
        None => Err(format!("no app `{name}` connected")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_apps_by_name_or_alias() {
        assert_eq!(known("VS Code").unwrap().name, "vscode");
        assert_eq!(known("code").unwrap().name, "vscode");
        assert_eq!(known("teams").unwrap().kind, Kind::WebView2);
        assert!(known("excel").is_none());
    }

    #[test]
    fn the_debug_port_goes_where_each_kind_takes_it() {
        let args = |c: &std::process::Command| -> Vec<String> {
            c.get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        let c = launch_command(Path::new("/x/Code.exe"), Kind::Electron, 9340);
        assert_eq!(args(&c), ["--remote-debugging-port=9340"]);
        let c = launch_command(Path::new("/x/Update.exe"), Kind::Electron, 9341);
        assert_eq!(
            args(&c),
            [
                "--processStart",
                "Discord.exe",
                "--process-start-args",
                "--remote-debugging-port=9341"
            ]
        );
        let c = launch_command(Path::new("/x/ms-teams.exe"), Kind::WebView2, 9342);
        assert!(args(&c).is_empty());
        let env: Vec<(String, String)> = c
            .get_envs()
            .filter_map(|(k, v)| {
                Some((
                    k.to_string_lossy().into_owned(),
                    v?.to_string_lossy().into_owned(),
                ))
            })
            .collect();
        assert!(env.contains(&(
            "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS".into(),
            "--remote-debugging-port=9342".into()
        )));
    }
}
