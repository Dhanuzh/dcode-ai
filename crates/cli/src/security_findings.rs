//! Security review findings: parsed from the reviewer's report, written as
//! SARIF 2.1.0 for code scanning, and used for `dcode-ai ci --fail-on`.
//!
//! Following Shannon's "no exploit, no report" rule (ideas only), a finding
//! counts only when the reviewer traced it from untrusted input to the
//! dangerous call in the code (`[verified]`); others are `[unverified]`,
//! reported separately and never fail a pipeline.

use serde::Serialize;
use serde_json::{Value, json};
use std::sync::LazyLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Finding {
    pub severity: Severity,
    pub verified: bool,
    pub path: String,
    pub line: u32,
    pub message: String,
}

/// `[HIGH] [verified] src/db.rs:42 — SQL injection — …`
static LINE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)^\s*(?:[-*]\s*)?`?\[(high|medium|low)\]\s*(?:\[(verified|unverified)\]\s*)?`?\s*([^\s:`]+):(\d+)`?\s*(?:—|–|-{1,2})\s*(.+?)\s*`?$",
    )
    .expect("finding regex")
});

/// Findings in a reviewer's report (lines in the format the security
/// reviewer is asked for; everything else is ignored).
pub fn parse(report: &str) -> Vec<Finding> {
    report
        .lines()
        .filter_map(|l| {
            let c = LINE.captures(l)?;
            let severity = match c[1].to_ascii_lowercase().as_str() {
                "high" => Severity::High,
                "medium" => Severity::Medium,
                _ => Severity::Low,
            };
            Some(Finding {
                severity,
                verified: c
                    .get(2)
                    .is_some_and(|v| v.as_str().eq_ignore_ascii_case("verified")),
                path: c[3].to_string(),
                line: c[4].parse().unwrap_or(1).max(1),
                message: c[5].to_string(),
            })
        })
        .collect()
}

/// Whether a verified finding reaches `threshold`.
pub fn fails(findings: &[Finding], threshold: Severity) -> bool {
    findings
        .iter()
        .any(|f| f.verified && f.severity >= threshold)
}

/// SARIF 2.1.0 for GitHub code scanning and other tools.
pub fn sarif(findings: &[Finding]) -> Value {
    let results: Vec<Value> = findings
        .iter()
        .map(|f| {
            let level = match f.severity {
                Severity::High => "error",
                Severity::Medium => "warning",
                Severity::Low => "note",
            };
            json!({
                "ruleId": "dcode-ai/security-review",
                "level": level,
                "message": {"text": if f.verified {
                    f.message.clone()
                } else {
                    format!("(unverified) {}", f.message)
                }},
                "locations": [{"physicalLocation": {
                    "artifactLocation": {"uri": f.path},
                    "region": {"startLine": f.line},
                }}],
                "properties": {"verified": f.verified, "severity": f.severity},
            })
        })
        .collect();
    json!({
        "$schema": "https://json.schemastore.org/sarif-2.1.0.json",
        "version": "2.1.0",
        "runs": [{
            "tool": {"driver": {
                "name": "dcode-ai",
                "informationUri": "https://github.com/Dhanuzh/dcode-ai",
                "version": env!("CARGO_PKG_VERSION"),
                "rules": [{
                    "id": "dcode-ai/security-review",
                    "shortDescription": {"text": "Security issue found by dcode-ai's security review"},
                }],
            }},
            "results": results,
        }],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = "\
Verified:
- [HIGH] [verified] src/db.rs:42 — SQL injection — `name` from the query string reaches `format!` in the SQL — use bind parameters
[MEDIUM] [verified] web/app.ts:7 - XSS - innerHTML of the comment
Needs checking:
- [HIGH] [unverified] src/auth.rs:10 — token compared with == — timing
- [LOW] src/log.rs:3 — logs the user's email
Checked: the diff, routes, auth.";

    #[test]
    fn parses_findings_with_their_verification() {
        let f = parse(REPORT);
        assert_eq!(f.len(), 4);
        assert_eq!(
            (f[0].severity, f[0].verified, f[0].path.as_str(), f[0].line),
            (Severity::High, true, "src/db.rs", 42)
        );
        assert!(f[1].verified && f[1].severity == Severity::Medium);
        assert!(!f[2].verified, "unverified stays unverified");
        assert!(!f[3].verified, "no label counts as unverified");
    }

    #[test]
    fn only_verified_findings_fail_the_pipeline() {
        let f = parse(REPORT);
        assert!(fails(&f, Severity::High));
        let unverified_high = parse("[HIGH] [unverified] a.rs:1 — maybe");
        assert!(!fails(&unverified_high, Severity::Low));
        assert!(parse("No security issues found.").is_empty());
    }

    #[test]
    fn sarif_has_locations_and_levels() {
        let s = sarif(&parse(REPORT));
        assert_eq!(s["version"], "2.1.0");
        let r = &s["runs"][0]["results"];
        assert_eq!(r.as_array().unwrap().len(), 4);
        assert_eq!(r[0]["level"], "error");
        assert_eq!(
            r[0]["locations"][0]["physicalLocation"]["region"]["startLine"],
            42
        );
        assert!(
            r[2]["message"]["text"]
                .as_str()
                .unwrap()
                .starts_with("(unverified)")
        );
    }
}
