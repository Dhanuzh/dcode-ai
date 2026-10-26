//! Spotting a model stuck repeating itself mid-reply ("degenerate" output),
//! so the stream can be stopped before it burns the token budget.

/// Longest repeated unit looked for, in bytes (a long sentence or a few lines).
const MAX_UNIT: usize = 500;
/// Repeated span needed before calling it a loop.
const MIN_SPAN: usize = 1_500;
/// The unit must repeat at least this many times back to back.
const MIN_REPEATS: usize = 5;

/// If `text` ends in one chunk repeated back to back over a long span,
/// return the byte length to keep: everything before the run plus one copy.
pub fn looping_tail(text: &str) -> Option<usize> {
    let b = text.as_bytes();
    let n = b.len();
    if n < MIN_SPAN {
        return None;
    }
    for unit in 1..=MAX_UNIT.min(n / MIN_REPEATS) {
        let span = MIN_SPAN.max(unit * MIN_REPEATS);
        if span > n {
            break;
        }
        // Periodic with period `unit` over the last `span` bytes.
        if b[n - span + unit..] != b[n - span..n - unit] {
            continue;
        }
        let mut start = n - span;
        while start > 0 && b[start - 1] == b[start - 1 + unit] {
            start -= 1;
        }
        let mut keep = start + unit;
        // The run may start mid-line; end the kept copy at a line break.
        if let Some(nl) = b[keep..(keep + unit).min(n)]
            .iter()
            .position(|&c| c == b'\n')
        {
            keep += nl + 1;
        }
        while !text.is_char_boundary(keep) {
            keep -= 1;
        }
        return Some(keep);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repeated_sentence_is_cut_to_one_copy() {
        let intro = "Here is the plan.\n";
        let line = "I will now check the configuration file again.\n";
        let text = format!("{intro}{}", line.repeat(60));
        let keep = looping_tail(&text).expect("loop");
        assert_eq!(&text[..keep], format!("{intro}{line}"));
    }

    #[test]
    fn varied_prose_is_left_alone() {
        let text: String = (0..300)
            .map(|i| format!("Step {i}: look at file_{i}.rs and note what changed.\n"))
            .collect();
        assert_eq!(looping_tail(&text), None);
    }

    #[test]
    fn short_repeats_are_not_a_loop() {
        let text = format!("{}done", "ok ".repeat(40));
        assert_eq!(looping_tail(&text), None);
    }

    #[test]
    fn multibyte_text_cuts_on_a_char_boundary() {
        let text = "→ retrying the request… ".repeat(120);
        let keep = looping_tail(&text).expect("loop");
        assert!(text.is_char_boundary(keep));
    }
}
