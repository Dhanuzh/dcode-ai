//! Accurate token counting backed by the `o200k_base` BPE (GPT-4o family).
//!
//! Replaces the previous `chars / 4` heuristic used for local context
//! budgeting. Authoritative usage still comes from provider `Usage` events;
//! this exists only for pre-send budgeting and compaction triggers, where a
//! real tokenizer keeps the "when to compact" decision honest across models
//! whose tokenization is far denser than 4 chars/token (code, JSON, CJK).

use std::sync::LazyLock;

use tiktoken_rs::CoreBPE;

/// Lazily-built BPE. `None` only if the embedded ranks fail to load, in which
/// case we fall back to a chars/4 estimate so counting never panics.
static BPE: LazyLock<Option<CoreBPE>> = LazyLock::new(|| tiktoken_rs::o200k_base().ok());

/// Count tokens in `text` using the `o200k_base` encoder.
///
/// `encode_ordinary` is used so embedded text that happens to look like a
/// special token (e.g. `<|endoftext|>`) is counted as plain text rather than
/// triggering special-token handling.
pub fn count_tokens(text: &str) -> usize {
    match &*BPE {
        Some(bpe) => bpe.encode_ordinary(text).len(),
        None => text.chars().count() / 4 + 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_more_than_zero_for_nonempty() {
        assert!(count_tokens("hello world") > 0);
    }

    #[test]
    fn empty_is_zero() {
        assert_eq!(count_tokens(""), 0);
    }

    #[test]
    fn dense_code_beats_chars_over_four() {
        // Punctuation-dense code tokenizes to more tokens than chars/4 would
        // predict — the whole reason for replacing the heuristic.
        let code = "fn main(){let x=vec![1,2,3];println!(\"{:?}\",x);}";
        let approx = code.chars().count() / 4;
        assert!(count_tokens(code) > approx);
    }
}
