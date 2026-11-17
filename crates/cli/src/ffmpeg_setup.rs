//! `/video setup`: downloads a static `ffmpeg` build into
//! `~/.dcode-ai/bin` (for MP4 videos with music) — only when you ask.
//! `/video setup voice`: Piper and an English voice, for a natural draft
//! narration.

use std::path::{Path, PathBuf};

/// The static build for this platform: (URL, what it is).
pub fn source() -> Option<(&'static str, &'static str)> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some((
            "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz",
            "johnvansickle.com static build",
        )),
        ("linux", "aarch64") => Some((
            "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-arm64-static.tar.xz",
            "johnvansickle.com static build",
        )),
        ("macos", _) => Some((
            "https://evermeet.cx/ffmpeg/getrelease/zip",
            "evermeet.cx build",
        )),
        ("windows", _) => Some((
            "https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip",
            "gyan.dev essentials build",
        )),
        _ => None,
    }
}

fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if p.is_dir() {
            if let Some(f) = find(&p, name) {
                return Some(f);
            }
        } else if p.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(p);
        }
    }
    None
}

/// Downloads and installs ffmpeg; returns where it went.
pub async fn install(progress: impl Fn(String)) -> Result<PathBuf, String> {
    let (url, what) = source()
        .ok_or("no ffmpeg download for this platform — install it with your package manager")?;
    let home = dcode_ai_common::config::home_dir().ok_or("no home folder")?;
    let bin = home.join(".dcode-ai").join("bin");
    std::fs::create_dir_all(&bin).map_err(|e| e.to_string())?;
    let work = std::env::temp_dir().join(format!("dcode-ai-ffmpeg-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    progress(format!("downloading ffmpeg ({what})…"));
    let bytes = reqwest::Client::new()
        .get(url)
        .header("User-Agent", "dcode-ai")
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("download: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("download: {e}"))?;
    let archive = work.join(if url.ends_with(".tar.xz") {
        "ffmpeg.tar.xz"
    } else {
        "ffmpeg.zip"
    });
    std::fs::write(&archive, &bytes).map_err(|e| e.to_string())?;
    progress(format!("unpacking ({} MB)…", bytes.len() / 1_000_000));
    // `tar` reads .tar.xz and .zip (bsdtar on macOS and Windows 10+).
    let status = std::process::Command::new("tar")
        .arg("-xf")
        .arg(&archive)
        .arg("-C")
        .arg(&work)
        .status()
        .map_err(|e| format!("tar: {e}"))?;
    if !status.success() {
        return Err("couldn't unpack the download (needs `tar` with xz/zip support)".into());
    }
    let name = if cfg!(windows) {
        "ffmpeg.exe"
    } else {
        "ffmpeg"
    };
    let found = find(&work, name).ok_or("ffmpeg wasn't in the download")?;
    let dest = bin.join(name);
    std::fs::copy(&found, &dest).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755));
    }
    let _ = std::fs::remove_dir_all(&work);
    let ok = std::process::Command::new(&dest)
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        return Err(format!("{} doesn't run here", dest.display()));
    }
    Ok(dest)
}

/// Piper's release for this platform (the last self-contained one).
fn piper_source() -> Option<String> {
    const BASE: &str = "https://github.com/rhasspy/piper/releases/download/2023.11.14-2/";
    let file = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "piper_linux_x86_64.tar.gz",
        ("linux", "aarch64") => "piper_linux_aarch64.tar.gz",
        ("macos", "aarch64") => "piper_macos_aarch64.tar.gz",
        ("macos", _) => "piper_macos_x64.tar.gz",
        ("windows", _) => "piper_windows_amd64.zip",
        _ => return None,
    };
    Some(format!("{BASE}{file}"))
}

/// The default voice: Lessac, medium quality (US English, clear and calm).
pub const VOICE: &str = "en_US-lessac-medium";

/// Piper voices worth knowing about: (name, what it sounds like). Any other
/// name from the Piper voice list works too.
pub const VOICES: &[(&str, &str)] = &[
    (
        "en_US-lessac-medium",
        "US English, clear and calm (default)",
    ),
    ("en_US-amy-medium", "US English, bright, female"),
    ("en_US-ryan-high", "US English, warm, male, high quality"),
    ("en_US-joe-medium", "US English, relaxed, male"),
    ("en_US-kristin-medium", "US English, friendly, female"),
    ("en_GB-alba-medium", "British English, female"),
    ("en_GB-alan-medium", "British English, male"),
    (
        "en_GB-northern_english_male-medium",
        "Northern English, male",
    ),
    ("de_DE-thorsten-medium", "German, male"),
    ("fr_FR-siwis-medium", "French, female"),
    ("es_ES-davefx-medium", "Spanish, male"),
    ("it_IT-paola-medium", "Italian, female"),
    ("pt_BR-faber-medium", "Brazilian Portuguese, male"),
    ("nl_NL-mls-medium", "Dutch"),
    ("hi_IN-pratham-medium", "Hindi, male"),
];

/// Where a voice's files are: `en_US-lessac-medium` →
/// `…/en/en_US/lessac/medium/`.
fn voice_base(name: &str) -> Result<String, String> {
    let mut parts = name.splitn(2, '-');
    let locale = parts.next().unwrap_or("");
    let rest = parts.next().ok_or_else(|| bad_voice(name))?;
    let (speaker, quality) = rest.rsplit_once('-').ok_or_else(|| bad_voice(name))?;
    let lang = locale.split('_').next().unwrap_or("");
    let ok = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !(ok(locale) && locale.contains('_') && ok(speaker) && ok(quality)) {
        return Err(bad_voice(name));
    }
    Ok(format!(
        // Pinned to v1.0.0: newer voice files need a newer Piper than the
        // one installed ("Phonemes must be one codepoint").
        "https://huggingface.co/rhasspy/piper-voices/resolve/v1.0.0/{lang}/{locale}/{speaker}/{quality}/"
    ))
}

fn bad_voice(name: &str) -> String {
    format!("`{name}` isn't a Piper voice name — like en_US-amy-medium (/video voices lists some)")
}

/// The Piper voices installed in `~/.dcode-ai/voices`.
pub fn installed_voices() -> Vec<String> {
    let Some(home) = dcode_ai_common::config::home_dir() else {
        return Vec::new();
    };
    let mut v: Vec<String> = std::fs::read_dir(home.join(".dcode-ai/voices"))
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

async fn fetch(url: &str) -> Result<Vec<u8>, String> {
    Ok(reqwest::Client::new()
        .get(url)
        .header("User-Agent", "dcode-ai")
        .send()
        .await
        .and_then(|r| r.error_for_status())
        .map_err(|e| format!("download: {e}"))?
        .bytes()
        .await
        .map_err(|e| format!("download: {e}"))?
        .to_vec())
}

/// Downloads Piper into `~/.dcode-ai/bin/piper/` and its voice into
/// `~/.dcode-ai/voices/`; returns the voice model.
pub async fn install_piper(progress: impl Fn(String)) -> Result<PathBuf, String> {
    install_piper_voice(VOICE, progress).await
}

/// [`install_piper`] with a voice of your choice.
pub async fn install_piper_voice(
    voice: &str,
    progress: impl Fn(String),
) -> Result<PathBuf, String> {
    let voice_base = voice_base(voice)?;
    let url = piper_source().ok_or("no Piper download for this platform")?;
    let home = dcode_ai_common::config::home_dir().ok_or("no home folder")?;
    let root = home.join(".dcode-ai");
    let bin = root.join("bin");
    let voices = root.join("voices");
    std::fs::create_dir_all(&bin).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&voices).map_err(|e| e.to_string())?;
    let exe = bin
        .join("piper")
        .join(if cfg!(windows) { "piper.exe" } else { "piper" });
    if !exe.is_file() {
        progress("downloading Piper (text to speech, ~25 MB)…".into());
        let bytes = fetch(&url).await?;
        let work = std::env::temp_dir().join(format!("dcode-ai-piper-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        let archive = work.join(if url.ends_with(".zip") {
            "piper.zip"
        } else {
            "piper.tar.gz"
        });
        std::fs::write(&archive, &bytes).map_err(|e| e.to_string())?;
        // The archive holds a `piper/` folder (the program, its libraries
        // and espeak-ng data): unpack it into ~/.dcode-ai/bin.
        let ok = std::process::Command::new("tar")
            .arg("-xf")
            .arg(&archive)
            .arg("-C")
            .arg(&bin)
            .status()
            .is_ok_and(|s| s.success());
        let _ = std::fs::remove_dir_all(&work);
        if !ok || !exe.is_file() {
            return Err("couldn't unpack Piper (needs `tar`)".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755));
        }
    }
    let model = voices.join(format!("{voice}.onnx"));
    for (name, what) in [
        (format!("{voice}.onnx.json"), "its settings"),
        (format!("{voice}.onnx"), "the voice (~20–110 MB)"),
    ] {
        let dest = voices.join(&name);
        if dest.is_file() {
            continue;
        }
        progress(format!("downloading {what}…"));
        let bytes = fetch(&format!("{voice_base}{name}"))
            .await
            .map_err(|e| format!("{e} — is `{voice}` in the Piper voice list?"))?;
        let part = dest.with_extension("part");
        std::fs::write(&part, &bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&part, &dest).map_err(|e| e.to_string())?;
    }
    // A word, to be sure it runs here.
    progress("trying it…".into());
    let probe = voices.join("probe.wav");
    let r =
        dcode_ai_core::video::voice::speak("piper", &model.display().to_string(), "Hello.", &probe);
    let _ = std::fs::remove_file(&probe);
    if let Err(e) = r {
        // A voice that doesn't run here: remove it, so trying again
        // downloads it again.
        let _ = std::fs::remove_file(&model);
        let _ = std::fs::remove_file(model.with_extension("onnx.json"));
        return Err(format!(
            "Piper is installed but didn't run `{voice}` here: {e}"
        ));
    }
    Ok(model)
}

/// A command started fully on its own: no terminal input or output, and
/// its own process group. A Windows program started from WSL with the
/// terminal's stdin otherwise takes the keyboard — dcode-ai's input box
/// stopped answering after `/video open`.
pub fn detached(mut cmd: std::process::Command) -> std::io::Result<()> {
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = cmd.spawn()?;
    // Reaped in the background, never waited on here.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Opens a file with the system's player/viewer (WSL: the Windows one),
/// without touching this terminal.
pub fn open(path: &Path) -> std::io::Result<()> {
    let wsl = std::env::var_os("WSL_DISTRO_NAME").is_some();
    if wsl {
        let win = std::process::Command::new("wslpath")
            .arg("-w")
            .arg(path)
            .stdin(std::process::Stdio::null())
            .output()?;
        let win = String::from_utf8_lossy(&win.stdout).trim().to_string();
        let mut c = std::process::Command::new("explorer.exe");
        c.arg(win);
        return detached(c);
    }
    #[cfg(target_os = "windows")]
    {
        let mut c = std::process::Command::new("cmd");
        c.args(["/C", "start", ""]).arg(path);
        detached(c)?;
    }
    #[cfg(target_os = "macos")]
    {
        let mut c = std::process::Command::new("open");
        c.arg(path);
        detached(c)?;
    }
    #[cfg(target_os = "linux")]
    {
        let mut c = std::process::Command::new("xdg-open");
        c.arg(path);
        detached(c)?;
    }
    Ok(())
}

#[cfg(test)]
mod voice_names {
    use super::*;

    /// Downloads a named voice and speaks with it:
    /// `cargo test -- --ignored a_named_voice_installs`.
    #[tokio::test]
    #[ignore]
    async fn a_named_voice_installs_and_speaks() {
        let model = install_piper_voice("de_DE-thorsten-medium", |s| eprintln!("{s}"))
            .await
            .unwrap();
        assert!(model.ends_with("de_DE-thorsten-medium.onnx"));
        let out = std::env::temp_dir().join("dcode-ai-de.wav");
        dcode_ai_core::video::voice::speak(
            "piper",
            "de_DE-thorsten-medium",
            "Wie spät ist es heute in Berlin?",
            &out,
        )
        .unwrap();
        assert!(std::fs::metadata(&out).unwrap().len() > 10_000);
        eprintln!("spoken: {}", out.display());
    }

    #[test]
    fn voice_names_map_to_their_folder() {
        assert_eq!(
            voice_base("en_US-lessac-medium").unwrap(),
            "https://huggingface.co/rhasspy/piper-voices/resolve/v1.0.0/en/en_US/lessac/medium/"
        );
        assert!(
            voice_base("en_GB-northern_english_male-medium")
                .unwrap()
                .ends_with("/en/en_GB/northern_english_male/medium/")
        );
        assert!(voice_base("lessac").is_err());
        assert!(voice_base("../x-y-z").is_err());
        assert!(VOICES.iter().all(|(v, _)| voice_base(v).is_ok()));
    }
}
