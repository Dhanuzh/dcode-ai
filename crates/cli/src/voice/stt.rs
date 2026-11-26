//! Speech to text on this machine: OpenAI's Whisper run by candle (pure
//! Rust, CPU). The model is downloaded once into `~/.dcode-ai/models/`;
//! audio never leaves the computer.
//!
//! `tiny.en` (quantized, ~40 MB) is the default; `base.en` (~290 MB) is
//! slower and better.

use std::path::PathBuf;
#[cfg(not(feature = "speech"))]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

#[cfg(feature = "speech")]
pub use super::whisper::Transcriber;

/// One downloadable model.
pub(super) struct Spec {
    pub(super) dir: &'static str,
    /// (file name on disk, URL).
    pub(super) files: [(&'static str, &'static str); 3],
    #[cfg_attr(not(feature = "speech"), allow(dead_code))]
    pub(super) quantized: bool,
}

pub(super) fn spec(name: &str) -> Spec {
    match name {
        "base.en" => Spec {
            dir: "whisper-base.en",
            files: [
                (
                    "model.safetensors",
                    "https://huggingface.co/openai/whisper-base.en/resolve/main/model.safetensors",
                ),
                (
                    "config.json",
                    "https://huggingface.co/openai/whisper-base.en/resolve/main/config.json",
                ),
                (
                    "tokenizer.json",
                    "https://huggingface.co/openai/whisper-base.en/resolve/main/tokenizer.json",
                ),
            ],
            quantized: false,
        },
        // Multilingual, bigger and better at languages like Tamil (downloaded
        // only when chosen with `[ui] stt_model`).
        "base" => Spec {
            dir: "whisper-base",
            files: [
                (
                    "model.safetensors",
                    "https://huggingface.co/openai/whisper-base/resolve/main/model.safetensors",
                ),
                (
                    "config.json",
                    "https://huggingface.co/openai/whisper-base/resolve/main/config.json",
                ),
                (
                    "tokenizer.json",
                    "https://huggingface.co/openai/whisper-base/resolve/main/tokenizer.json",
                ),
            ],
            quantized: false,
        },
        "small" => Spec {
            dir: "whisper-small",
            files: [
                (
                    "model.safetensors",
                    "https://huggingface.co/openai/whisper-small/resolve/main/model.safetensors",
                ),
                (
                    "config.json",
                    "https://huggingface.co/openai/whisper-small/resolve/main/config.json",
                ),
                (
                    "tokenizer.json",
                    "https://huggingface.co/openai/whisper-small/resolve/main/tokenizer.json",
                ),
            ],
            quantized: false,
        },
        // Multilingual: hears ~99 languages and says which it heard.
        "tiny" => Spec {
            dir: "whisper-tiny",
            files: [
                (
                    "model.gguf",
                    "https://huggingface.co/lmz/candle-whisper/resolve/main/model-tiny-q80.gguf",
                ),
                (
                    "config.json",
                    "https://huggingface.co/lmz/candle-whisper/resolve/main/config-tiny.json",
                ),
                (
                    "tokenizer.json",
                    "https://huggingface.co/lmz/candle-whisper/resolve/main/tokenizer-tiny.json",
                ),
            ],
            quantized: true,
        },
        _ => Spec {
            dir: "whisper-tiny.en",
            files: [
                (
                    "model.gguf",
                    "https://huggingface.co/lmz/candle-whisper/resolve/main/model-tiny-en-q80.gguf",
                ),
                (
                    "config.json",
                    "https://huggingface.co/lmz/candle-whisper/resolve/main/config-tiny-en.json",
                ),
                (
                    "tokenizer.json",
                    "https://huggingface.co/lmz/candle-whisper/resolve/main/tokenizer-tiny-en.json",
                ),
            ],
            quantized: true,
        },
    }
}

pub(super) fn models_dir() -> PathBuf {
    dcode_ai_common::config::home_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join(".dcode-ai/models")
}

/// Whether `name` is downloaded already.
pub fn is_downloaded(name: &str) -> bool {
    let s = spec(name);
    let dir = models_dir().join(s.dir);
    s.files.iter().all(|(f, _)| dir.join(f).is_file())
}

/// Downloads `name` if needed; `progress` gets short status lines.
pub async fn ensure_model(name: &str, progress: impl Fn(String)) -> Result<PathBuf, String> {
    let s = spec(name);
    let dir = models_dir().join(s.dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    for (file, url) in s.files {
        let path = dir.join(file);
        if path.is_file() {
            continue;
        }
        progress(format!("downloading the speech model ({name}): {file}…"));
        let bytes = reqwest::Client::new()
            .get(url)
            .header("User-Agent", "dcode-ai")
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| format!("download {file}: {e}"))?
            .bytes()
            .await
            .map_err(|e| format!("download {file}: {e}"))?;
        let tmp = dir.join(format!("{file}.part"));
        std::fs::write(&tmp, &bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &path).map_err(|e| e.to_string())?;
    }
    Ok(dir)
}

/// A language to hear instead of guessing it (`/talk lang ta`): guessing
/// is unreliable for some languages on small models.
pub(super) static FORCED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Sets (or clears) the language multilingual models listen for.
pub fn force_language(lang: Option<String>) {
    if let Ok(mut f) = FORCED.lock() {
        *f = lang;
    }
}

/// Whisper's languages, in its token order (the codes).
pub const LANGUAGES: &[&str] = &[
    "en", "zh", "de", "es", "ru", "ko", "fr", "ja", "pt", "tr", "pl", "ca", "nl", "ar", "sv", "it",
    "id", "hi", "fi", "vi", "he", "uk", "el", "ms", "cs", "ro", "da", "hu", "ta", "no", "th", "ur",
    "hr", "bg", "lt", "la", "mi", "ml", "cy", "sk", "te", "fa", "lv", "bn", "sr", "az", "sl", "kn",
    "et", "mk", "br", "eu", "is", "hy", "ne", "mn", "bs", "kk", "sq", "sw", "gl", "mr", "pa", "si",
    "km", "sn", "yo", "so", "af", "oc", "ka", "be", "tg", "sd", "gu", "am", "yi", "lo", "uz", "fo",
    "ht", "ps", "tk", "nn", "mt", "sa", "lb", "my", "bo", "tl", "mg", "as", "tt", "haw", "ln",
    "ha", "ba", "jw", "su",
];

/// A language code's English name, for asking the agent to answer in it.
pub fn language_name(code: &str) -> &str {
    match code {
        "en" => "English",
        "zh" => "Chinese",
        "de" => "German",
        "es" => "Spanish",
        "ru" => "Russian",
        "ko" => "Korean",
        "fr" => "French",
        "ja" => "Japanese",
        "pt" => "Portuguese",
        "tr" => "Turkish",
        "pl" => "Polish",
        "nl" => "Dutch",
        "ar" => "Arabic",
        "sv" => "Swedish",
        "it" => "Italian",
        "id" => "Indonesian",
        "hi" => "Hindi",
        "fi" => "Finnish",
        "vi" => "Vietnamese",
        "he" => "Hebrew",
        "uk" => "Ukrainian",
        "el" => "Greek",
        "cs" => "Czech",
        "ro" => "Romanian",
        "da" => "Danish",
        "hu" => "Hungarian",
        "ta" => "Tamil",
        "no" => "Norwegian",
        "th" => "Thai",
        "ur" => "Urdu",
        "te" => "Telugu",
        "ml" => "Malayalam",
        "kn" => "Kannada",
        "mr" => "Marathi",
        "bn" => "Bengali",
        "gu" => "Gujarati",
        "pa" => "Punjabi",
        "fa" => "Persian",
        "ms" => "Malay",
        "tl" => "Tagalog",
        other => other,
    }
}

/// Records from the microphone until `max` passes, or after speech `quiet`
/// of silence, or `stop` is set. 16 kHz mono samples.
pub async fn record(
    max: std::time::Duration,
    quiet: std::time::Duration,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<Vec<i16>, String> {
    use std::sync::atomic::Ordering;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
    let mixer = std::sync::Arc::new(std::sync::Mutex::new(super::mixer::Mixer::default()));
    let _io = super::audio::start(tx, mixer)?;
    let mut gate = super::vad::Gate::default();
    let mut pcm = Vec::new();
    let mut heard = false;
    let mut silent = std::time::Duration::ZERO;
    let frame_len = std::time::Duration::from_millis(20);
    let start = std::time::Instant::now();
    loop {
        if stop.load(Ordering::SeqCst) || start.elapsed() >= max {
            break;
        }
        let frame =
            match tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await {
                Ok(Some(f)) => f,
                Ok(None) => break,
                Err(_) => continue,
            };
        let speech = gate.open(&frame);
        pcm.extend_from_slice(&frame);
        if speech {
            heard = true;
            silent = std::time::Duration::ZERO;
        } else if heard {
            silent += frame_len;
            if silent >= quiet {
                break;
            }
        } else if start.elapsed() > std::time::Duration::from_secs(8) {
            // Nothing said at all.
            break;
        }
    }
    if !heard {
        return Err(if stop.load(Ordering::SeqCst) {
            "stopped".into()
        } else {
            "didn't hear anything — check the microphone".into()
        });
    }
    Ok(pcm)
}

/// A build without the `speech` feature: everything else about voice works
/// (recording, calls); turning speech into text says it isn't included.
#[cfg(not(feature = "speech"))]
pub struct Transcriber {
    pub language: Option<String>,
}

#[cfg(not(feature = "speech"))]
impl Transcriber {
    pub fn load(_name: &str) -> Result<Self, String> {
        Err("speech recognition isn't in this build (built without the `speech` feature)".into())
    }

    pub fn transcribe(&mut self, _pcm: &[i16]) -> Result<String, String> {
        Err("speech recognition isn't in this build".into())
    }

    pub fn transcribe_with_stop(
        &mut self,
        _pcm: &[i16],
        _stop: Option<&AtomicBool>,
    ) -> Result<String, String> {
        Err("speech recognition isn't in this build".into())
    }
}

/// The loaded model, kept for the next `/talk`.
static LOADED: std::sync::Mutex<Option<(String, Transcriber)>> = std::sync::Mutex::new(None);

/// Pre-load the STT model in the background when already downloaded.
/// Returns `Ok(true)` when a model is ready in memory.
pub async fn warm_up_if_downloaded(model: &str) -> Result<bool, String> {
    if !is_downloaded(model) {
        return Ok(false);
    }
    let name = model.to_string();
    tokio::task::spawn_blocking(move || {
        let mut slot = LOADED.lock().map_err(|e| e.to_string())?;
        if slot.as_ref().is_none_or(|(n, _)| *n != name) {
            *slot = Some((name.clone(), Transcriber::load(&name)?));
        }
        Ok::<bool, String>(true)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Transcribes 16 kHz mono samples with `model` (downloaded and loaded
/// the first time; kept loaded). `stop` cancels.
pub async fn transcribe_pcm(
    model: &str,
    pcm: Vec<i16>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<String, String> {
    if !is_downloaded(model) {
        ensure_model(model, |_| {}).await?;
    }
    let name = model.to_string();
    tokio::task::spawn_blocking(move || {
        let mut slot = LOADED.lock().map_err(|e| e.to_string())?;
        if slot.as_ref().is_none_or(|(n, _)| *n != name) {
            *slot = Some((name.clone(), Transcriber::load(&name)?));
        }
        let (_, t) = slot.as_mut().ok_or("no model")?;
        t.transcribe_with_stop(&pcm, Some(stop.as_ref()))
    })
    .await
    .map_err(|e| e.to_string())?
    .map(|t| clean(&t))
}

/// What [`listen`] heard.
pub struct Heard {
    pub text: String,
    /// The language heard (a multilingual model), e.g. `de`.
    pub language: Option<String>,
    /// Listening was ended with Esc ("done talking") rather than a pause.
    pub by_esc: bool,
}

/// Listens (until a pause of `quiet`, at most `max`, or Esc) and returns
/// what was said. Downloads and loads the model the first time.
///
/// `stop` is Esc: while listening it means "done talking" (what was said is
/// still transcribed); while transcribing it cancels.
pub async fn listen(
    model: &str,
    max: std::time::Duration,
    quiet: std::time::Duration,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    progress: impl Fn(String),
) -> Result<Heard, String> {
    if !is_downloaded(model) {
        ensure_model(model, &progress).await?;
    }
    progress("🎙 listening… (stops when you pause · Esc stops)".into());
    let started = std::time::Instant::now();
    let recording = record(max, quiet, stop.clone());
    tokio::pin!(recording);
    let recorded = loop {
        tokio::select! {
            r = &mut recording => break r,
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                let secs = started.elapsed().as_secs();
                progress(format!("🎙 listening… {secs}s (stops when you pause · Esc stops)"));
            }
        }
    };
    // Esc while listening ended the recording: clear it so a second Esc can
    // cancel the transcription.
    let by_esc = stop.swap(false, Ordering::SeqCst);
    let pcm = recorded?;
    if by_esc && pcm.len() < 16_000 / 2 {
        return Err("stopped".into());
    }
    progress(format!(
        "✎ transcribing {:.0}s of audio… (Esc cancels)",
        pcm.len() as f32 / 16_000.0
    ));
    let name = model.to_string();
    let stop_for_transcribe = stop.clone();
    let mut transcribe = tokio::task::spawn_blocking(move || {
        let mut slot = LOADED.lock().map_err(|e| e.to_string())?;
        if slot.as_ref().is_none_or(|(n, _)| *n != name) {
            *slot = Some((name.clone(), Transcriber::load(&name)?));
        }
        let (_, t) = slot.as_mut().ok_or("no model")?;
        t.language = None;
        let text = t.transcribe_with_stop(&pcm, Some(stop_for_transcribe.as_ref()))?;
        Ok::<_, String>((text, t.language.clone()))
    });
    let started = std::time::Instant::now();
    loop {
        tokio::select! {
            out = &mut transcribe => {
                let (text, language) = out.map_err(|e| e.to_string())??;
                return Ok(Heard { text: clean(&text), by_esc, language });
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                if stop.load(Ordering::SeqCst) {
                    progress("✎ transcribing… stopping".into());
                } else {
                    let secs = started.elapsed().as_secs();
                    progress(format!("✎ transcribing… {secs}s (Esc cancels)"));
                }
            }
        }
    }
}

/// Whisper's habits on near-silence ("you", "Thank you.") and stray marks.
pub fn clean(text: &str) -> String {
    let t = text.trim();
    let bare = t
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_ascii_lowercase();
    if matches!(
        bare.as_str(),
        "you" | "thank you" | "thanks for watching" | "bye"
    ) || t.starts_with('[') && t.ends_with(']')
    {
        return String::new();
    }
    t.to_string()
}

/// "stop", "stop talking", "that's all" … end a hands-free conversation.
pub fn is_stop(text: &str) -> bool {
    let bare: String = text
        .to_ascii_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .collect();
    matches!(
        bare.trim(),
        "stop"
            | "stop talking"
            | "stop listening"
            | "thats all"
            | "that is all"
            | "goodbye"
            | "exit"
            | "quit"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hallucinations_and_stop_words() {
        assert_eq!(clean(" you "), "");
        assert_eq!(clean("Thank you."), "");
        assert_eq!(clean("[BLANK_AUDIO]"), "");
        assert_eq!(clean(" Fix the login bug. "), "Fix the login bug.");
        assert!(is_stop("Stop."));
        assert!(is_stop("that's all"));
        assert!(!is_stop("stop the server and restart it"));
    }

    /// `listen` end to end with the fake microphone: finishing listening
    /// must not count as Esc (it used to, so every transcription "stopped").
    /// Needs the model downloaded: `cargo test -- --ignored listen_`.
    #[tokio::test]
    #[ignore]
    async fn listen_transcribes_after_a_pause_without_esc() {
        // SAFETY: test-only, set before the audio thread starts.
        unsafe { std::env::set_var("DCODE_AI_FAKE_AUDIO", "tone") };
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let r = listen(
            "tiny.en",
            std::time::Duration::from_secs(2),
            std::time::Duration::from_millis(300),
            stop.clone(),
            |s| eprintln!("{s}"),
        )
        .await;
        assert!(
            r.as_ref().is_ok_and(|h| !h.by_esc),
            "{:?}",
            r.as_ref().err()
        );
        assert!(
            !stop.load(std::sync::atomic::Ordering::SeqCst),
            "nobody pressed Esc"
        );
    }

    /// Esc while listening means "done talking": what was heard is still
    /// transcribed, and the flag is cleared so a second Esc can cancel.
    #[tokio::test]
    #[ignore]
    async fn listen_esc_while_talking_still_transcribes() {
        // SAFETY: test-only, set before the audio thread starts.
        unsafe { std::env::set_var("DCODE_AI_FAKE_AUDIO", "tone") };
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let esc = stop.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(1200)).await;
            esc.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let r = listen(
            "tiny.en",
            std::time::Duration::from_secs(30),
            std::time::Duration::from_secs(5),
            stop.clone(),
            |s| eprintln!("{s}"),
        )
        .await;
        assert!(r.as_ref().is_ok_and(|h| h.by_esc), "{:?}", r.as_ref().err());
        assert!(!stop.load(std::sync::atomic::Ordering::SeqCst));
    }

    /// Downloads tiny.en and transcribes a generated tone (no words): runs
    /// end to end without failing. `cargo test -- --ignored stt`.
    #[tokio::test]
    #[ignore]
    async fn the_multilingual_model_hears_the_language() {
        // A 16 kHz mono WAV of someone speaking German.
        let Ok(path) = std::env::var("DCODE_AI_TEST_WAV_DE") else {
            return;
        };
        ensure_model("tiny", |s| eprintln!("{s}")).await.unwrap();
        let mut t = Transcriber::load("tiny").unwrap();
        let raw = std::fs::read(path).unwrap();
        let data = raw.windows(4).position(|w| w == b"data").unwrap() + 8;
        let pcm: Vec<i16> = raw[data..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| i16::from_le_bytes(*c))
            .collect();
        let text = t.transcribe(&pcm).unwrap();
        eprintln!("{:?} → {text:?}", t.language);
        assert_eq!(t.language.as_deref(), Some("de"));
        assert!(text.to_lowercase().contains("berlin"), "{text}");
    }

    #[tokio::test]
    #[ignore]
    async fn real_model_runs() {
        ensure_model("tiny.en", |s| eprintln!("{s}")).await.unwrap();
        let mut t = Transcriber::load("tiny.en").unwrap();
        let pcm: Vec<i16> = (0..16000 * 2)
            .map(|i| ((i as f32 * 0.05).sin() * 3000.0) as i16)
            .collect();
        let text = t.transcribe(&pcm).unwrap();
        eprintln!("tone → {text:?}");
        // A real recording (16 kHz mono 16-bit WAV) from $DCODE_AI_TEST_WAV.
        if let Ok(path) = std::env::var("DCODE_AI_TEST_WAV") {
            let raw = std::fs::read(path).unwrap();
            let data = raw.windows(4).position(|w| w == b"data").unwrap() + 8;
            let pcm: Vec<i16> = raw[data..]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| i16::from_le_bytes(*c))
                .collect();
            let started = std::time::Instant::now();
            let text = t.transcribe(&pcm).unwrap();
            eprintln!(
                "wav ({:.1}s audio, {:?}) → {text:?}",
                pcm.len() as f32 / 16000.0,
                started.elapsed()
            );
            assert!(!text.is_empty());
        }
    }
}
