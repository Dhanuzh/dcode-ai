//! Parsing of user free-text replies in the TUI: approval verdicts (yes/no)
//! and interactive-question answers (suggested / numbered option / custom).
//! Extracted from `tui::app`.

use dcode_ai_common::event::QuestionSelection;

pub(crate) fn parse_approval_verdict(line: &str) -> Option<bool> {
    let mut s = line.trim().to_lowercase();
    while matches!(
        s.chars().last(),
        Some('.' | '!' | '?' | ',' | ';' | ':' | '"' | '\'')
    ) {
        s.pop();
    }
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // Slash commands (handled before this in caller for passthrough; bare forms here too)
    match s {
        "/approve" | "/y" | "/yes" | "/ok" => return Some(true),
        "/deny" | "/n" | "/no" => return Some(false),
        _ => {}
    }
    let word = s.split_whitespace().next()?;
    match word {
        "y" | "yes" | "ok" | "okay" | "approve" | "approved" | "allow" | "1" | "true" => Some(true),
        "n" | "no" | "deny" | "denied" | "reject" | "rejected" | "decline" | "declined" | "0"
        | "false" => Some(false),
        _ => None,
    }
}

pub(crate) fn parse_tui_question_answer(
    raw: &str,
    q: &dcode_ai_common::event::InteractiveQuestionPayload,
) -> Option<QuestionSelection> {
    let t = raw.trim();
    if t.is_empty() || t == "0" || t.eq_ignore_ascii_case("s") {
        return Some(QuestionSelection::Suggested);
    }
    if let Ok(n) = t.parse::<usize>()
        && n >= 1
        && n <= q.options.len()
    {
        return Some(QuestionSelection::Option {
            option_id: q.options[n - 1].id.clone(),
        });
    }
    if q.allow_custom && !t.is_empty() {
        return Some(QuestionSelection::Custom {
            text: t.to_string(),
        });
    }
    None
}
