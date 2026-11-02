//! What you hear: one queue per speaker (an adaptive jitter buffer), mixed
//! into the speakers' output.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Smallest and largest amount buffered before a speaker plays, in samples.
const MIN_PREBUFFER: usize = 640; // 40 ms
const START_PREBUFFER: usize = 960; // 60 ms
const MAX_PREBUFFER: usize = 4800; // 300 ms
/// Each dry-out mid-sentence adds this much buffering.
const GROW: usize = 320; // 20 ms
/// After this long without a dry-out, buffering shrinks a little.
const CALM: Duration = Duration::from_secs(10);
/// Drop the oldest audio past the buffer target plus this much.
const SLACK: usize = 3200; // 200 ms
/// Concealment: how much of the last audio is repeated over a gap.
const CONCEAL: usize = 320; // 20 ms

struct Speaker {
    queue: VecDeque<i16>,
    playing: bool,
    volume: f32,
    /// Buffer target (adapts to the network).
    prebuffer: usize,
    last_dry: Instant,
    /// The last samples played (repeated, faded, over a gap).
    tail: Vec<i16>,
}

impl Speaker {
    fn new() -> Self {
        Self {
            queue: VecDeque::new(),
            playing: false,
            volume: 1.0,
            prebuffer: START_PREBUFFER,
            last_dry: Instant::now(),
            tail: Vec::new(),
        }
    }
}

pub struct Mixer {
    speakers: HashMap<String, Speaker>,
    /// How loud the output was lately, and when (for the echo gate).
    level: f32,
    level_at: Instant,
}

impl Default for Mixer {
    fn default() -> Self {
        Self {
            speakers: HashMap::new(),
            level: 0.0,
            level_at: Instant::now() - Duration::from_secs(60),
        }
    }
}

impl Mixer {
    pub fn push(&mut self, who: &str, samples: &[i16]) {
        let s = self
            .speakers
            .entry(who.to_string())
            .or_insert_with(Speaker::new);
        s.queue.extend(samples.iter().copied());
        let excess = s.queue.len().saturating_sub(s.prebuffer + SLACK);
        s.queue.drain(..excess);
    }

    /// Volume for one speaker (0.0–2.0); remembered for when they speak.
    pub fn set_volume(&mut self, who: &str, volume: f32) {
        self.speakers
            .entry(who.to_string())
            .or_insert_with(Speaker::new)
            .volume = volume.clamp(0.0, 2.0);
    }

    pub fn volume(&self, who: &str) -> f32 {
        self.speakers.get(who).map(|s| s.volume).unwrap_or(1.0)
    }

    pub fn forget(&mut self, who: &str) {
        self.speakers.remove(who);
    }

    /// How loud the speakers played within `within` (RMS), or 0.
    pub fn recent_level(&self, within: Duration) -> f32 {
        if self.level_at.elapsed() < within {
            self.level
        } else {
            0.0
        }
    }

    /// Fills `out` with everyone mixed (silence where nobody speaks).
    pub fn fill(&mut self, out: &mut [i16]) {
        let mut acc = vec![0i32; out.len()];
        for s in self.speakers.values_mut() {
            if !s.playing {
                if s.queue.len() < s.prebuffer {
                    continue;
                }
                s.playing = true;
            }
            let mut dry_at = None;
            let mut mine = Vec::with_capacity(acc.len());
            for (i, a) in acc.iter_mut().enumerate() {
                match s.queue.pop_front() {
                    Some(v) => {
                        mine.push(v);
                        *a += (v as f32 * s.volume) as i32
                    }
                    None => {
                        dry_at = Some(i);
                        break;
                    }
                }
            }
            if let Some(i) = dry_at {
                // Ran dry mid-sentence: hide the gap with the last audio,
                // fading out, then wait for a (bigger) fresh buffer.
                let n = (acc.len() - i).min(CONCEAL).min(s.tail.len());
                for k in 0..n {
                    let fade = 1.0 - (k as f32 / n.max(1) as f32);
                    let v = s.tail[s.tail.len() - n + k] as f32 * 0.5 * fade;
                    acc[i + k] += (v * s.volume) as i32;
                }
                s.playing = false;
                s.prebuffer = (s.prebuffer + GROW).min(MAX_PREBUFFER);
                s.last_dry = Instant::now();
            } else if s.last_dry.elapsed() > CALM && s.prebuffer > MIN_PREBUFFER {
                s.prebuffer = s.prebuffer.saturating_sub(GROW / 2).max(MIN_PREBUFFER);
                s.last_dry = Instant::now();
            }
            // Remember what this speaker just said (for concealment).
            if !mine.is_empty() {
                let start = mine.len().saturating_sub(CONCEAL);
                s.tail.clear();
                s.tail.extend_from_slice(&mine[start..]);
            }
        }
        let mut sum = 0f64;
        for (o, a) in out.iter_mut().zip(acc) {
            *o = a.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
            sum += (*o as f64) * (*o as f64);
        }
        let rms = (sum / out.len().max(1) as f64).sqrt() as f32;
        if rms > 50.0 {
            self.level = rms;
            self.level_at = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speakers_mix_after_a_prebuffer_without_clipping() {
        let mut m = Mixer::default();
        m.push("a", &[20000; 320]);
        let mut out = [0i16; 320];
        m.fill(&mut out);
        assert!(out.iter().all(|&s| s == 0), "still prebuffering");
        m.push("a", &[20000; 960]);
        m.push("b", &[20000; 1280]);
        m.fill(&mut out);
        assert!(out.iter().all(|&s| s == i16::MAX), "summed and clamped");
        assert!(m.recent_level(Duration::from_millis(250)) > 10000.0);
        m.set_volume("b", 0.0);
        m.fill(&mut out);
        assert!(out.iter().all(|&s| s == 20000));
        // Too far behind: the oldest audio is dropped.
        m.push("c", &vec![1; 10_000]);
        assert!(m.speakers["c"].queue.len() <= START_PREBUFFER + SLACK);
    }

    #[test]
    fn a_gap_is_concealed_and_the_buffer_grows() {
        let mut m = Mixer::default();
        m.push("a", &[8000; 960]);
        let mut out = [0i16; 320];
        for _ in 0..3 {
            m.fill(&mut out);
        }
        // Ran dry: the next frame fades the last audio instead of silence.
        m.fill(&mut out);
        assert!(out[0] > 1000, "concealed, not cut: {}", out[0]);
        assert!(out[319].abs() < out[0].abs(), "fading out");
        assert_eq!(m.speakers["a"].prebuffer, START_PREBUFFER + GROW);
        // Another dry-out grows it again.
        m.push("a", &[8000; 1280]);
        for _ in 0..5 {
            m.fill(&mut out);
        }
        assert_eq!(m.speakers["a"].prebuffer, START_PREBUFFER + 2 * GROW);
    }
}
