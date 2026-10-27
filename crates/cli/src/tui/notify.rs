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
