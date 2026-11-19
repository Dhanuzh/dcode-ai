//! `model = "auto"`: each request goes to a cheap, fast model or a strong
//! one by what it needs. Quick questions, look-ups and small edits take the
//! cheap one; planning, debugging, building and long or open-ended requests
//! take the strong one. A run that fails, comes back empty, or keeps failing
//! its tools on the cheap model moves up to the strong one.

/// The two models auto mode chooses between.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Routing {
    pub cheap: String,
    pub strong: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Cheap,
    Strong,
}

/// Whether `model` asks for auto routing.
pub fn is_auto(model: &str) -> bool {
    model.trim().eq_ignore_ascii_case("auto")
}

impl Routing {
    pub fn model(&self, tier: Tier) -> &str {
        match tier {
            Tier::Cheap => &self.cheap,
            Tier::Strong => &self.strong,
        }
    }
}

/// Words that mean real work: the strong model.
const STRONG: &[&str] = &[
    "plan",
    "design",
    "architect",
    "refactor",
    "implement",
    "build",
    "migrate",
    "debug",
    "investigate",
    "why ",
    "root cause",
    "optimi",
    "performance",
    "security",
    "review",
    "write tests",
    "add tests",
    "feature",
    "fix the",
    "fix this",
    "bug",
    "crash",
    "failing",
    "every ",
    "all the ",
    "across",
    "rewrite",
    "/fleet",
];

/// Openings of quick questions and look-ups: the cheap model.
const QUICK_START: &[&str] = &[
    "what ",
    "what's",
    "where ",
    "which ",
    "who ",
    "when ",
    "list ",
    "show ",
    "find ",
    "read ",
    "open ",
    "how many",
    "is there",
    "does ",
    "do we",
    "explain ",
    "summarize",
    "summarise",
    "rename ",
    "format ",
    "commit",
    "typo",
    "translate",
    "hello",
    "thanks",
];

/// The tier a new request starts on, and why (shown to the user).
pub fn classify(text: &str) -> (Tier, &'static str) {
    let t = text.trim().to_lowercase();
    if t.chars().count() > 400 || t.lines().count() > 6 || t.contains("```") {
        return (Tier::Strong, "a long request");
    }
    if STRONG.iter().any(|w| t.contains(w)) {
        return (Tier::Strong, "real work");
    }
    if t.chars().count() <= 160 && QUICK_START.iter().any(|w| t.starts_with(w)) {
        return (Tier::Cheap, "a quick one");
    }
    if t.chars().count() <= 60 {
        return (Tier::Cheap, "a short one");
    }
    (Tier::Strong, "to be safe")
}

/// The cheap and strong defaults for a provider (by its config key), when
/// `[model.routing]` doesn't name them.
pub fn defaults_for(provider: &str, current_local_model: &str) -> Routing {
    match provider {
        "copilot" => Routing {
            cheap: "gpt-5-mini".into(),
            strong: "claude-sonnet-5".into(),
        },
        "local" => Routing {
            cheap: current_local_model.into(),
            strong: current_local_model.into(),
        },
        _ => Routing {
            cheap: "claude-haiku-5-5".into(),
            strong: "claude-opus-5-5".into(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quick_questions_go_cheap_and_work_goes_strong() {
        assert_eq!(classify("what does run_script do?").0, Tier::Cheap);
        assert_eq!(classify("where is the config loaded").0, Tier::Cheap);
        assert_eq!(classify("thanks!").0, Tier::Cheap);
        assert_eq!(classify("refactor the auth module").0, Tier::Strong);
        assert_eq!(
            classify("why does the login test fail on CI?").0,
            Tier::Strong
        );
        assert_eq!(
            classify("what is the bug in parse()?").0,
            Tier::Strong,
            "bug wins"
        );
        assert_eq!(classify(&"x ".repeat(300)).0, Tier::Strong);
        assert_eq!(classify("```rust\nfn a(){}\n```").0, Tier::Strong);
        assert_eq!(
            classify("please make the sidebar a little wider and change its colour to the accent")
                .0,
            Tier::Strong,
            "unclear and longer: strong"
        );
    }

    #[test]
    fn auto_and_defaults() {
        assert!(is_auto("Auto") && !is_auto("claude-opus-5-5"));
        let r = defaults_for("anthropic", "");
        assert_eq!(r.model(Tier::Cheap), "claude-haiku-5-5");
        assert_eq!(r.model(Tier::Strong), "claude-opus-5-5");
        let l = defaults_for("local", "qwen3-coder");
        assert_eq!(l.cheap, l.strong);
    }
}
