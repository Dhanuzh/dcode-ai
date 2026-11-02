//! Tailscale, for joining a session from another network: find it, its
//! address, and how to install it. dcode-ai never installs it on its own —
//! `dcode-ai setup tailscale` asks first, then runs Tailscale's own
//! installer; signing in happens in Tailscale's page.

use std::process::{Command, Stdio};
use std::time::Duration;

/// The `tailscale` command to use: the native one, or on WSL the Windows
/// one (`tailscale.exe`, which mirrored networking shares with WSL).
fn binary() -> Option<&'static str> {
    ["tailscale", "tailscale.exe"].into_iter().find(|b| {
        Command::new(b)
            .arg("version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

pub fn installed() -> bool {
    binary().is_some()
}

/// This machine's Tailscale address (`100.x.y.z`), when it's connected.
pub fn ip() -> Option<String> {
    let bin = binary()?;
    let mut child = Command::new(bin)
        .args(["ip", "-4"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // Don't hang when the daemon isn't running.
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(50))
            }
            _ => {
                let _ = child.kill();
                return None;
            }
        }
    }
    let out = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .map(str::trim)
        .find(|l| l.starts_with("100.") && l.parse::<std::net::Ipv4Addr>().is_ok())
        .map(str::to_string)
}

/// How to install Tailscale here: what it does, and the commands (run in a
/// shell) — Tailscale's official installers only.
pub fn install_plan() -> (String, Vec<String>) {
    let wsl = std::env::var_os("WSL_DISTRO_NAME").is_some();
    if wsl {
        return (
            "On WSL, install Tailscale on Windows (WSL shares its network): winget, then sign in from the tray icon.".into(),
            vec!["winget.exe install --id Tailscale.Tailscale -e".into()],
        );
    }
    if cfg!(target_os = "macos") {
        return (
            "Install Tailscale with Homebrew, then open it and sign in.".into(),
            vec![
                "brew install --cask tailscale".into(),
                "open -a Tailscale".into(),
            ],
        );
    }
    if cfg!(windows) {
        return (
            "Install Tailscale with winget, then sign in from the tray icon.".into(),
            vec!["winget install --id Tailscale.Tailscale -e".into()],
        );
    }
    (
        "Install Tailscale with its official script (asks for your password), then sign in through the link it prints.".into(),
        vec![
            "curl -fsSL https://tailscale.com/install.sh | sh".into(),
            "sudo tailscale up".into(),
        ],
    )
}
