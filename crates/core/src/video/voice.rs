//! Narration audio: the draft voice (Piper when installed, else the
//! system's — Windows SAPI, also from WSL; macOS `say`; `espeak-ng`), your
//! recorded takes (`voice/<line>.take.wav`), and the small audio tools both
//! need: WAV in and out, resampling, trimming silence, and scoring a take
//! against its script.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use super::Line;

/// Mono 16-bit audio at `rate` Hz.
#[derive(Debug, Clone)]
pub struct Pcm {
    pub rate: u32,
    pub samples: Vec<i16>,
}

impl Pcm {
    pub fn seconds(&self) -> f64 {
        self.samples.len() as f64 / self.rate.max(1) as f64
    }
}

/// Reads a PCM WAV (16-bit or 32-bit float, any rate and channels) as mono.
pub fn read_wav(path: &Path) -> Result<Pcm, String> {
    let b = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if b.len() < 12 || &b[..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err(format!("{}: not a WAV file", path.display()));
    }
    let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    let (mut fmt, mut channels, mut rate, mut bits) = (0u16, 1u16, 0u32, 0u16);
    let mut i = 12;
    while i + 8 <= b.len() {
        let id = &b[i..i + 4];
        let len = u32_at(i + 4) as usize;
        let body = i + 8;
        let end = (body + len).min(b.len());
        if id == b"fmt " && len >= 16 {
            fmt = u16_at(body);
            channels = u16_at(body + 2).max(1);
            rate = u32_at(body + 4);
            bits = u16_at(body + 14);
            if fmt == 0xFFFE && len >= 26 {
                fmt = u16_at(body + 24);
            }
        } else if id == b"data" {
            let data = &b[body..end];
            let ch = channels as usize;
            let samples: Vec<i16> = match (fmt, bits) {
                (1, 16) => data
                    .chunks_exact(2 * ch)
                    .map(|f| {
                        let sum: i32 = f
                            .as_chunks::<2>()
                            .0
                            .iter()
                            .map(|s| i16::from_le_bytes(*s) as i32)
                            .sum();
                        (sum / ch as i32) as i16
                    })
                    .collect(),
                (3, 32) => data
                    .chunks_exact(4 * ch)
                    .map(|f| {
                        let sum: f32 = f
                            .as_chunks::<4>()
                            .0
                            .iter()
                            .map(|s| f32::from_le_bytes(*s))
                            .sum();
                        ((sum / ch as f32).clamp(-1.0, 1.0) * 32767.0) as i16
                    })
                    .collect(),
                _ => {
                    return Err(format!(
                        "{}: unsupported WAV ({bits}-bit, format {fmt})",
                        path.display()
                    ));
                }
            };
            if rate == 0 {
                return Err(format!("{}: no format chunk", path.display()));
            }
            return Ok(Pcm { rate, samples });
        }
        i = body + len + (len & 1);
    }
    Err(format!("{}: no audio data", path.display()))
}

/// Writes mono 16-bit samples as a WAV.
pub fn write_wav_mono(path: &Path, rate: u32, samples: &[i16]) -> std::io::Result<()> {
    let data_len = (samples.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + data_len as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes());
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        b.extend_from_slice(&s.to_le_bytes());
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, b)
}

/// Linear resampling to `to` Hz, as floats in −1…1.
pub fn resample(pcm: &Pcm, to: u32) -> Vec<f32> {
    let src: Vec<f32> = pcm.samples.iter().map(|&s| s as f32 / 32768.0).collect();
    if pcm.rate == to || src.is_empty() {
        return src;
    }
    let step = pcm.rate as f64 / to as f64;
    let n = (src.len() as f64 / step) as usize;
    (0..n)
        .map(|i| {
            let p = i as f64 * step;
            let j = p as usize;
            let k = (p - j as f64) as f32;
            let a = src[j.min(src.len() - 1)];
            let b = src[(j + 1).min(src.len() - 1)];
            a + (b - a) * k
        })
        .collect()
}

/// Cuts the silence before and after the speech (keeps a short margin).
pub fn trim_silence(pcm: &Pcm) -> Pcm {
    let frame = (pcm.rate as usize / 50).max(1); // 20 ms
    let levels: Vec<f32> = pcm
        .samples
        .chunks(frame)
        .map(|c| {
            let sum: f64 = c.iter().map(|&s| (s as f64).powi(2)).sum();
            (sum / c.len() as f64).sqrt() as f32
        })
        .collect();
    let peak = levels.iter().copied().fold(0.0, f32::max);
    let floor = (peak * 0.08).max(250.0);
    let first = levels.iter().position(|&l| l > floor);
    let last = levels.iter().rposition(|&l| l > floor);
    let (Some(a), Some(z)) = (first, last) else {
        return pcm.clone();
    };
    let pad = 8; // 160 ms
    let from = a.saturating_sub(pad) * frame;
    let to = ((z + 1 + pad) * frame).min(pcm.samples.len());
    Pcm {
        rate: pcm.rate,
        samples: pcm.samples[from..to].to_vec(),
    }
}

/// Words, lowercase, letters and digits only.
fn words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric() && c != '\'')
        .map(|w| w.trim_matches('\'').to_lowercase())
        .filter(|w| !w.is_empty())
        .collect()
}

/// How much of `script` was heard, 0–1 (words in order, so a skipped or
/// changed phrase lowers it).
pub fn score(script: &str, heard: &str) -> f64 {
    let a = words(script);
    let b = words(heard);
    if a.is_empty() {
        return 1.0;
    }
    let mut dp = vec![vec![0u16; b.len() + 1]; a.len() + 1];
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            dp[i][j] = if a[i - 1] == b[j - 1] {
                dp[i - 1][j - 1] + 1
            } else {
                dp[i - 1][j].max(dp[i][j - 1])
            };
        }
    }
    dp[a.len()][b.len()] as f64 / a.len() as f64
}

/// Takes below this sound unlike their line.
pub const HEARD_OK: f64 = 0.6;

pub fn voice_dir(dir: &Path) -> PathBuf {
    dir.join("voice")
}

pub fn take_path(dir: &Path, id: &str) -> PathBuf {
    voice_dir(dir).join(format!("{}.take.wav", super::clean_id(id)))
}

pub fn draft_path(dir: &Path, id: &str) -> PathBuf {
    voice_dir(dir).join(format!("{}.draft.wav", super::clean_id(id)))
}

fn on_path(name: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    })
}

fn dcode_dir(sub: &str) -> Option<PathBuf> {
    dcode_ai_common::config::home_dir().map(|h| h.join(".dcode-ai").join(sub))
}

fn piper_bin() -> Option<PathBuf> {
    let name = if cfg!(windows) { "piper.exe" } else { "piper" };
    dcode_dir("bin")
        .map(|d| d.join(name))
        .filter(|p| p.is_file())
        .or_else(|| {
            dcode_dir("bin/piper")
                .map(|d| d.join(name))
                .filter(|p| p.is_file())
        })
        .or_else(|| on_path(name))
}

fn piper_model(arg: &str) -> Option<PathBuf> {
    if !arg.is_empty() {
        let p = PathBuf::from(arg);
        return p.is_file().then_some(p);
    }
    let mut models: Vec<PathBuf> = std::fs::read_dir(dcode_dir("voices")?)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "onnx"))
        .collect();
    models.sort();
    models.into_iter().next()
}

fn is_wsl() -> bool {
    std::env::var_os("WSL_DISTRO_NAME").is_some()
        || std::fs::read_to_string("/proc/version")
            .is_ok_and(|v| v.to_ascii_lowercase().contains("microsoft"))
}

fn sapi_available() -> bool {
    cfg!(windows) || (is_wsl() && on_path("powershell.exe").is_some())
}

fn espeak_bin() -> Option<PathBuf> {
    on_path("espeak-ng").or_else(|| on_path("espeak"))
}

/// The draft voice to use for `spec` (`engine[:arg]`, or empty for the
/// first available): `(engine, arg)`.
pub fn pick_engine(spec: &str) -> Result<(String, String), String> {
    let (engine, arg) = spec
        .split_once(':')
        .map(|(a, b)| (a.trim().to_ascii_lowercase(), b.trim().to_string()))
        .unwrap_or_else(|| (spec.trim().to_ascii_lowercase(), String::new()));
    if !engine.is_empty() {
        return Ok((engine, arg));
    }
    if piper_bin().is_some() && piper_model("").is_some() {
        return Ok(("piper".into(), String::new()));
    }
    if cfg!(target_os = "macos") {
        return Ok(("say".into(), String::new()));
    }
    if sapi_available() {
        return Ok(("sapi".into(), String::new()));
    }
    if espeak_bin().is_some() {
        return Ok(("espeak".into(), String::new()));
    }
    Err(
        "no draft voice here — install Piper (with a voice in ~/.dcode-ai/voices) or espeak-ng, \
or record your own with /video voice"
            .into(),
    )
}

/// A file path as the program will see it (Windows paths for SAPI on WSL).
fn native_path(p: &Path) -> Result<String, String> {
    if cfg!(windows) || !is_wsl() {
        return Ok(p.display().to_string());
    }
    let o = Command::new("wslpath")
        .arg("-w")
        .arg(p)
        .output()
        .map_err(|e| format!("wslpath: {e}"))?;
    Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
}

fn run(mut cmd: Command, what: &str) -> Result<(), String> {
    let o = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("{what}: {e}"))?;
    if o.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{what}: {}",
            String::from_utf8_lossy(&o.stderr)
                .lines()
                .last()
                .unwrap_or("failed")
        ))
    }
}

/// Speaks `text` into the WAV `out` with `engine` (`arg`: a voice or a
/// model).
pub fn speak(engine: &str, arg: &str, text: &str, out: &Path) -> Result<(), String> {
    let dir = out.parent().ok_or("no folder")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let txt = out.with_extension("txt");
    std::fs::write(&txt, text).map_err(|e| e.to_string())?;
    let r = match engine {
        "sapi" => {
            let q = |s: &str| s.replace('\'', "''");
            let voice = if arg.is_empty() {
                String::new()
            } else {
                format!("$s.SelectVoice('{}');", q(arg))
            };
            let script = format!(
                "Add-Type -AssemblyName System.Speech; \
$s = New-Object System.Speech.Synthesis.SpeechSynthesizer; {voice} \
$f = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(22050, \
[System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen, [System.Speech.AudioFormat.AudioChannel]::Mono); \
$s.SetOutputToWaveFile('{}', $f); \
$s.Speak([IO.File]::ReadAllText('{}', [Text.Encoding]::UTF8)); $s.Dispose()",
                q(&native_path(out)?),
                q(&native_path(&txt)?)
            );
            let exe = if cfg!(windows) {
                "powershell"
            } else {
                "powershell.exe"
            };
            let mut c = Command::new(exe);
            c.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
            run(c, "Windows speech")
        }
        "say" => {
            let mut c = Command::new("say");
            c.arg("-o")
                .arg(out)
                .args(["--file-format=WAVE", "--data-format=LEI16@22050", "-f"])
                .arg(&txt);
            if !arg.is_empty() {
                c.args(["-v", arg]);
            }
            run(c, "say")
        }
        "piper" => {
            let bin = piper_bin().ok_or("piper isn't installed")?;
            let model = piper_model(arg)
                .ok_or("no Piper voice: put a .onnx voice (and its .json) in ~/.dcode-ai/voices")?;
            let mut child = Command::new(bin)
                .arg("--model")
                .arg(&model)
                .arg("--output_file")
                .arg(out)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .map_err(|e| format!("piper: {e}"))?;
            if let Some(mut sin) = child.stdin.take() {
                use std::io::Write;
                let _ = sin.write_all(text.replace('\n', " ").as_bytes());
            }
            let o = child.wait_with_output().map_err(|e| e.to_string())?;
            if o.status.success() {
                Ok(())
            } else {
                Err(format!(
                    "piper: {}",
                    String::from_utf8_lossy(&o.stderr)
                        .lines()
                        .last()
                        .unwrap_or("failed")
                ))
            }
        }
        "espeak" | "espeak-ng" => {
            let bin = espeak_bin().ok_or("espeak-ng isn't installed")?;
            let mut c = Command::new(bin);
            c.arg("-w").arg(out).arg("-f").arg(&txt);
            if !arg.is_empty() {
                c.args(["-v", arg]);
            }
            run(c, "espeak")
        }
        other => Err(format!(
            "unknown draft voice `{other}` — sapi, say, piper or espeak"
        )),
    };
    let _ = std::fs::remove_file(&txt);
    r?;
    if out.is_file() {
        Ok(())
    } else {
        Err(format!("{engine} made no audio"))
    }
}

/// The draft voice for `line`, made once and made again only when its text
/// or the voice changes.
pub fn draft(dir: &Path, line: &Line, spec: &str) -> Result<PathBuf, String> {
    let (engine, arg) = pick_engine(spec)?;
    let out = draft_path(dir, &line.id);
    let key_path = out.with_extension("key");
    let key = format!("{engine}:{arg}\n{}", line.text.trim());
    if out.is_file() && std::fs::read_to_string(&key_path).is_ok_and(|k| k == key) {
        return Ok(out);
    }
    speak(&engine, &arg, line.text.trim(), &out)?;
    let _ = std::fs::write(&key_path, key);
    Ok(out)
}

/// Plays a WAV and returns when it's done: Windows' player (also from
/// WSL), `afplay` on macOS, `paplay` or `aplay` on Linux.
pub fn play(path: &Path) -> Result<(), String> {
    let c = if cfg!(windows) || sapi_available() {
        let q = |s: &str| s.replace('\'', "''");
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
                "(New-Object Media.SoundPlayer '{}').PlaySync()",
                q(&native_path(path)?)
            ),
        ]);
        c
    } else if cfg!(target_os = "macos") {
        let mut c = Command::new("afplay");
        c.arg(path);
        c
    } else if let Some(p) = on_path("paplay").or_else(|| on_path("aplay")) {
        let mut c = Command::new(p);
        c.arg(path);
        c
    } else {
        return Err("no audio player here (paplay or aplay)".into());
    };
    run(c, "play")
}

/// Seconds a line takes to say, guessed from its words (no audio).
pub fn estimate_seconds(text: &str) -> f64 {
    words(text).len() as f64 * 0.38 + 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_round_trip_resample_and_trim() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        // 0.5 s silence, 0.5 s tone, 0.5 s silence at 16 kHz.
        let mut s = vec![0i16; 8000];
        s.extend((0..8000).map(|i| ((i as f32 * 0.2).sin() * 8000.0) as i16));
        s.extend(vec![0i16; 8000]);
        write_wav_mono(&p, 16000, &s).unwrap();
        let pcm = read_wav(&p).unwrap();
        assert_eq!((pcm.rate, pcm.samples.len()), (16000, 24000));
        assert_eq!(resample(&pcm, 44100).len(), 66150);
        let t = trim_silence(&pcm);
        assert!(t.seconds() > 0.5 && t.seconds() < 0.9, "{}", t.seconds());
    }

    #[test]
    fn takes_are_scored_against_their_line() {
        let line = "Click Create project, then name it Apollo.";
        assert_eq!(score(line, "click create project then name it apollo"), 1.0);
        assert!(score(line, "click create project") < HEARD_OK);
        assert!(
            score(
                line,
                "and now we click create project then we name it apollo"
            ) >= 0.99
        );
        assert!(score(line, "") == 0.0);
    }

    #[test]
    fn take_files_have_safe_names() {
        let d = Path::new("/v");
        assert_eq!(
            take_path(d, "../Intro 1"),
            Path::new("/v/voice/intro-1.take.wav")
        );
    }

    /// Speaks a line with this machine's draft voice:
    /// `cargo test -p dcode-ai-core -- --ignored draft_voice`.
    #[test]
    #[ignore]
    fn draft_voice_speaks() {
        let dir = tempfile::tempdir().unwrap();
        let line = Line {
            id: "intro-1".into(),
            at: 0.0,
            text: "Welcome. Let's create a project.".into(),
            section: String::new(),
            only: Vec::new(),
        };
        let p = draft(dir.path(), &line, "").unwrap();
        let pcm = read_wav(&p).unwrap();
        eprintln!(
            "{:?} {:.2}s at {} Hz",
            pick_engine(""),
            pcm.seconds(),
            pcm.rate
        );
        assert!(pcm.seconds() > 1.0);
        let modified = std::fs::metadata(&p).unwrap().modified().unwrap();
        draft(dir.path(), &line, "").unwrap();
        assert_eq!(
            std::fs::metadata(&p).unwrap().modified().unwrap(),
            modified,
            "cached"
        );
    }
}
