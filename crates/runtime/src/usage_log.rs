//! Token use across sessions (`/stats`): one line per finished turn in
//! `~/.dcode-ai/usage.jsonl`, kept on this machine only.

use chrono::{DateTime, Duration, Local, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// One finished turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TurnUsage {
    pub at: DateTime<Utc>,
    pub workspace: String,
    pub model: String,
    /// Every token processed: input, cache reads and writes, output.
    pub tokens: u64,
    /// The split, when known (lines from older versions have only `tokens`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub input: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub output: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cache_read: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cache_write: u64,
    /// At API list prices.
    #[serde(default, skip_serializing_if = "is_zero_f")]
    pub cost_usd: f64,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub session: String,
    /// The parent session, for a sub-agent's turn.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub parent: String,
    /// Code Mode in this turn: scripts run, tool calls made inside them, and
    /// the tokens their intermediate results would have added to the
    /// conversation (inner results minus what the scripts returned).
    #[serde(default, skip_serializing_if = "is_zero32")]
    pub scripts: u32,
    #[serde(default, skip_serializing_if = "is_zero32")]
    pub script_calls: u32,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub script_kept_out: u64,
}

fn is_zero32(n: &u32) -> bool {
    *n == 0
}

/// Below this, on average per script, Code Mode isn't paying for itself.
const CODE_MODE_MIN_KEPT_OUT: u64 = 300;
/// Scripts needed before judging.
const CODE_MODE_SAMPLE: u32 = 20;

/// Code Mode's record in `entries`: (scripts, inner calls, tokens kept out).
pub fn code_mode_totals(entries: &[TurnUsage]) -> (u32, u32, u64) {
    entries.iter().fold((0, 0, 0), |(s, c, k), e| {
        (s + e.scripts, c + e.script_calls, k + e.script_kept_out)
    })
}

/// Why Code Mode should be off, judged on the last 30 days: after enough
/// scripts, they kept too little out of the conversation to be worth it.
pub fn code_mode_verdict(entries: &[TurnUsage], now: DateTime<Utc>) -> Option<String> {
    let recent: Vec<TurnUsage> = entries
        .iter()
        .filter(|e| now - e.at <= Duration::days(30))
        .cloned()
        .collect();
    let (scripts, _, kept) = code_mode_totals(&recent);
    (scripts >= CODE_MODE_SAMPLE && kept / u64::from(scripts) < CODE_MODE_MIN_KEPT_OUT).then(|| {
        format!(
            "Code Mode is off: its last {scripts} scripts kept only ~{} tokens each out of the conversation — not worth a script ([harness] code_mode_auto = false keeps it on)",
            kept / u64::from(scripts)
        )
    })
}

fn is_zero(n: &u64) -> bool {
    *n == 0
}

fn is_zero_f(n: &f64) -> bool {
    *n == 0.0
}

/// `/cost today|week|month`: tokens, cache hits and cost for the period, by
/// day and model, with sub-agents counted separately.
pub fn cost_lines(entries: &[TurnUsage], today: NaiveDate, days: i64) -> Vec<String> {
    let since = today - Duration::days(days - 1);
    let picked: Vec<&TurnUsage> = entries
        .iter()
        .filter(|e| {
            let d = e.at.with_timezone(&Local).date_naive();
            d >= since && d <= today
        })
        .collect();
    let period = match days {
        1 => "today".to_string(),
        7 => "the last 7 days".to_string(),
        n => format!("the last {n} days"),
    };
    if picked.is_empty() {
        return vec![format!("No turns recorded {period}.")];
    }
    #[derive(Default)]
    struct Sum {
        turns: u32,
        tokens: u64,
        input: u64,
        output: u64,
        read: u64,
        write: u64,
        cost: f64,
        split: bool,
    }
    impl Sum {
        fn add(&mut self, e: &TurnUsage) {
            self.turns += 1;
            self.tokens += e.tokens;
            self.input += e.input;
            self.output += e.output;
            self.read += e.cache_read;
            self.write += e.cache_write;
            self.cost += e.cost_usd;
            self.split |= e.input + e.cache_read + e.cache_write > 0;
        }
        fn hit(&self) -> Option<u64> {
            (self.read * 100).checked_div(self.input + self.read + self.write)
        }
    }
    let mut all = Sum::default();
    let mut agents = Sum::default();
    let mut by_model: BTreeMap<String, Sum> = BTreeMap::new();
    let mut by_day: BTreeMap<NaiveDate, Sum> = BTreeMap::new();
    for e in &picked {
        all.add(e);
        if !e.parent.is_empty() {
            agents.add(e);
        }
        by_model.entry(e.model.clone()).or_default().add(e);
        by_day
            .entry(e.at.with_timezone(&Local).date_naive())
            .or_default()
            .add(e);
    }
    let money = |c: f64| {
        if c > 0.0 {
            format!("  ~${c:.2}")
        } else {
            String::new()
        }
    };
    let hit = |s: &Sum| s.hit().map(|h| format!("  cache {h}%")).unwrap_or_default();
    let mut lines = vec![
        format!(
            "Usage {period}: {} tokens in {} turns{}{}",
            fmt_tokens(all.tokens),
            all.turns,
            hit(&all),
            money(all.cost)
        ),
        String::new(),
    ];
    if all.split {
        lines.push(format!(
            "  fresh input {:>8} · cache reads {:>8} · cache writes {:>8} · output {:>8}",
            fmt_tokens(all.input),
            fmt_tokens(all.read),
            fmt_tokens(all.write),
            fmt_tokens(all.output)
        ));
        if let Some(h) = all.hit()
            && h < 50
        {
            lines.push(format!(
                "  ⚠ only {h}% of input came from the cache — long idle gaps, model switches and changing tools re-send everything at full price"
            ));
        }
        lines.push(String::new());
    }
    if by_day.len() > 1 {
        lines.push("By day:".into());
        for (d, s) in &by_day {
            lines.push(format!(
                "  {} {:>8}{}{}",
                d.format("%a %d %b"),
                fmt_tokens(s.tokens),
                hit(s),
                money(s.cost)
            ));
        }
        lines.push(String::new());
    }
    lines.push("By model:".into());
    let mut models: Vec<(&String, &Sum)> = by_model.iter().collect();
    models.sort_by_key(|(_, s)| std::cmp::Reverse(s.tokens));
    for (m, s) in models {
        lines.push(format!(
            "  {m:<28} {:>8}{}{}",
            fmt_tokens(s.tokens),
            hit(s),
            money(s.cost)
        ));
    }
    if agents.turns > 0 {
        lines.push(String::new());
        lines.push(format!(
            "Sub-agents: {} tokens in {} runs ({}% of the total){}",
            fmt_tokens(agents.tokens),
            agents.turns,
            (agents.tokens * 100).checked_div(all.tokens).unwrap_or(0),
            money(agents.cost)
        ));
    }
    let owned: Vec<TurnUsage> = picked.iter().map(|e| (*e).clone()).collect();
    let (scripts, calls, kept) = code_mode_totals(&owned);
    if scripts > 0 {
        lines.push(String::new());
        lines.push(format!(
            "Code Mode: {scripts} scripts ran {calls} tool calls and kept ~{} tokens of their results out of the conversation (~{} per script) — each of those tokens would otherwise be re-read on every later turn",
            fmt_tokens(kept),
            fmt_tokens(kept / u64::from(scripts))
        ));
    }
    lines
}

/// The log is cut back to this many lines when it grows past twice that.
const KEEP_LINES: usize = 20_000;

pub fn log_path() -> Option<PathBuf> {
    dcode_ai_common::config::dcode_ai_home_dir().map(|h| h.join("usage.jsonl"))
}

/// Append a turn to the log. Failures are ignored: stats are a convenience.
pub fn record(entry: &TurnUsage) {
    if cfg!(test) || entry.tokens == 0 {
        return;
    }
    if let Some(path) = log_path() {
        append(&path, entry);
    }
}

fn append(path: &Path, entry: &TurnUsage) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(line) = serde_json::to_string(entry) else {
        return;
    };
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(f, "{line}");
    }
    // Keep the file small.
    if std::fs::metadata(path).is_ok_and(|m| m.len() > (KEEP_LINES as u64) * 2 * 200)
        && let Ok(text) = std::fs::read_to_string(path)
    {
        let lines: Vec<&str> = text.lines().collect();
        if lines.len() > KEEP_LINES * 2 {
            let kept = lines[lines.len() - KEEP_LINES..].join("\n") + "\n";
            let _ = std::fs::write(path, kept);
        }
    }
}

pub fn load(path: &Path) -> Vec<TurnUsage> {
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        })
        .unwrap_or_default()
}

fn fmt_tokens(n: u64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n if n >= 1_000 => format!("{:.1}k", n as f64 / 1e3),
        n => n.to_string(),
    }
}

/// `/stats` lines: tokens per day for the last `days` days (today last) and
/// per project over the same span, busiest first.
#[cfg(test)]
mod code_mode_tests {
    use super::*;

    fn turn(scripts: u32, kept: u64) -> TurnUsage {
        TurnUsage {
            at: Utc::now(),
            scripts,
            script_calls: scripts * 3,
            script_kept_out: kept,
            ..Default::default()
        }
    }

    #[test]
    fn code_mode_is_judged_on_what_it_keeps_out() {
        let now = Utc::now();
        // Too few scripts to judge.
        assert!(code_mode_verdict(&[turn(5, 0)], now).is_none());
        // Plenty kept out: stays on.
        assert!(code_mode_verdict(&[turn(25, 25 * 4_000)], now).is_none());
        // Many scripts, little kept out: off.
        let v = code_mode_verdict(&[turn(25, 25 * 50)], now).unwrap();
        assert!(v.contains("Code Mode is off"), "{v}");
        let lines =
            cost_lines(&[turn(4, 8_000)], now.with_timezone(&Local).date_naive(), 1).join("\n");
        assert!(
            lines.contains("Code Mode: 4 scripts ran 12 tool calls"),
            "{lines}"
        );
    }
}

pub fn summary_lines(entries: &[TurnUsage], today: NaiveDate, days: i64) -> Vec<String> {
    let since = today - Duration::days(days - 1);
    let mut per_day: BTreeMap<NaiveDate, (u64, u32)> = BTreeMap::new();
    let mut per_project: BTreeMap<String, u64> = BTreeMap::new();
    for e in entries {
        let day = e.at.with_timezone(&Local).date_naive();
        if day < since || day > today {
            continue;
        }
        let d = per_day.entry(day).or_default();
        d.0 += e.tokens;
        d.1 += 1;
        *per_project.entry(e.workspace.clone()).or_default() += e.tokens;
    }
    if per_day.is_empty() {
        return vec![format!("No turns recorded in the last {days} days yet.")];
    }
    let max = per_day.values().map(|d| d.0).max().unwrap_or(1).max(1);
    let total: u64 = per_day.values().map(|d| d.0).sum();
    let mut lines = vec![format!(
        "Tokens processed, last {days} days: {} in {} turns",
        fmt_tokens(total),
        per_day.values().map(|d| d.1).sum::<u32>()
    )];
    lines.push(String::new());
    let mut day = since;
    while day <= today {
        let (tokens, turns) = per_day.get(&day).copied().unwrap_or_default();
        let bar = "█".repeat(((tokens * 20).div_ceil(max)) as usize);
        lines.push(format!(
            "  {} {:<20} {:>7}  {}",
            day.format("%a %d %b"),
            bar,
            fmt_tokens(tokens),
            if turns == 0 {
                String::new()
            } else {
                format!("{turns} turn{}", if turns == 1 { "" } else { "s" })
            }
        ));
        day += Duration::days(1);
    }
    lines.push(String::new());
    lines.push("By project:".into());
    let mut projects: Vec<(String, u64)> = per_project.into_iter().collect();
    projects.sort_by_key(|p| std::cmp::Reverse(p.1));
    for (ws, tokens) in projects.iter().take(8) {
        let name = Path::new(ws)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| ws.clone());
        lines.push(format!("  {name:<28} {:>7}", fmt_tokens(*tokens)));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(days_ago: i64, ws: &str, tokens: u64) -> TurnUsage {
        TurnUsage {
            at: Utc::now() - Duration::days(days_ago),
            workspace: ws.into(),
            model: "m".into(),
            tokens,
            ..Default::default()
        }
    }

    #[test]
    fn totals_per_day_and_project() {
        let entries = [
            turn(0, "/w/app", 1_500_000),
            turn(0, "/w/app", 500_000),
            turn(1, "/w/site", 300_000),
            turn(40, "/w/old", 9_000_000),
        ];
        let today = Local::now().date_naive();
        let lines = summary_lines(&entries, today, 14);
        assert!(lines[0].contains("2.3M in 3 turns"), "{lines:?}");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("2.0M") && l.contains("2 turns"))
        );
        let app = lines.iter().position(|l| l.contains("app")).unwrap();
        let site = lines.iter().position(|l| l.contains("site")).unwrap();
        assert!(app < site, "busiest project first");
        assert!(
            !lines.iter().any(|l| l.contains("old")),
            "outside the window"
        );
    }

    #[test]
    fn appends_and_loads_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("usage.jsonl");
        append(&path, &turn(0, "/w/a", 10));
        append(&path, &turn(0, "/w/b", 20));
        let loaded = load(&path);
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[1].tokens, 20);
    }

    #[test]
    fn cost_shows_cache_hits_money_and_sub_agents() {
        let t = |model: &str, read: u64, fresh: u64, parent: &str| TurnUsage {
            at: Utc::now(),
            model: model.into(),
            tokens: read + fresh + 1_000,
            input: fresh,
            cache_read: read,
            output: 1_000,
            cost_usd: 0.25,
            parent: parent.into(),
            ..Default::default()
        };
        let entries = [
            t("claude-opus-5-5", 90_000, 10_000, ""),
            t("claude-opus-5-5", 80_000, 20_000, ""),
            t("claude-haiku-5-5", 0, 50_000, "s1"),
        ];
        let lines = cost_lines(&entries, Local::now().date_naive(), 1);
        assert!(
            lines[0].starts_with("Usage today: 253.0k tokens in 3 turns"),
            "{lines:?}"
        );
        assert!(
            lines[0].contains("cache 68%") && lines[0].contains("~$0.75"),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("claude-opus-5-5") && l.contains("cache 85%"))
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("Sub-agents: 51.0k tokens in 1 runs"))
        );
        // Old lines (tokens only) still count, without a split.
        let old = [TurnUsage {
            at: Utc::now(),
            tokens: 5_000,
            model: "m".into(),
            ..Default::default()
        }];
        let lines = cost_lines(&old, Local::now().date_naive(), 7);
        assert!(lines[0].contains("5.0k tokens in 1 turns"), "{lines:?}");
    }
}
