//! `/video setup`: downloads a static `ffmpeg` build into
//! `~/.dcode-ai/bin` (for MP4 videos with music) — only when you ask.

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

/// Opens a file with the system's player/viewer (WSL: the Windows one).
pub fn open(path: &Path) -> std::io::Result<()> {
    let wsl = std::env::var_os("WSL_DISTRO_NAME").is_some();
    if wsl {
        let win = std::process::Command::new("wslpath")
            .arg("-w")
            .arg(path)
            .output()?;
        let win = String::from_utf8_lossy(&win.stdout).trim().to_string();
        std::process::Command::new("explorer.exe")
            .arg(win)
            .spawn()?;
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    std::process::Command::new("cmd")
        .args(["/C", "start", ""])
        .arg(path)
        .spawn()?;
    #[cfg(target_os = "macos")]
    std::process::Command::new("open").arg(path).spawn()?;
    #[cfg(target_os = "linux")]
    std::process::Command::new("xdg-open").arg(path).spawn()?;
    Ok(())
}
