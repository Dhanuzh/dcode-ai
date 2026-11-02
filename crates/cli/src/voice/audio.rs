//! Microphone in, speakers out, at 16 kHz mono 16-bit.
//!
//! - Linux (and WSLg): the PulseAudio protocol in pure Rust — works with
//!   PulseAudio and PipeWire, and links no audio library, so dcode-ai still
//!   starts on machines without sound.
//! - macOS and Windows: cpal (CoreAudio / WASAPI).
//! - `DCODE_AI_FAKE_AUDIO=tone|silence`: no sound card (tests, CI).

use super::mixer::Mixer;
use super::{FRAME, RATE};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::UnboundedSender;

/// Running audio; dropping it closes the microphone and the speakers.
pub struct AudioIo {
    stop: Arc<AtomicBool>,
    #[allow(dead_code)]
    backend: &'static str,
}

impl AudioIo {
    pub fn backend(&self) -> &'static str {
        self.backend
    }
}

impl Drop for AudioIo {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

/// Turns arbitrary-sized chunks of samples into 20 ms frames.
pub(crate) struct Framer {
    buf: Vec<i16>,
    out: UnboundedSender<Vec<i16>>,
}

impl Framer {
    pub(crate) fn new(out: UnboundedSender<Vec<i16>>) -> Self {
        Self {
            buf: Vec::with_capacity(FRAME * 2),
            out,
        }
    }
    pub(crate) fn push(&mut self, samples: impl IntoIterator<Item = i16>) {
        for s in samples {
            self.buf.push(s);
            if self.buf.len() == FRAME {
                let _ = self
                    .out
                    .send(std::mem::replace(&mut self.buf, Vec::with_capacity(FRAME)));
            }
        }
    }
}

/// Start the microphone (frames to `capture`) and the speakers (from
/// `mixer`).
pub fn start(
    capture: UnboundedSender<Vec<i16>>,
    mixer: Arc<Mutex<Mixer>>,
) -> Result<AudioIo, String> {
    let stop = Arc::new(AtomicBool::new(false));
    if let Ok(kind) = std::env::var("DCODE_AI_FAKE_AUDIO") {
        fake(kind == "tone", capture, mixer, stop.clone());
        return Ok(AudioIo {
            stop,
            backend: "fake",
        });
    }
    #[cfg(target_os = "linux")]
    {
        pulse::start(capture, mixer, stop.clone())?;
        Ok(AudioIo {
            stop,
            backend: "pulse",
        })
    }
    #[cfg(any(target_os = "macos", windows))]
    {
        native::start(capture, mixer, stop.clone())?;
        Ok(AudioIo {
            stop,
            backend: "native",
        })
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (capture, mixer);
        Err("calls aren't supported on this platform yet".into())
    }
}

/// No sound card: a tone or silence in, output pulled and thrown away.
fn fake(
    tone: bool,
    capture: UnboundedSender<Vec<i16>>,
    mixer: Arc<Mutex<Mixer>>,
    stop: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let mut t = 0u64;
        let mut out = vec![0i16; FRAME];
        while !stop.load(Ordering::SeqCst) {
            let frame: Vec<i16> = (0..FRAME)
                .map(|_| {
                    t += 1;
                    if tone {
                        ((t as f32 * 2.0 * std::f32::consts::PI * 440.0 / RATE as f32).sin()
                            * 8000.0) as i16
                    } else {
                        0
                    }
                })
                .collect();
            if capture.send(frame).is_err() {
                break;
            }
            if let Ok(mut m) = mixer.lock() {
                m.fill(&mut out);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    });
}

#[cfg(target_os = "linux")]
mod pulse {
    use super::*;
    use pulseaudio::{AsPlaybackSource, Client, protocol};

    fn spec() -> protocol::SampleSpec {
        protocol::SampleSpec {
            format: protocol::SampleFormat::S16Le,
            channels: 1,
            sample_rate: RATE,
        }
    }

    fn attr(target_ms: u32) -> protocol::stream::BufferAttr {
        let bytes = RATE * 2 * target_ms / 1000;
        protocol::stream::BufferAttr {
            max_length: u32::MAX,
            target_length: bytes,
            pre_buffering: bytes / 2,
            minimum_request_length: (FRAME * 2) as u32,
            fragment_size: (FRAME * 2) as u32,
        }
    }

    pub(super) fn start(
        capture: UnboundedSender<Vec<i16>>,
        mixer: Arc<Mutex<Mixer>>,
        stop: Arc<AtomicBool>,
    ) -> Result<(), String> {
        let client = Client::from_env(c"dcode-ai").map_err(|e| {
            format!(
                "no sound server found ({e}). Calls need PulseAudio or PipeWire (on WSL: Windows 11's WSLg)"
            )
        })?;
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        std::thread::spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = ready_tx.send(Err(e.to_string()));
                    return;
                }
            };
            rt.block_on(async move {
                let mut framer = Framer::new(capture);
                let record = client
                    .create_record_stream(
                        protocol::RecordStreamParams {
                            sample_spec: spec(),
                            channel_map: protocol::ChannelMap::mono(),
                            source_name: Some(protocol::DEFAULT_SOURCE.to_owned()),
                            buffer_attr: attr(40),
                            ..Default::default()
                        },
                        move |data: &[u8]| {
                            framer.push(
                                data.as_chunks::<2>()
                                    .0
                                    .iter()
                                    .map(|b| i16::from_le_bytes(*b)),
                            );
                        },
                    )
                    .await;
                let mut out = vec![0i16; FRAME * 4];
                let speak = mixer.clone();
                let playback = client
                    .create_playback_stream(
                        protocol::PlaybackStreamParams {
                            sample_spec: spec(),
                            channel_map: protocol::ChannelMap::mono(),
                            sink_name: Some(protocol::DEFAULT_SINK.to_owned()),
                            buffer_attr: attr(60),
                            ..Default::default()
                        },
                        (move |buf: &mut [u8]| {
                            let n = (buf.len() / 2).min(out.len());
                            if let Ok(mut m) = speak.lock() {
                                m.fill(&mut out[..n]);
                            }
                            for (i, s) in out[..n].iter().enumerate() {
                                let b = s.to_le_bytes();
                                buf[i * 2] = b[0];
                                buf[i * 2 + 1] = b[1];
                            }
                            n * 2
                        })
                        .as_playback_source(),
                    )
                    .await;
                let (record, playback) = match (record, playback) {
                    (Ok(r), Ok(p)) => (r, p),
                    (Err(e), _) | (_, Err(e)) => {
                        let _ = ready_tx
                            .send(Err(format!("can't open the microphone or speakers: {e}")));
                        return;
                    }
                };
                let _ = ready_tx.send(Ok(()));
                while !stop.load(Ordering::SeqCst) {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                let _ = record.delete().await;
                let _ = playback.delete().await;
            });
        });
        ready_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| "the sound server didn't answer".to_string())?
    }
}

#[cfg(any(target_os = "macos", windows))]
mod native {
    use super::*;
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    /// Linear resampling from `from` Hz to `to` Hz (speech quality).
    pub(super) struct Resampler {
        step: f64,
        pos: f64,
        last: f32,
    }

    impl Resampler {
        pub(super) fn new(from: u32, to: u32) -> Self {
            Self {
                step: from as f64 / to as f64,
                pos: 0.0,
                last: 0.0,
            }
        }
        pub(super) fn run(&mut self, input: &[f32], out: &mut Vec<f32>) {
            for &x in input {
                while self.pos <= 1.0 {
                    out.push(self.last + (x - self.last) * self.pos as f32);
                    self.pos += self.step;
                }
                self.pos -= 1.0;
                self.last = x;
            }
        }
    }

    pub(super) fn start(
        capture: UnboundedSender<Vec<i16>>,
        mixer: Arc<Mutex<Mixer>>,
        stop: Arc<AtomicBool>,
    ) -> Result<(), String> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();
        // cpal streams must stay on the thread that made them.
        std::thread::spawn(move || {
            let host = cpal::default_host();
            let run = || -> Result<(cpal::Stream, cpal::Stream), String> {
                let input = host.default_input_device().ok_or("no microphone found")?;
                let output = host.default_output_device().ok_or("no speakers found")?;
                let in_cfg = input.default_input_config().map_err(|e| e.to_string())?;
                let out_cfg = output.default_output_config().map_err(|e| e.to_string())?;
                let in_ch = in_cfg.channels() as usize;
                let out_ch = out_cfg.channels() as usize;
                let in_rate = in_cfg.sample_rate().0;
                let out_rate = out_cfg.sample_rate().0;

                let mut framer = Framer::new(capture);
                let mut down = Resampler::new(in_rate, RATE);
                let mut mono = Vec::new();
                let mut res = Vec::new();
                let err = |e| eprintln!("[call] audio: {e}");
                let in_stream = input
                    .build_input_stream(
                        &in_cfg.config(),
                        move |data: &[f32], _: &cpal::InputCallbackInfo| {
                            mono.clear();
                            mono.extend(
                                data.chunks(in_ch)
                                    .map(|c| c.iter().sum::<f32>() / in_ch as f32),
                            );
                            res.clear();
                            down.run(&mono, &mut res);
                            framer.push(res.iter().map(|v| (v.clamp(-1.0, 1.0) * 32767.0) as i16));
                        },
                        err,
                        None,
                    )
                    .map_err(|e| e.to_string())?;
                let mut up = Resampler::new(RATE, out_rate);
                let mut pending: std::collections::VecDeque<f32> = Default::default();
                let mut frame = vec![0i16; FRAME];
                let mut tmp = Vec::new();
                let out_stream = output
                    .build_output_stream(
                        &out_cfg.config(),
                        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                            let need = data.len() / out_ch;
                            while pending.len() < need {
                                if let Ok(mut m) = mixer.lock() {
                                    m.fill(&mut frame);
                                }
                                tmp.clear();
                                let f: Vec<f32> =
                                    frame.iter().map(|&s| s as f32 / 32768.0).collect();
                                up.run(&f, &mut tmp);
                                pending.extend(tmp.iter().copied());
                            }
                            for chunk in data.chunks_mut(out_ch) {
                                let v = pending.pop_front().unwrap_or(0.0);
                                chunk.iter_mut().for_each(|c| *c = v);
                            }
                        },
                        err,
                        None,
                    )
                    .map_err(|e| e.to_string())?;
                in_stream.play().map_err(|e| e.to_string())?;
                out_stream.play().map_err(|e| e.to_string())?;
                Ok((in_stream, out_stream))
            };
            match run() {
                Ok(streams) => {
                    let _ = ready_tx.send(Ok(()));
                    while !stop.load(Ordering::SeqCst) {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    drop(streams);
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                }
            }
        });
        ready_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .map_err(|_| "the audio device didn't start".to_string())?
    }
}
