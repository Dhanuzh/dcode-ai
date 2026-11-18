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

/// The voice `/talk` replies use (`/talk voice`): `system` (the default),
/// `piper:<voice>` or `windows:<voice name>`. Kept in
/// `~/.dcode-ai/talk-voice`.
pub fn talk_voice() -> String {
    dcode_ai_common::config::dcode_ai_home_dir()
        .and_then(|d| std::fs::read_to_string(d.join("talk-voice")).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "system".into())
}

pub fn set_talk_voice(v: &str) -> Result<(), String> {
    let d = dcode_ai_common::config::dcode_ai_home_dir().ok_or("no home folder")?;
    std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
    std::fs::write(d.join("talk-voice"), v).map_err(|e| e.to_string())
}

/// Windows voices here (the classic and the newer OneCore ones): (name,
/// language like `en-US`). Empty off Windows/WSL.
pub fn windows_voices() -> Vec<(String, String)> {
    if !(cfg!(windows) || (is_wsl() && on_path("powershell.exe"))) {
        return Vec::new();
    }
    let script = "$null = [Windows.Media.SpeechSynthesis.SpeechSynthesizer,Windows.Media.SpeechSynthesis,ContentType=WindowsRuntime]; \
        [Windows.Media.SpeechSynthesis.SpeechSynthesizer]::AllVoices | ForEach-Object { $_.DisplayName + '|' + $_.Language }";
    let out = Command::new(if cfg!(windows) {
        "powershell"
    } else {
        "powershell.exe"
    })
    .args(["-NoProfile", "-NonInteractive", "-Command", script])
    .stdin(Stdio::null())
    .output();
    let Ok(out) = out else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let (n, lang) = l.trim().split_once('|')?;
            Some((n.trim().to_string(), lang.trim().to_string()))
        })
        .collect()
}

/// Piper voices installed in `~/.dcode-ai/voices` (names like
/// `en_US-lessac-medium`).
pub fn piper_voices() -> Vec<String> {
    let Some(dir) = dcode_ai_common::config::dcode_ai_home_dir().map(|d| d.join("voices")) else {
        return Vec::new();
    };
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            e.file_name()
                .to_str()?
                .strip_suffix(".onnx")
                .map(str::to_string)
        })
        .collect();
    v.sort();
    v
}

/// How a reply will be read.
#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    /// The system's default voice (the classic way, as before).
    System,
    /// A Piper voice.
    Piper(String),
    /// A Windows voice, by its name.
    Windows(String),
}

/// Picks the voice for a reply in `lang` (`None`: the chosen voice as is).
/// A language with no voice here is an error (reading Tamil with an
/// English voice says nonsense), naming the language.
pub fn plan_voice(
    chosen: &str,
    lang: Option<&str>,
    piper: &[String],
    windows: &[(String, String)],
) -> Result<Plan, String> {
    let chosen_plan = match chosen.split_once(':') {
        Some(("piper", v)) if piper.iter().any(|p| p == v) => Plan::Piper(v.to_string()),
        Some(("windows", v)) => Plan::Windows(v.to_string()),
        _ => Plan::System,
    };
    let Some(lang) = lang.filter(|l| *l != "en") else {
        // English: the chosen voice, unless it's a Piper voice of another
        // language.
        if let Plan::Piper(v) = &chosen_plan
            && !v.starts_with("en")
        {
            return Ok(Plan::System);
        }
        return Ok(chosen_plan);
    };
    // The chosen voice, when it speaks this language.
    match &chosen_plan {
        Plan::Piper(v) if v.starts_with(&format!("{lang}_")) => return Ok(chosen_plan),
        Plan::Windows(n)
            if windows
                .iter()
                .any(|(wn, wl)| wn == n && wl.starts_with(&format!("{lang}-"))) =>
        {
            return Ok(chosen_plan);
        }
        _ => {}
    }
    // Any installed voice for it: Piper first (more natural), then Windows.
    if let Some(v) = piper.iter().find(|v| v.starts_with(&format!("{lang}_"))) {
        return Ok(Plan::Piper(v.clone()));
    }
    if let Some((n, _)) = windows
        .iter()
        .find(|(_, l)| l.starts_with(&format!("{lang}-")))
    {
        return Ok(Plan::Windows(n.clone()));
    }
    // macOS and Linux voices are picked by language as before.
    if !windows.is_empty() || cfg!(windows) {
        return Err(lang.to_string());
    }
    Ok(Plan::System)
}

/// Plays a WAV file, as a child that can be stopped.
fn play_wav(path: &std::path::Path) -> Result<std::process::Child, String> {
    let mut c = if cfg!(windows) || (is_wsl() && on_path("powershell.exe")) {
        let win = if cfg!(windows) {
            path.display().to_string()
        } else {
            let o = Command::new("wslpath")
                .arg("-w")
                .arg(path)
                .output()
                .map_err(|e| e.to_string())?;
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
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
                "(New-Object System.Media.SoundPlayer '{}').PlaySync()",
                win.replace('\'', "''")
            ),
        ]);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = Command::new("afplay");
        c.arg(path);
        c
    } else if on_path("paplay") {
        let mut c = Command::new("paplay");
        c.arg(path);
        c
    } else {
        let mut c = Command::new("aplay");
        c.arg("-q").arg(path);
        c
    };
    c.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())
}

/// A Windows voice by name (classic or OneCore), through the newer speech
/// API; the text comes in on stdin.
fn windows_voice_child(name: &str, text: &str, rate: i32) -> Result<std::process::Child, String> {
    use std::io::Write;
    let script = format!(
        "Add-Type -AssemblyName System.Runtime.WindowsRuntime; \
         $null = [Windows.Media.SpeechSynthesis.SpeechSynthesizer,Windows.Media.SpeechSynthesis,ContentType=WindowsRuntime]; \
         $s = New-Object Windows.Media.SpeechSynthesis.SpeechSynthesizer; \
         $v = [Windows.Media.SpeechSynthesis.SpeechSynthesizer]::AllVoices | Where-Object {{ $_.DisplayName -eq '{name}' }} | Select-Object -First 1; \
         if ($v) {{ $s.Voice = $v }}; \
         try {{ $s.Options.SpeakingRate = {speed} }} catch {{}}; \
         $text = [Console]::In.ReadToEnd(); \
         $asTask = ([System.WindowsRuntimeSystemExtensions].GetMethods() | Where-Object {{ $_.Name -eq 'AsTask' -and $_.GetParameters().Count -eq 1 -and $_.GetParameters()[0].ParameterType.Name -eq 'IAsyncOperation`1' }})[0]; \
         $t = $asTask.MakeGenericMethod([Windows.Media.SpeechSynthesis.SpeechSynthesisStream]).Invoke($null, @($s.SynthesizeTextToStreamAsync($text))); \
         $t.Wait(); \
         $p = New-Object System.Media.SoundPlayer ([System.IO.WindowsRuntimeStreamExtensions]::AsStreamForRead($t.Result)); \
         $p.PlaySync()",
        name = name.replace('\'', "''"),
        speed = 1.0 + rate as f64 * 0.15,
    );
    let mut child = Command::new(if cfg!(windows) {
        "powershell"
    } else {
        "powershell.exe"
    })
    .args(["-NoProfile", "-NonInteractive", "-Command", &script])
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

fn speak_child(text: &str) -> Result<std::process::Child, String> {
    use std::io::Write;
    let text = speakable(text);
    if text.is_empty() {
        return Err("nothing to read aloud".into());
    }
    let sp = speech();
    // The chosen voice, or one for the language the answer is in.
    let chosen = talk_voice();
    let needs_lookup = chosen != "system" || sp.lang.as_deref().is_some_and(|l| l != "en");
    if needs_lookup {
        let windows = if chosen.starts_with("windows:") || sp.lang.is_some() {
            windows_voices()
        } else {
            Vec::new()
        };
        match plan_voice(&chosen, sp.lang.as_deref(), &piper_voices(), &windows) {
            Ok(Plan::Piper(v)) => {
                let wav =
                    std::env::temp_dir().join(format!("dcode-ai-talk-{}.wav", std::process::id()));
                dcode_ai_core::video::voice::speak("piper", &v, &text, &wav)?;
                return play_wav(&wav);
            }
            Ok(Plan::Windows(n)) => return windows_voice_child(&n, &text, sp.rate),
            Ok(Plan::System) => {}
            Err(lang) => {
                return Err(format!(
                    "no voice for {} here, so the answer isn't read aloud (an English voice would say nonsense) — add one: Windows Settings → Time & language → Language & region → {} → Language options → Speech; or a Piper voice for it (/video voices)",
                    crate::voice::stt::language_name(&lang),
                    crate::voice::stt::language_name(&lang),
                ));
            }
        }
    }
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

#[cfg(test)]
mod voice_plan_tests {
    use super::*;

    fn win() -> Vec<(String, String)> {
        vec![
            ("Microsoft David".into(), "en-US".into()),
            ("Microsoft Mark".into(), "en-US".into()),
            ("Microsoft Valluvar".into(), "ta-IN".into()),
        ]
    }

    #[test]
    fn a_language_gets_a_voice_that_speaks_it() {
        let piper = vec![
            "de_DE-thorsten-medium".to_string(),
            "en_US-lessac-medium".to_string(),
        ];
        // Tamil: the Windows Tamil voice.
        assert_eq!(
            plan_voice("system", Some("ta"), &piper, &win()),
            Ok(Plan::Windows("Microsoft Valluvar".into()))
        );
        // German: the Piper German voice, before any Windows one.
        assert_eq!(
            plan_voice("system", Some("de"), &piper, &win()),
            Ok(Plan::Piper("de_DE-thorsten-medium".into()))
        );
        // No voice for it: said plainly, never read by an English voice.
        assert_eq!(
            plan_voice("system", Some("ml"), &piper, &win()),
            Err("ml".into())
        );
    }

    #[test]
    fn the_chosen_voice_is_used_for_its_language() {
        let piper = vec!["en_US-ryan-high".to_string()];
        assert_eq!(
            plan_voice("piper:en_US-ryan-high", None, &piper, &win()),
            Ok(Plan::Piper("en_US-ryan-high".into()))
        );
        assert_eq!(
            plan_voice("windows:Microsoft Mark", Some("en"), &piper, &win()),
            Ok(Plan::Windows("Microsoft Mark".into()))
        );
        // A chosen voice that isn't installed: the system voice.
        assert_eq!(
            plan_voice("piper:gone", None, &piper, &win()),
            Ok(Plan::System)
        );
    }
}

#[cfg(test)]
mod live_voices {
    /// Lists the voices this machine has (nothing is installed or played).
    #[test]
    #[ignore]
    fn lists_this_machines_voices() {
        eprintln!("windows: {:?}", super::windows_voices());
        eprintln!("piper: {:?}", super::piper_voices());
        let plan = super::plan_voice(
            "system",
            Some("ta"),
            &super::piper_voices(),
            &super::windows_voices(),
        );
        eprintln!("tamil → {plan:?}");
    }
}

#[cfg(test)]
mod live_speak {
    #[test]
    #[ignore]
    fn a_windows_voice_by_name_speaks() {
        let mut c = super::windows_voice_child("Microsoft Mark", "Hi.", 0).unwrap();
        let s = c.wait().unwrap();
        assert!(s.success(), "{s:?}");
    }
}
