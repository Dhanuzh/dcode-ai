//! Voice calls between the people on a session (`/call`).
//!
//! Each person's microphone goes through a voice gate (silence isn't sent)
//! and IMA ADPCM, to the host, who forwards it to everyone else in the
//! call; each person mixes what they hear. Audio is never recorded or
//! logged and never reaches the agent.

pub mod adpcm;
pub mod audio;
pub mod mixer;
pub mod stt;
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
/// Push-to-talk: the mic stays open this long after the key's last
/// (repeated) press — releasing the key closes it.
const PTT_HOLD: Duration = Duration::from_millis(650);
/// Echo: the speakers' sound reaches the mic within this.
const ECHO_WINDOW: Duration = Duration::from_millis(250);
/// A mic frame quieter than this share of what just played is echo.
const ECHO_RATIO: f32 = 0.6;

/// Whether a microphone frame at `mic` RMS is (probably) the speakers'
/// sound at `playing` RMS coming back.
pub fn is_echo(mic: f32, playing: f32) -> bool {
    playing > 0.0 && mic < playing * ECHO_RATIO
}

/// A call as seen from one person: their microphone, what they hear, who
/// is speaking. Cheap to clone.
#[derive(Clone)]
pub struct Call {
    inner: Arc<Inner>,
}

struct Inner {
    me: String,
    muted: AtomicBool,
    /// Push-to-talk on, and until when the key is held.
    ptt: AtomicBool,
    held_until: Mutex<Instant>,
    /// Drop mic frames that are the speakers' echo.
    echo_gate: AtomicBool,
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
                ptt: AtomicBool::new(false),
                held_until: Mutex::new(Instant::now()),
                echo_gate: AtomicBool::new(true),
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
                if me_call.muted() || (me_call.ptt() && !me_call.held()) {
                    continue;
                }
                // The speakers' own sound coming back: don't send it.
                if me_call.inner.echo_gate.load(Ordering::Relaxed) {
                    let playing = me_call
                        .inner
                        .mixer
                        .lock()
                        .map(|m| m.recent_level(ECHO_WINDOW))
                        .unwrap_or(0.0);
                    if is_echo(vad::rms(&frame), playing) {
                        continue;
                    }
                }
                if !gate.open(&frame) {
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

    pub fn ptt(&self) -> bool {
        self.inner.ptt.load(Ordering::Relaxed)
    }

    pub fn set_ptt(&self, on: bool) {
        self.inner.ptt.store(on, Ordering::Relaxed);
    }

    /// The push-to-talk key is down (pressed or repeating) now.
    pub fn hold(&self) {
        if let Ok(mut t) = self.inner.held_until.lock() {
            *t = Instant::now() + PTT_HOLD;
        }
    }

    /// The push-to-talk key was released (terminals that report it).
    pub fn release(&self) {
        if let Ok(mut t) = self.inner.held_until.lock() {
            *t = Instant::now();
        }
    }

    pub fn held(&self) -> bool {
        self.inner
            .held_until
            .lock()
            .map(|t| Instant::now() < *t)
            .unwrap_or(false)
    }

    pub fn echo_gate(&self) -> bool {
        self.inner.echo_gate.load(Ordering::Relaxed)
    }

    pub fn set_echo_gate(&self, on: bool) {
        self.inner.echo_gate.store(on, Ordering::Relaxed);
    }

    pub fn volume(&self, who: &str) -> f32 {
        self.inner
            .mixer
            .lock()
            .map(|m| m.volume(who))
            .unwrap_or(1.0)
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
        if who == self.inner.me && (self.muted() || (self.ptt() && !self.held())) {
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

    #[tokio::test]
    async fn push_to_talk_sends_only_while_held() {
        // SAFETY: tests that touch this set the same value.
        unsafe { std::env::set_var("DCODE_AI_FAKE_AUDIO", "tone") };
        let (call, mut out) = Call::start("ptt").unwrap();
        call.set_echo_gate(false);
        call.set_ptt(true);
        tokio::time::sleep(Duration::from_millis(200)).await;
        while out.try_recv().is_ok() {}
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(out.try_recv().is_err(), "nothing while the key is up");
        call.hold();
        let got = tokio::time::timeout(Duration::from_secs(1), out.recv()).await;
        assert!(got.is_ok(), "frames while held");
        call.release();
        tokio::time::sleep(Duration::from_millis(100)).await;
        while out.try_recv().is_ok() {}
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(out.try_recv().is_err(), "closed again after release");
        call.hang_up();
    }

    #[test]
    fn quiet_frames_while_the_speakers_play_are_echo() {
        assert!(is_echo(1000.0, 4000.0));
        assert!(
            !is_echo(5000.0, 4000.0),
            "speaking over someone gets through"
        );
        assert!(!is_echo(1000.0, 0.0), "nothing playing: not echo");
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
