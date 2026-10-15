//! Clipboard abstraction with arboard + OSC 52 + tmux passthrough.
//!
//! Ported from koda's koda-cli/src/clipboard.rs (MIT, koda project).
//! Strategy mirrors Claude Code's `osc.ts`:
//!
//! 1. Native copy (arboard) when not over SSH.
//! 2. `tmux load-buffer -w -` when inside tmux.
//! 3. OSC 52 (raw or DCS-passthrough wrapped) written to /dev/tty.

use base64::{Engine as _, engine::general_purpose::STANDARD as B64};
use std::io::Write;

const OSC52_MAX_RAW_BYTES: usize = 100_000;

/// Copy `text` to the system clipboard.
///
/// Returns a short status phrase for embedding in a user-facing message.
/// Returns `Err` only when all backends fail.
pub fn copy_to_clipboard(text: &str) -> Result<String, String> {
    let mut successes: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();

    if !is_ssh_connection() {
        match try_arboard(text) {
            Ok(()) => successes.push("to clipboard (native)".to_string()),
            Err(e) => errors.push(format!("native copy failed: {e}")),
        }
    }
    if is_tmux() {
        match tmux_load_buffer(text) {
            Ok(()) => successes.push("to clipboard (tmux buffer)".to_string()),
            Err(e) => errors.push(format!("tmux copy failed: {e}")),
        }
    }
    match osc52_write(text) {
        Ok(msg) => successes.push(msg),
        Err(e) => errors.push(format!("OSC 52 failed: {e}")),
    }

    if let Some(msg) = successes.into_iter().next() {
        Ok(msg)
    } else if errors.is_empty() {
        Err("no clipboard backend available".to_string())
    } else {
        Err(errors.join("; "))
    }
}

fn is_ssh_connection() -> bool {
    std::env::var("SSH_CONNECTION").is_ok()
}

fn is_tmux() -> bool {
    std::env::var("TMUX").is_ok()
}

fn try_arboard(text: &str) -> Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.set_text(text).map_err(|e| e.to_string())
}

fn tmux_load_buffer(text: &str) -> Result<(), String> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    let Ok(mut child) = Command::new("tmux")
        .args(["load-buffer", "-w", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Err("failed to spawn tmux".to_string());
    };

    if let Some(stdin) = child.stdin.take() {
        let mut stdin = stdin;
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| format!("tmux stdin write failed: {e}"))?;
    }
    let status = child.wait().map_err(|e| format!("tmux wait failed: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("tmux exited with status {status}"))
    }
}

fn osc52_write(text: &str) -> Result<String, String> {
    let raw = text.as_bytes();
    if raw.len() > OSC52_MAX_RAW_BYTES {
        return Err(format!(
            "payload too large for OSC 52 ({} bytes, max {OSC52_MAX_RAW_BYTES})",
            raw.len()
        ));
    }

    let encoded = B64.encode(raw);
    let inner = format!("\x1b]52;c;{encoded}\x07");

    let seq = if is_tmux() {
        let doubled = inner.replace('\x1b', "\x1b\x1b");
        format!("\x1bPtmux;{doubled}\x1b\\")
    } else {
        inner
    };

    write_to_tty(&seq)?;

    Ok(if is_tmux() {
        "to clipboard (via tmux)".to_string()
    } else {
        "to clipboard (via terminal)".to_string()
    })
}

fn write_to_tty(data: &str) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::fs::OpenOptions;
        if let Ok(mut tty) = OpenOptions::new().write(true).open("/dev/tty") {
            return tty
                .write_all(data.as_bytes())
                .and_then(|()| tty.flush())
                .map_err(|e| format!("/dev/tty write error: {e}"));
        }
    }

    let mut err = std::io::stderr().lock();
    err.write_all(data.as_bytes())
        .and_then(|()| err.flush())
        .map_err(|e| format!("stderr write error: {e}"))
}
