//! Background music, made here: a calm, royalty-free pad and arpeggio over
//! a four-chord loop, with fades — so a video never needs a licensed track.
//! (A file of your own can be used instead: `music` in the edit list.)

use std::f64::consts::TAU;
use std::path::Path;

pub const RATE: u32 = 44_100;

/// MIDI note → Hz.
fn hz(note: f64) -> f64 {
    440.0 * 2f64.powf((note - 69.0) / 12.0)
}

/// `seconds` of stereo 16-bit music (interleaved), quiet enough to sit
/// under a screen recording.
pub fn ambient(seconds: f64) -> Vec<i16> {
    let n = (seconds.max(0.5) * RATE as f64) as usize;
    // C – Am – F – G, voiced low and wide; four seconds each.
    const CHORDS: [[f64; 4]; 4] = [
        [48.0, 55.0, 64.0, 67.0],
        [45.0, 52.0, 60.0, 64.0],
        [41.0, 48.0, 57.0, 60.0],
        [43.0, 50.0, 59.0, 62.0],
    ];
    const CHORD_SECONDS: f64 = 4.0;
    const STEP: f64 = 0.5; // arpeggio note every half second
    let mut out = Vec::with_capacity(n * 2);
    let fade_in = 1.5;
    let fade_out = 2.5_f64.min(seconds / 3.0);
    for i in 0..n {
        let t = i as f64 / RATE as f64;
        let ci = (t / CHORD_SECONDS) as usize % CHORDS.len();
        let local = t % CHORD_SECONDS;
        // Pad: soft sines with slow swell between chords.
        let swell = (local / 0.8).min(1.0) * ((CHORD_SECONDS - local) / 0.8).min(1.0);
        let mut left = 0.0;
        let mut right = 0.0;
        for (k, note) in CHORDS[ci].iter().enumerate() {
            let f = hz(*note);
            let detune = 1.0 + 0.0015 * (k as f64 - 1.5);
            let v = (TAU * f * t).sin() * 0.6 + (TAU * f * 2.0 * detune * t).sin() * 0.12;
            let pan = 0.35 + 0.1 * k as f64;
            left += v * (1.0 - pan) * swell;
            right += v * pan * swell;
        }
        // Arpeggio: a soft pluck up the chord, an octave higher.
        let step = (t / STEP) as usize;
        let since = t - step as f64 * STEP;
        let note = CHORDS[ci][step % 4] + 12.0;
        let pluck = (TAU * hz(note) * t).sin() * (-since * 5.0).exp() * 0.35;
        left += pluck * 0.55;
        right += pluck * 0.45;
        let fade = (t / fade_in).min(1.0) * ((seconds - t) / fade_out).clamp(0.0, 1.0);
        let gain = 0.11 * fade;
        out.push((left * gain * i16::MAX as f64) as i16);
        out.push((right * gain * i16::MAX as f64) as i16);
    }
    out
}

/// Writes interleaved stereo samples as a WAV file.
pub fn write_wav(path: &Path, samples: &[i16]) -> std::io::Result<()> {
    let data_len = (samples.len() * 2) as u32;
    let mut b = Vec::with_capacity(44 + data_len as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&2u16.to_le_bytes()); // stereo
    b.extend_from_slice(&RATE.to_le_bytes());
    b.extend_from_slice(&(RATE * 4).to_le_bytes());
    b.extend_from_slice(&4u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        b.extend_from_slice(&s.to_le_bytes());
    }
    std::fs::write(path, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn music_is_quiet_fades_and_lasts_as_long_as_asked() {
        let s = ambient(6.0);
        assert_eq!(s.len(), 6 * RATE as usize * 2);
        let peak = s.iter().map(|v| v.unsigned_abs()).max().unwrap();
        assert!(peak > 1000, "audible: {peak}");
        assert!(peak < 16_000, "stays under the video: {peak}");
        // Silent at the very start and end (fades).
        assert!(s[0].unsigned_abs() < 50 && s[s.len() - 1].unsigned_abs() < 50);
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("m.wav");
        write_wav(&p, &s).unwrap();
        let raw = std::fs::read(&p).unwrap();
        assert_eq!(&raw[..4], b"RIFF");
        assert_eq!(raw.len(), 44 + s.len() * 2);
    }
}
