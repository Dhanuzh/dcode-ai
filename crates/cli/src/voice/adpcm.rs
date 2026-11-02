//! IMA ADPCM, one self-contained packet per 20 ms frame (4:1): a lost or
//! late packet costs only its own 20 ms.
//!
//! Packet: predictor (i16 LE), step index (u8), then two samples per byte
//! (low nibble first).

const INDEX_TABLE: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];

const STEP_TABLE: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];

#[derive(Clone, Copy)]
struct State {
    predictor: i32,
    index: i32,
}

impl State {
    fn decode(&mut self, nibble: u8) -> i16 {
        let step = STEP_TABLE[self.index as usize];
        let mut diff = step >> 3;
        if nibble & 4 != 0 {
            diff += step;
        }
        if nibble & 2 != 0 {
            diff += step >> 1;
        }
        if nibble & 1 != 0 {
            diff += step >> 2;
        }
        if nibble & 8 != 0 {
            self.predictor -= diff;
        } else {
            self.predictor += diff;
        }
        self.predictor = self.predictor.clamp(-32768, 32767);
        self.index = (self.index + INDEX_TABLE[nibble as usize]).clamp(0, 88);
        self.predictor as i16
    }

    fn encode(&mut self, sample: i16) -> u8 {
        let step = STEP_TABLE[self.index as usize];
        let mut diff = sample as i32 - self.predictor;
        let mut nibble = 0u8;
        if diff < 0 {
            nibble = 8;
            diff = -diff;
        }
        let mut s = step;
        if diff >= s {
            nibble |= 4;
            diff -= s;
        }
        s >>= 1;
        if diff >= s {
            nibble |= 2;
            diff -= s;
        }
        s >>= 1;
        if diff >= s {
            nibble |= 1;
        }
        // Track exactly what the decoder will reconstruct.
        self.decode(nibble);
        nibble
    }
}

/// Picks a starting step index that suits the frame's first jump.
fn start_index(samples: &[i16]) -> i32 {
    let jump = samples
        .windows(2)
        .take(8)
        .map(|w| (w[1] as i32 - w[0] as i32).abs())
        .max()
        .unwrap_or(0);
    STEP_TABLE.iter().position(|&s| s * 2 >= jump).unwrap_or(88) as i32
}

pub fn encode(samples: &[i16]) -> Vec<u8> {
    let first = samples.first().copied().unwrap_or(0);
    let mut st = State {
        predictor: first as i32,
        index: start_index(samples),
    };
    let mut out = Vec::with_capacity(3 + samples.len().div_ceil(2));
    out.extend_from_slice(&first.to_le_bytes());
    out.push(st.index as u8);
    for pair in samples.chunks(2) {
        let lo = st.encode(pair[0]);
        let hi = pair.get(1).map(|&s| st.encode(s)).unwrap_or(0);
        out.push(lo | (hi << 4));
    }
    out
}

/// Decodes a packet into `n` samples (the frame size); `None` if malformed.
pub fn decode(packet: &[u8], n: usize) -> Option<Vec<i16>> {
    if packet.len() < 3 + n.div_ceil(2) {
        return None;
    }
    let mut st = State {
        predictor: i16::from_le_bytes([packet[0], packet[1]]) as i32,
        index: (packet[2] as i32).clamp(0, 88),
    };
    let mut out = Vec::with_capacity(n);
    for &b in &packet[3..] {
        if out.len() < n {
            out.push(st.decode(b & 0x0f));
        }
        if out.len() < n {
            out.push(st.decode(b >> 4));
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tone_survives_the_round_trip() {
        let tone: Vec<i16> = (0..320)
            .map(|i| {
                ((i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 16000.0).sin() * 8000.0) as i16
            })
            .collect();
        let packet = encode(&tone);
        assert_eq!(packet.len(), 3 + 160, "4:1 plus a 3-byte header");
        let back = decode(&packet, 320).unwrap();
        let err: f64 = tone
            .iter()
            .zip(&back)
            .map(|(a, b)| ((*a as f64) - (*b as f64)).powi(2))
            .sum::<f64>()
            / 320.0;
        let signal: f64 = tone.iter().map(|a| (*a as f64).powi(2)).sum::<f64>() / 320.0;
        let snr = 10.0 * (signal / err.max(1.0)).log10();
        assert!(snr > 20.0, "SNR {snr:.1} dB");
        assert!(decode(&packet[..10], 320).is_none());
    }
}
