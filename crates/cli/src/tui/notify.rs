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
    let mut child = speak_child(text)?;
    // Reap it in the background.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Reads `text` aloud and returns when it's done (so the microphone
/// doesn't hear it).
pub fn speak_and_wait(text: &str) -> Result<(), String> {
    speak_child(text)?
        .wait()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Reads `text` aloud until it's done or `stop` is set (then the voice is
/// cut off). Returns whether it was stopped.
pub fn speak_until(text: &str, stop: &std::sync::atomic::AtomicBool) -> Result<bool, String> {
    let mut child = speak_child(text)?;
    loop {
        if stop.load(std::sync::atomic::Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(true);
        }
        match child.try_wait() {
            Ok(Some(_)) => return Ok(false),
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(80)),
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// How spoken replies sound: the language to pick a voice for (`de`…;
/// `None`: the system's default voice) and the speed (−3 slower … +3 faster).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Speech {
    pub lang: Option<String>,
    pub rate: i32,
}

static SPEECH: std::sync::Mutex<Speech> = std::sync::Mutex::new(Speech {
    lang: None,
    rate: 0,
});

/// Sets how [`speak`] and friends sound from now on.
pub fn set_speech(s: Speech) {
    if let Ok(mut g) = SPEECH.lock() {
        *g = s;
    }
}

pub fn speech() -> Speech {
    SPEECH.lock().map(|g| g.clone()).unwrap_or_default()
}

/// A macOS voice for a language (`say -v ?` lists `Name  de_DE  # …`).
fn mac_voice(lang: &str) -> Option<String> {
    let o = Command::new("say").args(["-v", "?"]).output().ok()?;
    String::from_utf8_lossy(&o.stdout).lines().find_map(|l| {
        let mut parts = l.split_whitespace();
        let mut name = Vec::new();
        for p in parts.by_ref() {
            if p.len() >= 5
                && p.as_bytes()[2] == b'_'
                && p[..2].chars().all(|c| c.is_ascii_lowercase())
            {
                return p.starts_with(lang).then(|| name.join(" "));
            }
            name.push(p);
        }
        None
    })
}

fn speak_child(text: &str) -> Result<std::process::Child, String> {
    use std::io::Write;
    let text = speakable(text);
    if text.is_empty() {
        return Err("nothing to read aloud".into());
    }
    let sp = speech();
    // Only a plain language code reaches a command line.
    let lang = sp
        .lang
        .filter(|l| (2..=3).contains(&l.len()) && l.chars().all(|c| c.is_ascii_lowercase()));
    let rate = sp.rate.clamp(-3, 3);
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
            &format!(
                "Add-Type -AssemblyName System.Speech; \
                 $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; \
                 $l = '{}'; if ($l) {{ $v = $s.GetInstalledVoices() | Where-Object {{ $_.VoiceInfo.Culture.TwoLetterISOLanguageName -eq $l }} | Select-Object -First 1; \
                 if ($v) {{ $s.SelectVoice($v.VoiceInfo.Name) }} }}; \
                 $s.Rate = {}; $s.Speak([Console]::In.ReadToEnd())",
                lang.clone().unwrap_or_default(),
                rate * 2
            ),
        ]);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = Command::new("say");
        c.args(["-f", "-", "-r", &(175 + rate * 20).to_string()]);
        if let Some(v) = lang.as_deref().and_then(mac_voice) {
            c.args(["-v", &v]);
        }
        c
    } else if on_path("spd-say") {
        let mut c = Command::new("spd-say");
        c.args(["-e", "-r", &(rate * 15).to_string()]);
        if let Some(l) = &lang {
            c.args(["-l", l]);
        }
        c
    } else if on_path("espeak") {
        let mut c = Command::new("espeak");
        c.args(["--stdin", "-s", &(175 + rate * 20).to_string()]);
        if let Some(l) = &lang {
            c.args(["-v", l]);
        }
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
    Ok(child)
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
