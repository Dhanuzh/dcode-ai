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
    (line_count > 1).then(|| format!("[pasted {line_count} lines #{counter}]"))
}

/// Replace each placeholder token in `text` with its stored full content.
pub(crate) fn expand_paste_tokens(text: &str, store: &HashMap<String, String>) -> String {
    let mut result = text.to_string();
    for (token, content) in store {
        result = result.replace(token.as_str(), content.as_str());
    }
    result
}
