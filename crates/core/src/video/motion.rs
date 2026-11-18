//! Camera and cursor motion the way screen-recording editors do it:
//! targets from the edit list (zooms) and the event track (where the cursor
//! goes), smoothed by a critically damped spring — no linear slides, no
//! overshoot. Nearby zooms merge: the camera stays in and pans from one
//! action to the next instead of bouncing out and in.
//!
//! Tracks are worked out once (at 240 Hz of recording time) and sampled per
//! frame, so frames can still be drawn in parallel.

use super::Edl;

/// A sampled track of three values (camera: scale, x, y in CSS pixels;
/// cursor: x, y, unused).
#[derive(Debug, Clone)]
pub struct Track {
    dt: f64,
    v: Vec<[f64; 3]>,
}

impl Track {
    pub fn at(&self, t: f64) -> [f64; 3] {
        if self.v.is_empty() {
            return [1.0, 0.0, 0.0];
        }
        let p = (t.max(0.0) / self.dt).min((self.v.len() - 1) as f64);
        let i = p as usize;
        let j = (i + 1).min(self.v.len() - 1);
        let k = p - i as f64;
        let (a, b) = (self.v[i], self.v[j]);
        [
            a[0] + (b[0] - a[0]) * k,
            a[1] + (b[1] - a[1]) * k,
            a[2] + (b[2] - a[2]) * k,
        ]
    }
}

const DT: f64 = 1.0 / 240.0;

/// A held zoom: start, end, scale, and the (time, x, y) it visits.
type Group = (f64, f64, f64, Vec<(f64, f64, f64)>);

/// Follows `target` with a critically damped spring per value (`hz`: how
/// quick; higher is snappier).
fn spring(target: impl Fn(f64) -> [f64; 3], until: f64, hz: [f64; 3]) -> Track {
    let n = ((until.max(0.0) / DT) as usize + 2).min(240 * 60 * 15);
    let mut x = target(0.0);
    let mut v = [0.0; 3];
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let t = i as f64 * DT;
        let g = target(t);
        for c in 0..3 {
            let w = std::f64::consts::TAU * hz[c];
            let a = w * w * (g[c] - x[c]) - 2.0 * w * v[c];
            v[c] += a * DT;
            x[c] += v[c] * DT;
        }
        out.push(x);
    }
    Track { dt: DT, v: out }
}

/// A zoom held over a stretch: zooms closer than this merge.
pub const MERGE_GAP: f64 = 1.5;
/// The camera starts moving this long before the action.
pub const LEAD: f64 = 0.55;

/// The camera: scale and centre (CSS pixels) over recording time. `rest`:
/// where it sits between zooms (the whole page, or its main content).
pub fn camera(edl: &Edl, rest: impl Fn(f64) -> [f64; 3], until: f64) -> Track {
    let mut zooms = edl.zooms.clone();
    zooms.sort_by(|a, b| a.at.total_cmp(&b.at));
    // Groups of zooms close in time: one held zoom that pans.
    let mut groups: Vec<Group> = Vec::new();
    for z in &zooms {
        let (start, end) = (z.at - LEAD, z.at + z.hold + 0.3);
        match groups.last_mut() {
            Some(g) if start - g.1 < MERGE_GAP => {
                g.1 = g.1.max(end);
                g.2 = g.2.max(z.scale);
                g.3.push((z.at, z.x, z.y));
            }
            _ => groups.push((start, end, z.scale, vec![(z.at, z.x, z.y)])),
        }
    }
    let target = |t: f64| -> [f64; 3] {
        for (start, end, scale, points) in &groups {
            if t >= *start && t <= *end {
                // The latest action that has begun (looking a little ahead).
                let p = points
                    .iter()
                    .rev()
                    .find(|p| p.0 - LEAD <= t)
                    .unwrap_or(&points[0]);
                return [scale.max(rest(t)[0] * 1.15), p.1, p.2];
            }
        }
        rest(t)
    };
    spring(target, until, [1.1, 1.3, 1.3])
}

/// The cursor (CSS pixels) over recording time: still between actions,
/// travelling to the next one shortly before it, smoothed.
pub fn cursor(points: &[(f64, f64, f64)], until: f64) -> Option<Track> {
    let first = *points.first()?;
    let target = |t: f64| -> [f64; 3] {
        // Head for the next action from 0.7 s before it.
        let p = points
            .iter()
            .rev()
            .find(|p| p.0 - 0.7 <= t)
            .unwrap_or(&first);
        [p.1, p.2, 0.0]
    };
    Some(spring(target, until, [2.2, 2.2, 1.0]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::Zoom;

    fn zoom(at: f64, x: f64) -> Zoom {
        Zoom {
            at,
            x,
            y: 100.0,
            scale: 2.0,
            hold: 1.0,
            anchor: String::new(),
        }
    }

    #[test]
    fn the_camera_eases_in_holds_pans_and_eases_out() {
        let edl = Edl {
            zooms: vec![zoom(3.0, 200.0), zoom(4.5, 600.0), zoom(12.0, 300.0)],
            ..Default::default()
        };
        let cam = camera(&edl, |_| [1.0, 500.0, 300.0], 16.0);
        // Out at the start.
        assert!((cam.at(0.5)[0] - 1.0).abs() < 1e-3);
        // In by the first action, centred near it.
        let a = cam.at(3.4);
        assert!(a[0] > 1.7, "{a:?}");
        assert!((a[1] - 200.0).abs() < 60.0, "{a:?}");
        // 4.5 s is close: it stays in and pans, never back to 1×.
        let mid = (35..45)
            .map(|i| cam.at(i as f64 / 10.0)[0])
            .fold(9.0, f64::min);
        assert!(mid > 1.6, "stays zoomed while panning: {mid}");
        assert!((cam.at(5.6)[1] - 600.0).abs() < 60.0);
        // Out again before the far-off third zoom.
        assert!((cam.at(10.0)[0] - 1.0).abs() < 0.05);
        // Smooth: no jumps between samples.
        let jumps = (0..1600)
            .map(|i| {
                let (a, b) = (cam.at(i as f64 / 100.0), cam.at((i + 1) as f64 / 100.0));
                (b[1] - a[1]).abs()
            })
            .fold(0.0, f64::max);
        assert!(jumps < 30.0, "{jumps}");
    }

    #[test]
    fn the_cursor_waits_then_travels_smoothly() {
        let pts = [(1.0, 0.0, 0.0), (5.0, 100.0, 0.0)];
        let c = cursor(&pts, 8.0).unwrap();
        assert!(c.at(2.0)[0].abs() < 1.0, "waits");
        let x = c.at(4.6)[0];
        assert!(x > 1.0 && x < 100.0, "on its way: {x}");
        assert!((c.at(6.0)[0] - 100.0).abs() < 1.0, "arrived");
    }
}
