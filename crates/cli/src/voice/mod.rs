//! Voice calls between the people on a session (`/call`).
//!
//! Each person's microphone goes through a voice gate (silence isn't sent)
//! and IMA ADPCM, to the host, who forwards it to everyone else in the
//! call; each person mixes what they hear. Audio is never recorded or
//! logged and never reaches the agent.

pub mod adpcm;
pub mod audio;
pub mod mixer;
pub mod vad;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// Samples per second.
pub const RATE: u32 = 16_000;
/// Samples per 20 ms frame.
pub const FRAME: usize = 320;
/// Someone counts as speaking this long after their last voiced frame.
const SPEAKING_FOR: Duration = Duration::from_millis(350);

/// A call as seen from one person: their microphone, what they hear, who
/// is speaking. Cheap to clone.
#[derive(Clone)]
pub struct Call {
    inner: Arc<Inner>,
}

struct Inner {
    me: String,
    muted: AtomicBool,
    mixer: Arc<Mutex<mixer::Mixer>>,
    speaking: Mutex<HashMap<String, Instant>>,
    started: Instant,
    _io: Mutex<Option<audio::AudioIo>>,
}

impl std::fmt::Debug for Call {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Call")
            .field("me", &self.inner.me)
            .field("muted", &self.muted())
            .finish()
    }
}

impl Call {
    /// Opens the microphone and speakers. Voiced frames from the microphone
    /// come out of the returned receiver, already encoded.
    pub fn start(me: &str) -> Result<(Self, mpsc::UnboundedReceiver<Vec<u8>>), String> {
        let mixer = Arc::new(Mutex::new(mixer::Mixer::default()));
        let (cap_tx, mut cap_rx) = mpsc::unbounded_channel::<Vec<i16>>();
        let io = audio::start(cap_tx, mixer.clone())?;
        let call = Self {
            inner: Arc::new(Inner {
                me: me.to_string(),
                muted: AtomicBool::new(false),
                mixer,
                speaking: Default::default(),
                started: Instant::now(),
                _io: Mutex::new(Some(io)),
            }),
        };
        let (out_tx, out_rx) = mpsc::unbounded_channel();
        let me_call = call.clone();
        tokio::spawn(async move {
            let mut gate = vad::Gate::default();
            while let Some(frame) = cap_rx.recv().await {
                if me_call.muted() || !gate.open(&frame) {
                    continue;
                }
                me_call.heard(&me_call.inner.me.clone());
                if out_tx.send(adpcm::encode(&frame)).is_err() {
                    break;
                }
            }
        });
        Ok((call, out_rx))
    }

    pub fn me(&self) -> &str {
        &self.inner.me
    }

    pub fn muted(&self) -> bool {
        self.inner.muted.load(Ordering::Relaxed)
    }

    pub fn set_muted(&self, on: bool) {
        self.inner.muted.store(on, Ordering::Relaxed);
    }

    pub fn elapsed(&self) -> Duration {
        self.inner.started.elapsed()
    }

    fn heard(&self, who: &str) {
        if let Ok(mut s) = self.inner.speaking.lock() {
            s.insert(who.to_string(), Instant::now());
        }
    }

    /// A packet from someone else in the call.
    pub fn play(&self, from: &str, packet: &[u8]) {
        if from == self.inner.me {
            return;
        }
        if let Some(samples) = adpcm::decode(packet, FRAME) {
            self.heard(from);
            if let Ok(mut m) = self.inner.mixer.lock() {
                m.push(from, &samples);
            }
        }
    }

    /// Someone left: stop playing what's left of them.
    pub fn forget(&self, who: &str) {
        if let Ok(mut m) = self.inner.mixer.lock() {
            m.forget(who);
        }
        if let Ok(mut s) = self.inner.speaking.lock() {
            s.remove(who);
        }
    }

    pub fn set_volume(&self, who: &str, volume: f32) {
        if let Ok(mut m) = self.inner.mixer.lock() {
            m.set_volume(who, volume);
        }
    }

    /// Is `who` speaking right now (you included)?
    pub fn speaking(&self, who: &str) -> bool {
        if who == self.inner.me && self.muted() {
            return false;
        }
        self.inner
            .speaking
            .lock()
            .ok()
            .and_then(|s| s.get(who).copied())
            .is_some_and(|t| t.elapsed() < SPEAKING_FOR)
    }

    /// Closes the microphone and speakers.
    pub fn hang_up(&self) {
        if let Ok(mut io) = self.inner._io.lock() {
            io.take();
        }
    }
}

/// `m:ss` for the footer.
pub fn clock(d: Duration) -> String {
    let s = d.as_secs();
    format!("{}:{:02}", s / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_tone_goes_out_encoded_and_comes_back_as_speech() {
        // SAFETY: only this test touches the variable, before audio starts.
        unsafe { std::env::set_var("DCODE_AI_FAKE_AUDIO", "tone") };
        let (a, mut out) = Call::start("alice").unwrap();
        let packet = tokio::time::timeout(Duration::from_secs(2), out.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(packet.len(), 3 + FRAME / 2);
        assert!(a.speaking("alice"));
        a.set_muted(true);
        assert!(!a.speaking("alice"), "muted isn't speaking");

        let (b, _out_b) = Call::start("bob").unwrap();
        b.play("alice", &packet);
        assert!(b.speaking("alice"));
        b.play("bob", &packet);
        b.forget("alice");
        assert!(!b.speaking("alice"));
        assert_eq!(clock(Duration::from_secs(134)), "2:14");
        a.hang_up();
        b.hang_up();
    }

    /// On a machine with sound: opens the real microphone and speakers.
    #[tokio::test]
    #[ignore]
    async fn real_audio_opens() {
        let mixer = Arc::new(Mutex::new(mixer::Mixer::default()));
        let (tx, mut rx) = mpsc::unbounded_channel();
        let io = audio::start(tx, mixer.clone()).expect("audio");
        // A second of 440 Hz to the speakers.
        let tone: Vec<i16> = (0..16000)
            .map(|i| {
                ((i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 16000.0).sin() * 6000.0) as i16
            })
            .collect();
        mixer.lock().unwrap().push("t", &tone);
        let mut frames = 0;
        let mut loudest = 0f32;
        let end = Instant::now() + Duration::from_secs(2);
        while Instant::now() < end {
            if let Ok(Some(f)) = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await {
                frames += 1;
                loudest = loudest.max(vad::rms(&f));
            }
        }
        eprintln!(
            "backend {} · mic frames in 2s: {frames} · loudest {loudest:.0}",
            io.backend()
        );
        drop(io);
    }
}
