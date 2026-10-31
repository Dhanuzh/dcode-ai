//! Token use across sessions (`/stats`): one line per finished turn in
//! `~/.dcode-ai/usage.jsonl`, kept on this machine only.

use chrono::{DateTime, Duration, Local, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// One finished turn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TurnUsage {
    pub at: DateTime<Utc>,
    pub workspace: String,
    pub model: String,
    /// Every token processed: input, cache reads and writes, output.
    pub tokens: u64,
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
}
