//! What you hear: one queue per speaker (a small jitter buffer), mixed into
//! the speakers' output.

use std::collections::{HashMap, VecDeque};

/// Start playing a speaker once this much has arrived (smooths uneven
/// arrival), in samples.
const PREBUFFER: usize = 960; // 60 ms
/// Drop the oldest audio past this (keeps the delay from growing).
const MAX_QUEUE: usize = 4800; // 300 ms

#[derive(Default)]
struct Speaker {
    queue: VecDeque<i16>,
    playing: bool,
    volume: f32,
}

#[derive(Default)]
pub struct Mixer {
    speakers: HashMap<String, Speaker>,
}

impl Mixer {
    pub fn push(&mut self, who: &str, samples: &[i16]) {
        let s = self
            .speakers
            .entry(who.to_string())
            .or_insert_with(|| Speaker {
                volume: 1.0,
                ..Default::default()
            });
        s.queue.extend(samples.iter().copied());
        let excess = s.queue.len().saturating_sub(MAX_QUEUE);
        s.queue.drain(..excess);
    }

    /// Volume for one speaker (0.0–2.0).
    pub fn set_volume(&mut self, who: &str, volume: f32) {
        if let Some(s) = self.speakers.get_mut(who) {
            s.volume = volume.clamp(0.0, 2.0);
        }
    }

    pub fn forget(&mut self, who: &str) {
        self.speakers.remove(who);
    }

    /// Fills `out` with everyone mixed (silence where nobody speaks).
    pub fn fill(&mut self, out: &mut [i16]) {
        let mut acc = vec![0i32; out.len()];
        for s in self.speakers.values_mut() {
            if !s.playing {
                if s.queue.len() < PREBUFFER {
                    continue;
                }
                s.playing = true;
            }
            for a in acc.iter_mut() {
                match s.queue.pop_front() {
                    Some(v) => *a += (v as f32 * s.volume) as i32,
                    None => {
                        // Ran dry: wait for a fresh prebuffer.
                        s.playing = false;
                        break;
                    }
                }
            }
        }
        for (o, a) in out.iter_mut().zip(acc) {
            *o = a.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
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
        m.set_volume("b", 0.0);
        m.fill(&mut out);
        assert!(out.iter().all(|&s| s == 20000));
        // Too far behind: the oldest audio is dropped.
        m.push("c", &vec![1; 10_000]);
        assert!(m.speakers["c"].queue.len() <= MAX_QUEUE);
    }
}
