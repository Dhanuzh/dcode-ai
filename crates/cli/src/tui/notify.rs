//! Desktop notifications when a turn finishes (or needs approval) while the
//! terminal is in the background: a Windows toast (also from WSL), macOS
//! Notification Center, or `notify-send` on Linux. Terminals that show OSC 9
//! notifications get one too when no native way is found.

use std::process::{Command, Stdio};

/// PowerShell's app id: toasts from an unregistered id are dropped.
const POWERSHELL_AUMID: &str =
    r"{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}\WindowsPowerShell\v1.0\powershell.exe";

fn is_wsl() -> bool {
    std::env::var_os("WSL_DISTRO_NAME").is_some()
}

fn on_path(bin: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(bin).is_file()))
}

/// PowerShell that shows a toast with `title` and `body`.
fn toast_script(title: &str, body: &str) -> String {
    let q = |s: &str| s.replace('\'', "''");
    format!(
        "[Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] > $null; \
         $t = [Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent([Windows.UI.Notifications.ToastTemplateType]::ToastText02); \
         $x = $t.GetElementsByTagName('text'); \
         $x.Item(0).AppendChild($t.CreateTextNode('{}')) > $null; \
         $x.Item(1).AppendChild($t.CreateTextNode('{}')) > $null; \
         [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{POWERSHELL_AUMID}').Show([Windows.UI.Notifications.ToastNotification]::new($t))",
        q(title),
        q(body)
    )
}

/// Text worth reading aloud: no code blocks, inline code marks, links or
/// Markdown symbols; at most ~1500 characters.
pub fn speakable(text: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        let cleaned: String = line
            .trim_start_matches(['#', '>', '-', '*', ' '])
            .chars()
            .filter(|c| !matches!(c, '`' | '*' | '_' | '|'))
            .collect();
        if !cleaned.trim().is_empty() {
            out.push_str(cleaned.trim());
            out.push(' ');
        }
    }
    out.chars()
        .take(1500)
        .collect::<String>()
        .trim()
        .to_string()
}

/// Read `text` aloud with the system's voice, in the background: `say` on
/// macOS, the speech synthesizer on Windows (also from WSL), `spd-say` or
/// `espeak` on Linux. The text goes in on stdin, never in a command line.
pub fn speak(text: &str) -> Result<(), String> {
    use std::io::Write;
    let text = speakable(text);
    if text.is_empty() {
        return Err("nothing to read aloud".into());
    }
    let mut cmd = if cfg!(windows) || (is_wsl() && on_path("powershell.exe")) {
        let mut c = Command::new(if cfg!(windows) {
            "powershell"
        } else {
            "powershell.exe"
        });
        c.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "Add-Type -AssemblyName System.Speech; \
             (New-Object System.Speech.Synthesis.SpeechSynthesizer).Speak([Console]::In.ReadToEnd())",
        ]);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = Command::new("say");
        c.args(["-f", "-"]);
        c
    } else if on_path("spd-say") {
        let mut c = Command::new("spd-say");
        c.args(["-e"]);
        c
    } else if on_path("espeak") {
        let mut c = Command::new("espeak");
        c.args(["--stdin"]);
        c
    } else {
        return Err("no voice found (install spd-say or espeak)".into());
    };
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(text.as_bytes());
    }
    // Reap it in the background.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// AppleScript string literal.
fn apple_quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The command that shows a native notification here, if any.
fn native_command(title: &str, body: &str) -> Option<Command> {
    if cfg!(windows) || (is_wsl() && on_path("powershell.exe")) {
        let mut c = Command::new(if cfg!(windows) {
            "powershell"
        } else {
            "powershell.exe"
        });
        c.args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            &toast_script(title, body),
        ]);
        return Some(c);
    }
    if cfg!(target_os = "macos") {
        let mut c = Command::new("osascript");
        c.args([
            "-e",
            &format!(
                "display notification {} with title {}",
                apple_quote(body),
                apple_quote(title)
            ),
        ]);
        return Some(c);
    }
    if on_path("notify-send") {
        let mut c = Command::new("notify-send");
        c.args(["--app-name=dcode-ai", title, body]);
        return Some(c);
    }
    None
}

/// Show a notification without blocking the UI.
pub fn desktop_notify(title: &str, body: &str) {
    match native_command(title, body) {
        Some(mut cmd) => {
            cmd.stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if let Ok(mut child) = cmd.spawn() {
                // Reap it off the UI thread.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
            }
        }
        None => {
            use std::io::Write;
            let mut so = std::io::stdout();
            // OSC 9 notification, plus a bell for terminals that only ring.
            let _ = write!(so, "\x1b]9;{title}: {body}\x07\x07");
            let _ = so.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_are_escaped_for_each_shell() {
        let ps = toast_script("dcode-ai", "it's done");
        assert!(ps.contains("CreateTextNode('it''s done')"), "{ps}");
        assert_eq!(apple_quote(r#"say "hi" \ ok"#), r#""say \"hi\" \\ ok""#);
    }
}

#[cfg(test)]
mod speak_tests {
    use super::speakable;

    #[test]
    fn code_and_markdown_are_not_read() {
        let text =
            "## Done\nFixed **the bug** in `auth.rs`.\n```rust\nfn main() {}\n```\n- tests pass";
        assert_eq!(speakable(text), "Done Fixed the bug in auth.rs. tests pass");
    }
}
