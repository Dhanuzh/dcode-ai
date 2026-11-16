//! The finished video's timeline: title card, the kept stretches of the
//! recording (at their speeds) with inserts (cards, holds) placed between
//! them, and the end card. Maps finished-video time ↔ recording time.

use super::{END_SECONDS, Edl, Insert, TITLE_SECONDS};

#[derive(Debug, Clone, PartialEq)]
pub enum Piece {
    Title,
    Span { from: f64, to: f64, speed: f64 },
    Insert(Insert),
    End,
}

/// What is on screen at a moment of the finished video.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Moment<'a> {
    /// Title card, seconds into it.
    Title(f64),
    /// End card, seconds into it.
    End(f64),
    /// The recording at this second.
    Frame(f64),
    /// An insert, seconds into it.
    Insert(&'a Insert, f64),
}

#[derive(Debug, Clone)]
pub struct Timeline {
    /// (start, length, piece).
    pieces: Vec<(f64, f64, Piece)>,
    total: f64,
}

impl Timeline {
    /// The timeline of `edl` (normalized) with `extra` inserts (holds
    /// added to fit the narration).
    pub fn new(edl: &Edl, extra: &[Insert]) -> Self {
        let mut ins: Vec<Insert> = edl
            .inserts
            .iter()
            .chain(extra)
            .filter(|i| i.seconds > 0.0)
            .cloned()
            .collect();
        ins.sort_by(|a, b| a.at.total_cmp(&b.at));
        let mut raw: Vec<(f64, Piece)> = Vec::new();
        if !edl.title.trim().is_empty() {
            raw.push((TITLE_SECONDS, Piece::Title));
        }
        let mut k = 0;
        for seg in &edl.segments {
            let mut from = seg.from;
            while k < ins.len() && ins[k].at < seg.to {
                let at = ins[k].at.max(from);
                if at > from + 1e-9 {
                    raw.push((
                        (at - from) / seg.speed,
                        Piece::Span {
                            from,
                            to: at,
                            speed: seg.speed,
                        },
                    ));
                    from = at;
                }
                let mut i = ins[k].clone();
                i.at = at;
                raw.push((i.seconds, Piece::Insert(i)));
                k += 1;
            }
            if seg.to > from + 1e-9 {
                raw.push((
                    (seg.to - from) / seg.speed,
                    Piece::Span {
                        from,
                        to: seg.to,
                        speed: seg.speed,
                    },
                ));
            }
        }
        let last = edl.segments.last().map(|s| s.to).unwrap_or(0.0);
        for i in &ins[k..] {
            let mut i = i.clone();
            i.at = i.at.min(last);
            raw.push((i.seconds, Piece::Insert(i)));
        }
        if !edl.end_text.trim().is_empty() {
            raw.push((END_SECONDS, Piece::End));
        }
        let mut t = 0.0;
        let pieces = raw
            .into_iter()
            .map(|(len, p)| {
                let start = t;
                t += len;
                (start, len, p)
            })
            .collect();
        Self { pieces, total: t }
    }

    pub fn total(&self) -> f64 {
        self.total
    }

    /// Where the end card starts (or the end).
    pub fn body_end(&self) -> f64 {
        self.pieces
            .iter()
            .find(|p| p.2 == Piece::End)
            .map(|p| p.0)
            .unwrap_or(self.total)
    }

    /// Where recording second `t` lands, if kept. At an insert's moment,
    /// the time after the insert.
    pub fn output_time(&self, t: f64) -> Option<f64> {
        let mut at_end = None;
        for (start, _, p) in &self.pieces {
            if let Piece::Span { from, to, speed } = p {
                if t >= *from && t < *to {
                    return Some(start + (t - from) / speed);
                }
                if (t - to).abs() < 1e-9 {
                    at_end = Some(start + (to - from) / speed);
                }
            }
        }
        at_end
    }

    /// What is on screen at finished-video second `ot`.
    pub fn at(&self, ot: f64) -> Moment<'_> {
        let idx = self.pieces.partition_point(|p| p.0 <= ot).saturating_sub(1);
        let Some((start, len, p)) = self.pieces.get(idx) else {
            return Moment::Frame(0.0);
        };
        let local = (ot - start).clamp(0.0, *len);
        match p {
            Piece::Title => Moment::Title(local),
            Piece::End => Moment::End(local),
            Piece::Span { from, to, speed } => Moment::Frame((from + local * speed).min(*to)),
            Piece::Insert(i) => Moment::Insert(i, local),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::Segment;

    fn hold(at: f64, seconds: f64) -> Insert {
        Insert {
            at,
            seconds,
            kind: "hold".into(),
            title: String::new(),
            items: Vec::new(),
        }
    }

    #[test]
    fn inserts_split_spans_and_push_later_times() {
        let edl = Edl {
            title: "T".into(),
            end_text: "E".into(),
            segments: vec![Segment {
                from: 0.0,
                to: 10.0,
                speed: 1.0,
            }],
            ..Default::default()
        };
        let plain = Timeline::new(&edl, &[]);
        assert_eq!(plain.total(), TITLE_SECONDS + 10.0 + END_SECONDS);
        let tl = Timeline::new(&edl, &[hold(4.0, 3.0)]);
        assert_eq!(tl.total(), plain.total() + 3.0);
        // Before the hold: unchanged; at and after it: 3 s later.
        assert_eq!(tl.output_time(2.0), Some(TITLE_SECONDS + 2.0));
        assert_eq!(tl.output_time(4.0), Some(TITLE_SECONDS + 7.0));
        assert_eq!(tl.output_time(10.0), Some(TITLE_SECONDS + 13.0));
        // During the hold the frame at 4 s is shown.
        match tl.at(TITLE_SECONDS + 5.0) {
            Moment::Insert(i, local) => {
                assert_eq!(i.at, 4.0);
                assert!((local - 1.0).abs() < 1e-9);
            }
            m => panic!("{m:?}"),
        }
        assert!(matches!(tl.at(0.5), Moment::Title(_)));
        assert!(matches!(tl.at(tl.total() - 0.1), Moment::End(_)));
        assert_eq!(tl.body_end(), tl.total() - END_SECONDS);
    }

    #[test]
    fn an_insert_in_a_cut_out_stretch_goes_at_the_next_kept_one() {
        let edl = Edl {
            segments: vec![
                Segment {
                    from: 0.0,
                    to: 2.0,
                    speed: 1.0,
                },
                Segment {
                    from: 5.0,
                    to: 8.0,
                    speed: 1.0,
                },
            ],
            inserts: vec![hold(3.0, 1.0)],
            ..Default::default()
        };
        let tl = Timeline::new(&edl, &[]);
        assert_eq!(tl.total(), 6.0);
        assert_eq!(tl.output_time(5.0), Some(3.0));
        assert_eq!(tl.output_time(3.0), None);
    }
}
