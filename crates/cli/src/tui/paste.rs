//! Paste-token helpers: collapse a large multi-line paste into a compact
//! placeholder token, and expand stored tokens back to their full text before
//! sending. Extracted from `tui::app`.

use std::collections::HashMap;

/// If `pasted` spans more than one line, return a compact placeholder token to
/// show in the composer (the full text is stored separately and expanded later).
pub(crate) fn pasted_lines_token(pasted: &str, counter: u32) -> Option<String> {
    let normalized = pasted.replace("\r\n", "\n").replace('\r', "\n");
    let trimmed = normalized.trim_end_matches('\n');
    let line_count = if trimmed.is_empty() {
        0
    } else {
        trimmed.split('\n').count()
    };
    (line_count > 1).then(|| format!("[Pasted text #{counter} +{line_count} lines]"))
}

/// Composer placeholder for a staged image (Claude Code's `[Image #N]`).
pub(crate) fn image_token(counter: u32) -> String {
    format!("[Image #{counter}]")
}

/// Whether `s` is a whole placeholder token (`[Image #N]` or
/// `[Pasted text #N +L lines]`), which the composer edits as one unit.
pub(crate) fn is_placeholder_token(s: &str) -> bool {
    let Some(inner) = s.strip_prefix('[').and_then(|r| r.strip_suffix(']')) else {
        return false;
    };
    if inner.contains('[') || inner.contains(']') {
        return false;
    }
    let digits = |t: &str| !t.is_empty() && t.chars().all(|c| c.is_ascii_digit());
    if let Some(n) = inner.strip_prefix("Image #") {
        return digits(n);
    }
    if let Some(rest) = inner.strip_prefix("Pasted text #")
        && let Some((n, lines)) = rest.split_once(" +")
    {
        return digits(n) && lines.strip_suffix(" lines").is_some_and(digits);
    }
    false
}

/// Char ranges `[start, end)` of placeholder tokens in `text`.
pub(crate) fn placeholder_char_ranges(text: &str) -> Vec<(usize, usize)> {
    let chars: Vec<char> = text.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '['
            && let Some(len) = chars[i..].iter().position(|&c| c == ']')
        {
            let tok: String = chars[i..=i + len].iter().collect();
            if is_placeholder_token(&tok) {
                out.push((i, i + len + 1));
                i += len + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// The placeholder token ending exactly at char `cursor`, as `(start, end)`.
pub(crate) fn placeholder_before(text: &str, cursor: usize) -> Option<(usize, usize)> {
    placeholder_char_ranges(text)
        .into_iter()
        .find(|&(_, end)| end == cursor)
}

/// Replace each placeholder token in `text` with its stored full content.
pub(crate) fn expand_paste_tokens(text: &str, store: &HashMap<String, String>) -> String {
    let mut result = text.to_string();
    for (token, content) in store {
        result = result.replace(token.as_str(), content.as_str());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_line_paste_gets_no_token() {
        assert_eq!(pasted_lines_token("just one line", 1), None);
    }

    #[test]
    fn empty_paste_gets_no_token() {
        assert_eq!(pasted_lines_token("", 1), None);
        assert_eq!(pasted_lines_token("\n\n\n", 1), None);
    }

    #[test]
    fn multiline_paste_reports_line_count_and_counter() {
        let token = pasted_lines_token("line 1\nline 2\nline 3", 7).unwrap();
        assert_eq!(token, "[Pasted text #7 +3 lines]");
    }

    #[test]
    fn trailing_newlines_do_not_inflate_line_count() {
        // A paste ending in one or more newlines shouldn't count a phantom
        // trailing empty line.
        let token = pasted_lines_token("a\nb\n\n\n", 1).unwrap();
        assert_eq!(token, "[Pasted text #1 +2 lines]");
    }

    #[test]
    fn crlf_and_bare_cr_normalize_like_unix_newlines() {
        let crlf = pasted_lines_token("a\r\nb\r\nc", 2).unwrap();
        let cr = pasted_lines_token("a\rb\rc", 2).unwrap();
        assert_eq!(crlf, "[Pasted text #2 +3 lines]");
        assert_eq!(cr, "[Pasted text #2 +3 lines]");
    }

    #[test]
    fn expand_replaces_token_with_stored_content() {
        let mut store = HashMap::new();
        store.insert(
            "[Pasted text #1 +3 lines]".to_string(),
            "a\nb\nc".to_string(),
        );
        let expanded = expand_paste_tokens("before [Pasted text #1 +3 lines] after", &store);
        assert_eq!(expanded, "before a\nb\nc after");
    }

    #[test]
    fn expand_leaves_unknown_text_untouched() {
        let store = HashMap::new();
        let text = "no tokens here";
        assert_eq!(expand_paste_tokens(text, &store), text);
    }

    #[test]
    fn expand_replaces_multiple_distinct_tokens() {
        let mut store = HashMap::new();
        store.insert(
            "[Pasted text #1 +2 lines]".to_string(),
            "aa\nbb".to_string(),
        );
        store.insert(
            "[Pasted text #12 +2 lines]".to_string(),
            "cc\ndd".to_string(),
        );
        let expanded = expand_paste_tokens(
            "[Pasted text #1 +2 lines] and [Pasted text #12 +2 lines]",
            &store,
        );
        assert_eq!(expanded, "aa\nbb and cc\ndd");
    }
    #[test]
    fn placeholder_tokens_are_recognised() {
        assert!(is_placeholder_token("[Image #3]"));
        assert!(is_placeholder_token("[Pasted text #2 +14 lines]"));
        assert!(!is_placeholder_token("[Image #]"));
        assert!(!is_placeholder_token("[image #1]"));
        assert!(!is_placeholder_token("[link](x)"));
        let t = "see [Image #1] and [Pasted text #2 +3 lines]!";
        let r = placeholder_char_ranges(t);
        assert_eq!(r, vec![(4, 14), (19, 44)]);
        assert_eq!(placeholder_before(t, 14), Some((4, 14)));
        assert_eq!(placeholder_before(t, 13), None);
    }
}
