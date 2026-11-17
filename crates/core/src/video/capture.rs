//! Capturing what isn't the browser: the whole screen (desktop apps, the
//! terminal) and a webcam, with ffmpeg — gdigrab on Windows, avfoundation
//! on macOS, x11grab on Linux. On WSL, where the Linux ffmpeg can't see the
//! Windows desktop, the screen is captured by a PowerShell loop instead.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// Frames taken from a screen recording, per second.
pub const SCREEN_FPS: f64 = 8.0;

/// A capture that is running.
pub struct Running {
    child: Child,
    /// What it writes: an MP4 file, or a folder of timestamped JPEGs (WSL).
    pub out: PathBuf,
    /// Recording second it started at.
    pub at: f64,
    /// A file whose existence tells the PowerShell loop to stop (WSL).
    stop_file: Option<PathBuf>,
}

fn is_wsl() -> bool {
    std::env::var_os("WSL_DISTRO_NAME").is_some()
        || std::fs::read_to_string("/proc/version")
            .is_ok_and(|v| v.to_ascii_lowercase().contains("microsoft"))
}

fn encode_args() -> [&'static str; 8] {
    [
        "-c:v",
        "libx264",
        "-preset",
        "ultrafast",
        "-crf",
        "23",
        "-pix_fmt",
        "yuv420p",
    ]
}

/// The ffmpeg input for the whole screen here, if there is one.
fn screen_input() -> Result<Vec<String>, String> {
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    if cfg!(windows) {
        return Ok(v(&["-f", "gdigrab", "-framerate", "15", "-i", "desktop"]));
    }
    if cfg!(target_os = "macos") {
        return Ok(v(&[
            "-f",
            "avfoundation",
            "-capture_cursor",
            "1",
            "-framerate",
            "15",
            "-i",
            "Capture screen 0:none",
        ]));
    }
    match std::env::var("DISPLAY") {
        Ok(d) if !d.is_empty() => Ok(v(&["-f", "x11grab", "-framerate", "15", "-i", &d])),
        _ => Err(
            "no screen to record here (no DISPLAY; Wayland-only desktops aren't supported)".into(),
        ),
    }
}

/// Starts recording the whole screen into `dir` (`screen.mp4`, or
/// `screen/` on WSL). `at`: the recording second now.
pub fn start_screen(dir: &Path, at: f64) -> Result<Running, String> {
    if is_wsl() && !cfg!(windows) {
        return start_wsl_screen(dir, at);
    }
    let bin = super::ffmpeg().ok_or("recording the screen needs ffmpeg — /video setup")?;
    let out = dir.join("screen.mp4");
    let child = Command::new(bin)
        .args(["-loglevel", "error", "-y"])
        .args(screen_input()?)
        .args(encode_args())
        .arg(&out)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("couldn't start ffmpeg: {e}"))?;
    Ok(Running {
        child,
        out,
        at,
        stop_file: None,
    })
}

/// The Windows desktop from WSL: PowerShell takes about five screenshots a
/// second, each named with its millisecond offset.
fn start_wsl_screen(dir: &Path, at: f64) -> Result<Running, String> {
    let out = dir.join("screen");
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let stop = dir.join("screen.stop");
    let _ = std::fs::remove_file(&stop);
    let win = |p: &Path| -> Result<String, String> {
        let o = Command::new("wslpath")
            .arg("-w")
            .arg(p)
            .output()
            .map_err(|e| e.to_string())?;
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let script = format!(
        r#"
Add-Type -AssemblyName System.Windows.Forms,System.Drawing
Add-Type -Name D -Namespace W -MemberDefinition '[DllImport("user32.dll")] public static extern bool SetProcessDPIAware();'
[void][W.D]::SetProcessDPIAware()
$dir = '{dir}'; $stop = '{stop}'
$b = [System.Windows.Forms.SystemInformation]::VirtualScreen
$enc = [System.Drawing.Imaging.ImageCodecInfo]::GetImageEncoders() | Where-Object {{ $_.MimeType -eq 'image/jpeg' }}
$p = New-Object System.Drawing.Imaging.EncoderParameters 1
$p.Param[0] = New-Object System.Drawing.Imaging.EncoderParameter ([System.Drawing.Imaging.Encoder]::Quality), ([long]85)
$sw = [Diagnostics.Stopwatch]::StartNew(); $i = 0
while (-not (Test-Path $stop) -and $sw.ElapsedMilliseconds -lt 600000) {{
  $t = $sw.ElapsedMilliseconds
  $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($b.Left, $b.Top, 0, 0, $bmp.Size)
  $i++
  $bmp.Save((Join-Path $dir ('s{{0:D6}}_{{1}}.jpg' -f $i, $t)), $enc, $p)
  $g.Dispose(); $bmp.Dispose()
  $wait = 200 - ($sw.ElapsedMilliseconds - $t); if ($wait -gt 0) {{ Start-Sleep -Milliseconds $wait }}
}}
"#,
        dir = win(&out)?.replace('\'', "''"),
        stop = win(&stop)?.replace('\'', "''"),
    );
    let child = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("couldn't start PowerShell to record the Windows screen: {e}"))?;
    let mut r = Running {
        child,
        out,
        at,
        stop_file: Some(stop),
    };
    if let Some(mut stdin) = r.child.stdin.take() {
        stdin
            .write_all(script.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    Ok(r)
}

/// The ffmpeg input for the default webcam.
fn webcam_input(bin: &Path) -> Result<Vec<String>, String> {
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    if cfg!(target_os = "macos") {
        return Ok(v(&[
            "-f",
            "avfoundation",
            "-framerate",
            "30",
            "-i",
            "0:none",
        ]));
    }
    if cfg!(windows) {
        // The first video device DirectShow lists.
        let o = Command::new(bin)
            .args([
                "-hide_banner",
                "-list_devices",
                "true",
                "-f",
                "dshow",
                "-i",
                "dummy",
            ])
            .output()
            .map_err(|e| e.to_string())?;
        let text = String::from_utf8_lossy(&o.stderr).to_string();
        let name = text
            .lines()
            .filter(|l| l.contains("(video)"))
            .find_map(|l| l.split('"').nth(1).map(str::to_string))
            .ok_or("no webcam found")?;
        return Ok(vec![
            "-f".into(),
            "dshow".into(),
            "-i".into(),
            format!("video={name}"),
        ]);
    }
    if is_wsl() {
        return Err("a webcam isn't reachable from WSL — record it on Windows and set `webcam` in edl.json to the clip".into());
    }
    if Path::new("/dev/video0").exists() {
        return Ok(v(&["-f", "v4l2", "-i", "/dev/video0"]));
    }
    Err("no webcam found (/dev/video0)".into())
}

/// Starts recording the webcam into `dir/webcam.mp4`.
pub fn start_webcam(dir: &Path, at: f64) -> Result<Running, String> {
    let bin = super::ffmpeg().ok_or("the webcam needs ffmpeg — /video setup")?;
    let input = webcam_input(&bin)?;
    let out = dir.join("webcam.mp4");
    let child = Command::new(&bin)
        .args(["-loglevel", "error", "-y"])
        .args(input)
        .args(["-vf", "scale=-2:480"])
        .args(encode_args())
        .arg(&out)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("couldn't start ffmpeg: {e}"))?;
    Ok(Running {
        child,
        out,
        at,
        stop_file: None,
    })
}

impl Running {
    /// Stops it and waits for the file to be finished (at most ~10 s).
    pub fn stop(mut self) {
        if let Some(stop) = &self.stop_file {
            let _ = std::fs::write(stop, b"");
        } else if let Some(mut stdin) = self.child.stdin.take() {
            // ffmpeg finishes the file on `q`.
            let _ = stdin.write_all(b"q");
        }
        for _ in 0..100 {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(stop) = &self.stop_file {
            let _ = std::fs::remove_file(stop);
        }
    }

    /// Whether it ended on its own already (it failed to start).
    pub fn failed_early(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }
}

/// The frames of a finished screen capture as `(t, file)` in `raw/`
/// (recording seconds), at most [`SCREEN_FPS`] a second.
pub fn screen_frames(cap_out: &Path, raw: &Path, at: f64) -> Result<Vec<(f64, String)>, String> {
    if cap_out.is_dir() {
        // WSL: s000001_<ms>.jpg
        let mut shots: Vec<(f64, PathBuf)> = std::fs::read_dir(cap_out)
            .map_err(|e| e.to_string())?
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                let stem = p.file_stem()?.to_str()?.to_string();
                let ms: f64 = stem.split('_').nth(1)?.parse().ok()?;
                Some((ms / 1000.0, p))
            })
            .collect();
        shots.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut out = Vec::new();
        for (i, (t, p)) in shots.iter().enumerate() {
            let file = format!("f{:06}.jpg", i + 1);
            std::fs::rename(p, raw.join(&file))
                .or_else(|_| std::fs::copy(p, raw.join(&file)).map(|_| ()))
                .map_err(|e| e.to_string())?;
            out.push((at + t, file));
        }
        let _ = std::fs::remove_dir_all(cap_out);
        return Ok(out);
    }
    let bin = super::ffmpeg().ok_or("ffmpeg is needed")?;
    let ok = Command::new(bin)
        .args(["-loglevel", "error", "-y", "-i"])
        .arg(cap_out)
        .args(["-vf", &format!("fps={SCREEN_FPS}"), "-q:v", "3"])
        .arg(raw.join("f%06d.jpg"))
        .stdin(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        return Err("couldn't read the screen recording".into());
    }
    let mut files: Vec<String> = std::fs::read_dir(raw)
        .map_err(|e| e.to_string())?
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(str::to_string))
        .filter(|n| n.ends_with(".jpg"))
        .collect();
    files.sort();
    Ok(files
        .into_iter()
        .enumerate()
        .map(|(i, f)| (at + i as f64 / SCREEN_FPS, f))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wsl_shots_become_timed_frames() {
        let d = tempfile::tempdir().unwrap();
        let cap = d.path().join("screen");
        let raw = d.path().join("raw");
        std::fs::create_dir_all(&cap).unwrap();
        std::fs::create_dir_all(&raw).unwrap();
        std::fs::write(cap.join("s000002_450.jpg"), b"b").unwrap();
        std::fs::write(cap.join("s000001_0.jpg"), b"a").unwrap();
        let f = screen_frames(&cap, &raw, 1.0).unwrap();
        assert_eq!(
            f,
            vec![(1.0, "f000001.jpg".into()), (1.45, "f000002.jpg".into())]
        );
        assert_eq!(std::fs::read(raw.join("f000001.jpg")).unwrap(), b"a");
        assert!(!cap.exists());
    }
}
