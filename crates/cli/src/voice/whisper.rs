//! Whisper itself (candle, CPU): the model, its tokenizer and decoding.
//! Built with the `speech` feature (on by default); without it,
//! `stt::Transcriber` says speech recognition isn't in this build.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use candle_core::{D, Device, IndexOp, Tensor};
use candle_transformers::models::whisper::{self as w, Config};

use super::stt::{FORCED, LANGUAGES, models_dir, spec};

/// The Mel filters for 80 bins (Whisper's own, from candle's example).
const MEL_80: &[u8] = include_bytes!("melfilters.bytes");

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
    /// A multilingual model: its language tokens, and `<|transcribe|>`.
    languages: Vec<(&'static str, u32)>,
    transcribe_token: Option<u32>,
    /// The language heard last (multilingual models).
    pub language: Option<String>,
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
        let languages: Vec<(&'static str, u32)> = LANGUAGES
            .iter()
            .filter_map(|l| tokens.id(&format!("<|{l}|>")).ok().map(|id| (*l, id)))
            .collect();
        let transcribe_token = tokens.id("<|transcribe|>").ok();
        // English-only models have no language tokens to choose from.
        let multilingual = !name.ends_with(".en") && languages.len() > 50;
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
            languages: if multilingual { languages } else { Vec::new() },
            transcribe_token,
            language: None,
        })
    }

    /// The most likely language of the audio (multilingual models).
    fn detect_language(&mut self, audio: &Tensor) -> Result<Option<(&'static str, u32)>, String> {
        if self.languages.is_empty() {
            return Ok(None);
        }
        let t = Tensor::new(&[self.sot][..], audio.device())
            .map_err(|e| e.to_string())?
            .unsqueeze(0)
            .map_err(|e| e.to_string())?;
        let ys = self
            .model
            .decode(&t, audio, true)
            .map_err(|e| e.to_string())?;
        let logits = self
            .model
            .logits(&ys.i((..1, ..1)).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?
            .i(0)
            .map_err(|e| e.to_string())?
            .i(0)
            .map_err(|e| e.to_string())?;
        let v: Vec<f32> = logits.to_vec1().map_err(|e| e.to_string())?;
        Ok(self
            .languages
            .iter()
            .copied()
            .max_by(|a, b| v[a.1 as usize].total_cmp(&v[b.1 as usize])))
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
        let mut tokens = vec![self.sot];
        // A multilingual model: which language, then "transcribe" (not
        // translate) in it.
        let forced = FORCED
            .lock()
            .ok()
            .and_then(|f| f.clone())
            .and_then(|f| self.languages.iter().find(|(c, _)| *c == f).copied());
        let picked = match forced {
            Some(l) => Some(l),
            None => self.detect_language(&audio)?,
        };
        if let Some((code, id)) = picked {
            self.language = Some(code.to_string());
            tokens.push(id);
            if let Some(t) = self.transcribe_token {
                tokens.push(t);
            }
        }
        tokens.push(self.no_timestamps);
        let prompt_len = tokens.len();
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
            if repeating(&tokens[prompt_len..]) {
                break;
            }
        }
        let _ = D::Minus1;
        Ok(self.tokens.decode(&tokens))
    }
}

/// Whisper on noise can loop on the same few tokens until the limit;
/// stop once the tail repeats (a 1–8 token phrase, four times running).
fn repeating(t: &[u32]) -> bool {
    (1..=8).any(|n| {
        t.len() >= n * 4 && {
            let tail = &t[t.len() - n..];
            (2..=4).all(|k| &t[t.len() - n * k..t.len() - n * (k - 1)] == tail)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeat_loops_are_caught() {
        assert!(!repeating(&[1, 2, 3, 4, 5, 6]));
        assert!(repeating(&[9, 7, 7, 7, 7]));
        assert!(repeating(&[1, 5, 6, 5, 6, 5, 6, 5, 6]));
        assert!(!repeating(&[5, 6, 5, 6, 5, 6]));
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
}
