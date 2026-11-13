//! Speech to text on this machine: OpenAI's Whisper run by candle (pure
//! Rust, CPU). The model is downloaded once into `~/.dcode-ai/models/`;
//! audio never leaves the computer.
//!
//! `tiny.en` (quantized, ~40 MB) is the default; `base.en` (~290 MB) is
//! slower and better.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use candle_core::{D, Device, IndexOp, Tensor};
use candle_transformers::models::whisper::{self as w, Config};

/// The Mel filters for 80 bins (Whisper's own, from candle's example).
const MEL_80: &[u8] = include_bytes!("melfilters.bytes");

/// One downloadable model.
struct Spec {
    dir: &'static str,
    /// (file name on disk, URL).
    files: [(&'static str, &'static str); 3],
    quantized: bool,
}

fn spec(name: &str) -> Spec {
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

fn models_dir() -> PathBuf {
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

/// Whisper's byte-level BPE, decode side only.
struct Tokens {
    text: HashMap<u32, String>,
    special: HashMap<String, u32>,
    byte_of: HashMap<char, u8>,
}

impl Tokens {
    fn load(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let v: serde_json::Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
        let mut text = HashMap::new();
        if let Some(vocab) = v["model"]["vocab"].as_object() {
            for (t, id) in vocab {
                if let Some(id) = id.as_u64() {
                    text.insert(id as u32, t.clone());
                }
            }
        }
        let mut special = HashMap::new();
        for a in v["added_tokens"].as_array().into_iter().flatten() {
            if let (Some(id), Some(c)) = (a["id"].as_u64(), a["content"].as_str()) {
                special.insert(c.to_string(), id as u32);
            }
        }
        // GPT-2's bytes ↔ printable characters table.
        let mut bs: Vec<u32> = (b'!' as u32..=b'~' as u32)
            .chain(0xA1..=0xAC)
            .chain(0xAE..=0xFF)
            .collect();
        let mut cs = bs.clone();
        let mut n = 0;
        for b in 0..256u32 {
            if !bs.contains(&b) {
                bs.push(b);
                cs.push(256 + n);
                n += 1;
            }
        }
        let byte_of = bs
            .iter()
            .zip(cs)
            .filter_map(|(&b, c)| char::from_u32(c).map(|c| (c, b as u8)))
            .collect();
        Ok(Self {
            text,
            special,
            byte_of,
        })
    }

    fn id(&self, token: &str) -> Result<u32, String> {
        self.special
            .get(token)
            .copied()
            .ok_or_else(|| format!("the speech model has no `{token}` token"))
    }

    /// Text for these ids, special tokens left out.
    fn decode(&self, ids: &[u32]) -> String {
        let specials: std::collections::HashSet<u32> = self.special.values().copied().collect();
        let mut bytes = Vec::new();
        for id in ids {
            if specials.contains(id) {
                continue;
            }
            if let Some(t) = self.text.get(id) {
                for ch in t.chars() {
                    if let Some(b) = self.byte_of.get(&ch) {
                        bytes.push(*b);
                    }
                }
            }
        }
        String::from_utf8_lossy(&bytes).trim().to_string()
    }
}

enum Model {
    Quantized(w::quantized_model::Whisper),
    Float(w::model::Whisper),
}

impl Model {
    fn encode(&mut self, mel: &Tensor) -> candle_core::Result<Tensor> {
        match self {
            Model::Quantized(m) => m.encoder.forward(mel, true),
            Model::Float(m) => m.encoder.forward(mel, true),
        }
    }
    fn decode(
        &mut self,
        tokens: &Tensor,
        audio: &Tensor,
        flush: bool,
    ) -> candle_core::Result<Tensor> {
        match self {
            Model::Quantized(m) => m.decoder.forward(tokens, audio, flush),
            Model::Float(m) => m.decoder.forward(tokens, audio, flush),
        }
    }
    fn logits(&self, x: &Tensor) -> candle_core::Result<Tensor> {
        match self {
            Model::Quantized(m) => m.decoder.final_linear(x),
            Model::Float(m) => m.decoder.final_linear(x),
        }
    }
}

/// A loaded model, ready to transcribe.
pub struct Transcriber {
    model: Model,
    config: Config,
    tokens: Tokens,
    mel_filters: Vec<f32>,
    suppress: Tensor,
    sot: u32,
    eot: u32,
    no_timestamps: u32,
    device: Device,
}

impl Transcriber {
    /// Loads `name` (downloaded with [`ensure_model`]).
    pub fn load(name: &str) -> Result<Self, String> {
        let s = spec(name);
        let dir = models_dir().join(s.dir);
        let device = Device::Cpu;
        let config: Config = serde_json::from_str(
            &std::fs::read_to_string(dir.join("config.json")).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        if config.num_mel_bins != 80 {
            return Err(format!(
                "{name} needs {} Mel bins; only 80 are built in",
                config.num_mel_bins
            ));
        }
        let model = if s.quantized {
            let vb = candle_transformers::quantized_var_builder::VarBuilder::from_gguf(
                dir.join("model.gguf"),
                &device,
            )
            .map_err(|e| e.to_string())?;
            Model::Quantized(
                w::quantized_model::Whisper::load(&vb, config.clone())
                    .map_err(|e| e.to_string())?,
            )
        } else {
            // SAFETY: the file is ours and not changed while mapped.
            let vb = unsafe {
                candle_nn::VarBuilder::from_mmaped_safetensors(
                    &[dir.join("model.safetensors")],
                    w::DTYPE,
                    &device,
                )
            }
            .map_err(|e| e.to_string())?;
            Model::Float(w::model::Whisper::load(&vb, config.clone()).map_err(|e| e.to_string())?)
        };
        let tokens = Tokens::load(&dir.join("tokenizer.json"))?;
        let sot = tokens.id(w::SOT_TOKEN)?;
        let eot = tokens.id(w::EOT_TOKEN)?;
        let no_timestamps = tokens.id(w::NO_TIMESTAMPS_TOKEN)?;
        let suppress: Vec<f32> = (0..config.vocab_size as u32)
            .map(|i| {
                if config.suppress_tokens.contains(&i) || i == no_timestamps {
                    f32::NEG_INFINITY
                } else {
                    0.0
                }
            })
            .collect();
        let suppress = Tensor::new(suppress.as_slice(), &device).map_err(|e| e.to_string())?;
        let mel_filters: Vec<f32> = MEL_80
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect();
        Ok(Self {
            model,
            config,
            tokens,
            mel_filters,
            suppress,
            sot,
            eot,
            no_timestamps,
            device,
        })
    }

    /// Text for 16 kHz mono samples (any length; 30 s at a time).
    pub fn transcribe(&mut self, pcm: &[i16]) -> Result<String, String> {
        self.transcribe_with_stop(pcm, None)
    }

    /// Like [`Self::transcribe`], but stops early when `stop` is set.
    pub fn transcribe_with_stop(
        &mut self,
        pcm: &[i16],
        stop: Option<&AtomicBool>,
    ) -> Result<String, String> {
        if stop.is_some_and(|s| s.load(Ordering::SeqCst)) {
            return Err("stopped".into());
        }
        let samples: Vec<f32> = pcm.iter().map(|&s| s as f32 / 32768.0).collect();
        let mel = w::audio::pcm_to_mel(&self.config, &samples, &self.mel_filters);
        let bins = self.config.num_mel_bins;
        let frames = mel.len() / bins;
        let mel =
            Tensor::from_vec(mel, (1, bins, frames), &self.device).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        let mut seek = 0;
        while seek < frames {
            if stop.is_some_and(|s| s.load(Ordering::SeqCst)) {
                return Err("stopped".into());
            }
            let len = (frames - seek).min(w::N_FRAMES);
            let seg = mel.narrow(2, seek, len).map_err(|e| e.to_string())?;
            out.push(self.segment(&seg, stop)?);
            seek += len;
        }
        Ok(out.join(" ").trim().to_string())
    }

    fn segment(&mut self, mel: &Tensor, stop: Option<&AtomicBool>) -> Result<String, String> {
        let audio = self.model.encode(mel).map_err(|e| e.to_string())?;
        let mut tokens = vec![self.sot, self.no_timestamps];
        let limit = self.config.max_target_positions / 2;
        for i in 0..limit {
            if stop.is_some_and(|s| s.load(Ordering::SeqCst)) {
                return Err("stopped".into());
            }
            let t = Tensor::new(tokens.as_slice(), mel.device())
                .map_err(|e| e.to_string())?
                .unsqueeze(0)
                .map_err(|e| e.to_string())?;
            let ys = self
                .model
                .decode(&t, &audio, i == 0)
                .map_err(|e| e.to_string())?;
            let (_, seq, _) = ys.dims3().map_err(|e| e.to_string())?;
            let logits = self
                .model
                .logits(&ys.i((..1, seq - 1..)).map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?
                .i(0)
                .map_err(|e| e.to_string())?
                .i(0)
                .map_err(|e| e.to_string())?;
            let logits = logits
                .broadcast_add(&self.suppress)
                .map_err(|e| e.to_string())?;
            let v: Vec<f32> = logits.to_vec1().map_err(|e| e.to_string())?;
            let next = v
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map(|(i, _)| i as u32)
                .unwrap_or(self.eot);
            if next == self.eot || tokens.len() > self.config.max_target_positions {
                break;
            }
            tokens.push(next);
        }
        let _ = D::Minus1;
        Ok(self.tokens.decode(&tokens))
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

/// Listens (until a pause of `quiet`, at most `max`) and returns what was
/// said. Downloads and loads the model the first time.
pub async fn listen(
    model: &str,
    max: std::time::Duration,
    quiet: std::time::Duration,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    progress: impl Fn(String),
) -> Result<String, String> {
    if !is_downloaded(model) {
        ensure_model(model, &progress).await?;
    }
    progress("🎙 listening… (stops when you pause · Esc stops)".into());
    let recorded = record(max, quiet, stop.clone()).await;
    // Done listening: Esc goes back to its usual job.
    let stopped = stop.swap(true, std::sync::atomic::Ordering::SeqCst);
    let pcm = recorded?;
    if stopped && pcm.len() < 16_000 / 2 {
        return Err("stopped".into());
    }
    progress("✎ transcribing…".into());
    let name = model.to_string();
    let stop_for_transcribe = stop.clone();
    let mut transcribe = tokio::task::spawn_blocking(move || {
        let mut slot = LOADED.lock().map_err(|e| e.to_string())?;
        if slot.as_ref().is_none_or(|(n, _)| *n != name) {
            *slot = Some((name.clone(), Transcriber::load(&name)?));
        }
        let (_, t) = slot.as_mut().ok_or("no model")?;
        t.transcribe_with_stop(&pcm, Some(stop_for_transcribe.as_ref()))
    });
    let started = std::time::Instant::now();
    loop {
        tokio::select! {
            out = &mut transcribe => {
                return out
                    .map_err(|e| e.to_string())?
                    .map(|t| clean(&t));
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                if stop.load(Ordering::SeqCst) {
                    progress("✎ transcribing… stopping".into());
                } else {
                    let secs = started.elapsed().as_secs();
                    if secs > 0 {
                        progress(format!("✎ transcribing… {secs}s"));
                    }
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

    #[test]
    fn mel_filters_are_whispers_80_bins() {
        // 80 bins × 201 FFT frequencies, f32.
        assert_eq!(MEL_80.len(), 80 * 201 * 4);
    }

    #[test]
    fn byte_level_tokens_decode() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.json");
        // "Ġ" is GPT-2's printable stand-in for a space.
        std::fs::write(
            &p,
            r#"{"model":{"vocab":{"Hello":0,"Ġworld":1,"!":2}},"added_tokens":[{"id":3,"content":"<|endoftext|>"}]}"#,
        )
        .unwrap();
        let t = Tokens::load(&p).unwrap();
        assert_eq!(t.decode(&[0, 1, 2, 3]), "Hello world!");
        assert_eq!(t.id("<|endoftext|>").unwrap(), 3);
    }

    /// Downloads tiny.en and transcribes a generated tone (no words): runs
    /// end to end without failing. `cargo test -- --ignored stt`.
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
