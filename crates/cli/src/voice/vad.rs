//! A simple voice gate: frames are sent while their energy is above a
//! floor that follows the background noise, and for a short while after
//! (so word endings aren't cut).

pub struct Gate {
    noise: f32,
    hang: u32,
}

/// Frames kept open after speech stops (20 ms each).
const HANGOVER: u32 = 15;
/// Speech must be this much louder than the noise floor.
const MARGIN: f32 = 3.0;
/// Quietest level that can count as speech (RMS, 16-bit).
const MIN_LEVEL: f32 = 300.0;

impl Default for Gate {
    fn default() -> Self {
        Self {
            noise: MIN_LEVEL,
            hang: 0,
        }
    }
}

pub fn rms(frame: &[i16]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }
    let sum: f64 = frame.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum / frame.len() as f64).sqrt() as f32
}

impl Gate {
    /// Whether to send this frame.
    pub fn open(&mut self, frame: &[i16]) -> bool {
        let level = rms(frame);
        let speech = level > (self.noise * MARGIN).max(MIN_LEVEL);
        if speech {
            self.hang = HANGOVER;
        } else {
            // Follow the background slowly (and quickly when it drops).
            let k = if level < self.noise { 0.2 } else { 0.02 };
            self.noise = (self.noise + (level - self.noise) * k).max(MIN_LEVEL / MARGIN);
            self.hang = self.hang.saturating_sub(1);
        }
        speech || self.hang > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_is_not_sent_and_speech_opens_with_a_hangover() {
        let mut g = Gate::default();
        let quiet = vec![20i16; 320];
        let loud: Vec<i16> = (0..320)
            .map(|i| if i % 2 == 0 { 6000 } else { -6000 })
            .collect();
        for _ in 0..50 {
            assert!(!g.open(&quiet));
        }
        assert!(g.open(&loud));
        let mut open_after = 0;
        for _ in 0..40 {
            if g.open(&quiet) {
                open_after += 1;
            }
        }
        assert_eq!(open_after, HANGOVER as usize - 1);
    }
}
