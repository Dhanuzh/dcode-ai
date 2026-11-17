//! `/recap`: a card (and a few-second video) of what you and the agent did
//! today — sessions, commits, lines changed, tokens and cost — and the
//! achievements behind it: day streaks, commits, bugs closed. New ones are
//! announced once.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Today's numbers.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Day {
    pub sessions: usize,
    pub requests: usize,
    pub commits: usize,
    pub added: usize,
    pub removed: usize,
    pub files: usize,
    pub tokens: u64,
    pub cost: f64,
}

/// All-time numbers behind the achievements.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Totals {
    /// Days in a row (up to today) with dcode-ai used.
    pub streak: usize,
    pub commits: usize,
    /// Commits whose subject says it fixed something.
    pub fixes: usize,
    pub days: usize,
}

fn git(ws: &Path, args: &[&str]) -> String {
    Command::new("git")
        .current_dir(ws)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

fn me(ws: &Path) -> String {
    git(ws, &["config", "user.email"]).trim().to_string()
}

/// Local dates (`YYYY-MM-DD`) of each usage-log entry, with its tokens,
/// cost and session.
fn usage() -> Vec<(String, u64, f64, String)> {
    let Some(f) = dcode_ai_common::config::dcode_ai_home_dir().map(|d| d.join("usage.jsonl"))
    else {
        return Vec::new();
    };
    std::fs::read_to_string(f)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let v: serde_json::Value = serde_json::from_str(l).ok()?;
            let at = chrono::DateTime::parse_from_rfc3339(v["at"].as_str()?).ok()?;
            Some((
                at.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d")
                    .to_string(),
                v["tokens"].as_u64().unwrap_or(0),
                v["cost_usd"].as_f64().unwrap_or(0.0),
                v["session"].as_str().unwrap_or("").to_string(),
            ))
        })
        .collect()
}

/// Days in a row ending today (or yesterday, when today has nothing yet).
pub fn streak(days: &BTreeSet<String>, today: chrono::NaiveDate) -> usize {
    let has = |d: chrono::NaiveDate| days.contains(&d.format("%Y-%m-%d").to_string());
    let mut d = if has(today) {
        today
    } else {
        today - chrono::Duration::days(1)
    };
    let mut n = 0;
    while has(d) {
        n += 1;
        d -= chrono::Duration::days(1);
    }
    n
}

pub fn today(ws: &Path) -> Day {
    let date = chrono::Local::now().format("%Y-%m-%d").to_string();
    let entries: Vec<_> = usage().into_iter().filter(|e| e.0 == date).collect();
    let sessions: BTreeSet<&String> = entries.iter().map(|e| &e.3).collect();
    let author = me(ws);
    let author_arg = format!("--author={author}");
    let mut args = vec![
        "log",
        "--since=midnight",
        "--no-merges",
        "--pretty=format:%H",
    ];
    if !author.is_empty() {
        args.push(&author_arg);
    }
    let commits = git(ws, &args).lines().filter(|l| !l.is_empty()).count();
    let mut stat_args = vec![
        "log",
        "--since=midnight",
        "--no-merges",
        "--numstat",
        "--pretty=format:",
    ];
    if !author.is_empty() {
        stat_args.push(&author_arg);
    }
    let (mut added, mut removed) = (0, 0);
    let mut files = BTreeSet::new();
    for l in git(ws, &stat_args).lines() {
        let mut p = l.split('\t');
        if let (Some(a), Some(r), Some(f)) = (p.next(), p.next(), p.next()) {
            added += a.parse::<usize>().unwrap_or(0);
            removed += r.parse::<usize>().unwrap_or(0);
            files.insert(f.to_string());
        }
    }
    Day {
        sessions: sessions.len(),
        requests: entries.len(),
        commits,
        added,
        removed,
        files: files.len(),
        tokens: entries.iter().map(|e| e.1).sum(),
        cost: entries.iter().map(|e| e.2).sum(),
    }
}

pub fn totals(ws: &Path) -> Totals {
    let days: BTreeSet<String> = usage().into_iter().map(|e| e.0).collect();
    let author = me(ws);
    let author_arg = format!("--author={author}");
    let mut args = vec!["log", "--no-merges", "--pretty=format:%s"];
    if !author.is_empty() {
        args.push(&author_arg);
    }
    let subjects = git(ws, &args);
    let fix = |s: &str| {
        let s = s.to_ascii_lowercase();
        s.starts_with("fix") || s.contains(" fix") || s.contains("bug") || s.contains("fixed")
    };
    Totals {
        streak: streak(&days, chrono::Local::now().date_naive()),
        commits: subjects.lines().filter(|l| !l.is_empty()).count(),
        fixes: subjects.lines().filter(|l| fix(l)).count(),
        days: days.len(),
    }
}

/// Achievements earned for these totals: (id, what it says).
pub fn earned(t: &Totals) -> Vec<(String, String)> {
    let mut v = Vec::new();
    for (n, name) in [
        (3, "3-day streak"),
        (7, "Week streak"),
        (30, "Month streak"),
    ] {
        if t.streak >= n {
            v.push((format!("streak-{n}"), format!("🔥 {name}")));
        }
    }
    for n in [10, 100, 500, 1000] {
        if t.commits >= n {
            v.push((format!("commits-{n}"), format!("📦 {n} commits")));
        }
    }
    for n in [1, 10, 50, 100] {
        if t.fixes >= n {
            v.push((
                format!("fixes-{n}"),
                if n == 1 {
                    "🐛 First bug closed".to_string()
                } else {
                    format!("🐛 {n} bugs closed")
                },
            ));
        }
    }
    for n in [10, 100] {
        if t.days >= n {
            v.push((format!("days-{n}"), format!("📅 {n} days with dcode-ai")));
        }
    }
    v
}

/// The highest achievement of each kind (streak, commits, bugs, days).
pub fn best(t: &Totals) -> Vec<String> {
    let mut seen = BTreeSet::new();
    earned(t)
        .into_iter()
        .rev()
        .filter(|(id, _)| seen.insert(id.split('-').next().unwrap_or("").to_string()))
        .map(|(_, text)| text)
        .collect()
}

fn seen_file() -> Option<PathBuf> {
    dcode_ai_common::config::dcode_ai_home_dir().map(|d| d.join("achievements.json"))
}

/// Achievements not announced before (and remembers them as announced).
pub fn new_achievements(ws: &Path) -> Vec<String> {
    let Some(file) = seen_file() else {
        return Vec::new();
    };
    let mut seen: BTreeSet<String> = std::fs::read_to_string(&file)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    let first_time = !file.exists();
    let mut fresh = Vec::new();
    for (id, text) in earned(&totals(ws)) {
        if seen.insert(id) && !first_time {
            fresh.push(text);
        }
    }
    let _ = std::fs::write(&file, serde_json::to_string(&seen).unwrap_or_default());
    fresh
}

fn compact(n: u64) -> String {
    match n {
        n if n >= 1_000_000 => format!("{:.1}M", n as f64 / 1e6),
        n if n >= 1_000 => format!("{:.1}k", n as f64 / 1e3),
        n => n.to_string(),
    }
}

/// Lines for the transcript.
pub fn lines(d: &Day, t: &Totals) -> Vec<String> {
    let mut v = vec![
        format!(
            "today: {} session(s), {} request(s), {} commit(s), +{} −{} lines in {} file(s), {} tokens, ${:.2}",
            d.sessions,
            d.requests,
            d.commits,
            d.added,
            d.removed,
            d.files,
            compact(d.tokens),
            d.cost
        ),
        format!(
            "all time: {}-day streak, {} commits, {} bugs closed, {} days with dcode-ai",
            t.streak, t.commits, t.fixes, t.days
        ),
    ];
    let badges = best(t);
    if !badges.is_empty() {
        v.push(format!("achievements: {}", badges.join(" · ")));
    }
    v
}

/// The card (PNG) and, with ffmpeg, a 6-second video with a slow zoom, in
/// `.dcode-ai/recaps/`.
pub fn write_card(ws: &Path, d: &Day, t: &Totals) -> Result<Vec<PathBuf>, String> {
    let project = ws
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stats = vec![
        (d.commits.to_string(), "commits".to_string()),
        (
            format!("+{}", d.added),
            format!("lines added (−{})", d.removed),
        ),
        (d.files.to_string(), "files changed".to_string()),
        (d.sessions.to_string(), "sessions".to_string()),
        (compact(d.tokens), "tokens".to_string()),
        (format!("{} 🔥", t.streak), "day streak".to_string()),
    ];
    // The card's font has no emoji: plain badges.
    let badges: Vec<String> = best(t)
        .into_iter()
        .map(|b| {
            b.chars()
                .skip_while(|c| !c.is_ascii_alphanumeric())
                .collect()
        })
        .collect();
    let stats: Vec<(String, String)> = stats
        .into_iter()
        .map(|(a, b)| (a.replace(" 🔥", ""), b))
        .collect();
    let img = dcode_ai_core::video::render::recap_card(
        &format!("Today in {project}"),
        &chrono::Local::now().format("%A, %e %B %Y").to_string(),
        &stats,
        &badges,
    );
    let dir = ws.join(".dcode-ai/recaps");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let stem = chrono::Local::now().format("recap-%Y-%m-%d").to_string();
    let png = dir.join(format!("{stem}.png"));
    img.save(&png).map_err(|e| e.to_string())?;
    let mut out = vec![png.clone()];
    if let Some(bin) = dcode_ai_core::video::ffmpeg() {
        let mp4 = dir.join(format!("{stem}.mp4"));
        let ok = Command::new(bin)
            .args(["-loglevel", "error", "-y", "-loop", "1", "-i"])
            .arg(&png)
            .args([
                "-vf",
                "scale=2400:-2,zoompan=z='min(zoom+0.0006,1.06)':x='iw/2-(iw/zoom/2)':y='ih/2-(ih/zoom/2)':d=180:s=1200x676:fps=30,fade=t=in:d=0.5,fade=t=out:st=5.5:d=0.5",
                "-t",
                "6",
                "-c:v",
                "libx264",
                "-pix_fmt",
                "yuv420p",
            ])
            .arg(&mp4)
            .stdin(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            out.push(mp4);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaks_count_days_in_a_row() {
        let days: BTreeSet<String> = ["2026-11-15", "2026-11-16", "2026-11-17", "2026-11-10"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let d = |s: &str| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        assert_eq!(streak(&days, d("2026-11-17")), 3);
        // Nothing yet today: yesterday's streak still counts.
        assert_eq!(streak(&days, d("2026-11-18")), 3);
        assert_eq!(streak(&days, d("2026-11-20")), 0);
    }

    #[test]
    fn achievements_follow_the_totals() {
        let t = Totals {
            streak: 7,
            commits: 120,
            fixes: 1,
            days: 12,
        };
        let ids: Vec<String> = earned(&t).into_iter().map(|e| e.0).collect();
        assert!(ids.contains(&"streak-7".into()) && ids.contains(&"streak-3".into()));
        assert!(!ids.contains(&"streak-30".into()));
        assert!(ids.contains(&"commits-100".into()) && !ids.contains(&"commits-500".into()));
        assert!(ids.contains(&"fixes-1".into()));
        assert!(ids.contains(&"days-10".into()));
    }

    #[test]
    fn a_card_is_written() {
        let d = tempfile::tempdir().unwrap();
        let files = write_card(
            d.path(),
            &Day {
                commits: 3,
                added: 120,
                ..Default::default()
            },
            &Totals {
                streak: 4,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(files[0].is_file());
        let img = image::open(&files[0]).unwrap();
        assert_eq!((img.width(), img.height()), (1200, 675));
    }
}
