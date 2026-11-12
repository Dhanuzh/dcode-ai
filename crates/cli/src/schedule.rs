//! Scheduled runs (`/schedule`): prompts that run on their own — the Jira
//! day every morning, the tests every night. Stored per project in
//! `.dcode-ai/schedules.json`; each run is a background session
//! (`dcode-ai run`), so it shows up in `/resume`.
//!
//! When: `daily 09:00`, `weekdays 09:00`, `mon 18:30` (or `monday`),
//! `every 30m` / `every 2h`, or a cron expression (`0 9 * * 1-5`). Times are
//! local.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Duration, Local, NaiveTime, TimeZone, Timelike, Weekday};
use serde::{Deserialize, Serialize};

/// When a schedule runs.
#[derive(Debug, Clone, PartialEq)]
pub enum When {
    Daily(NaiveTime),
    Weekdays(NaiveTime),
    Weekly(Weekday, NaiveTime),
    Every(Duration),
    Cron(Cron),
}

/// A five-field cron expression: minute hour day-of-month month day-of-week.
#[derive(Debug, Clone, PartialEq)]
pub struct Cron {
    minutes: Vec<u32>,
    hours: Vec<u32>,
    days: Vec<u32>,
    months: Vec<u32>,
    weekdays: Vec<u32>,
    /// Day-of-month and day-of-week both restricted: either matches (cron's rule).
    either_day: bool,
}

fn parse_field(f: &str, lo: u32, hi: u32) -> Option<Vec<u32>> {
    let mut out = Vec::new();
    for part in f.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (r, s.parse::<u32>().ok().filter(|s| *s > 0)?),
            None => (part, 1),
        };
        let (a, b) = if range == "*" {
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            (a.parse().ok()?, b.parse().ok()?)
        } else {
            let v: u32 = range.parse().ok()?;
            (v, if part.contains('/') { hi } else { v })
        };
        if a < lo || b > hi || a > b {
            return None;
        }
        out.extend((a..=b).step_by(step as usize));
    }
    out.sort_unstable();
    out.dedup();
    (!out.is_empty()).then_some(out)
}

impl Cron {
    pub fn parse(s: &str) -> Option<Self> {
        let f: Vec<&str> = s.split_whitespace().collect();
        if f.len() != 5 {
            return None;
        }
        // Sunday may be written 7.
        let mut weekdays = parse_field(f[4], 0, 7)?;
        if weekdays.contains(&7) {
            weekdays.retain(|d| *d != 7);
            weekdays.push(0);
            weekdays.sort_unstable();
            weekdays.dedup();
        }
        Some(Self {
            minutes: parse_field(f[0], 0, 59)?,
            hours: parse_field(f[1], 0, 23)?,
            days: parse_field(f[2], 1, 31)?,
            months: parse_field(f[3], 1, 12)?,
            weekdays,
            either_day: f[2] != "*" && f[4] != "*",
        })
    }

    fn day_matches(&self, t: &DateTime<Local>) -> bool {
        let dom = self.days.contains(&t.day());
        let dow = self.weekdays.contains(&t.weekday().num_days_from_sunday());
        if self.either_day {
            dom || dow
        } else {
            dom && dow
        }
    }

    /// The first matching minute after `from`.
    fn next_after(&self, from: DateTime<Local>) -> Option<DateTime<Local>> {
        let mut t = (from + Duration::minutes(1))
            .with_second(0)?
            .with_nanosecond(0)?;
        // A year of minutes at most (cron expressions that never match stop).
        for _ in 0..(366 * 24 * 60) {
            if self.months.contains(&t.month())
                && self.day_matches(&t)
                && self.hours.contains(&t.hour())
                && self.minutes.contains(&t.minute())
            {
                return Some(t);
            }
            // Jump whole hours or days when they can't match.
            t = if !self.months.contains(&t.month()) || !self.day_matches(&t) {
                let next_day = t.date_naive().succ_opt()?.and_hms_opt(0, 0, 0)?;
                Local.from_local_datetime(&next_day).earliest()?
            } else if !self.hours.contains(&t.hour()) {
                (t + Duration::hours(1)).with_minute(0)?
            } else {
                t + Duration::minutes(1)
            };
        }
        None
    }
}

fn parse_time(s: &str) -> Option<NaiveTime> {
    let s = s.trim().to_ascii_lowercase();
    let (s, pm, am) = if let Some(x) = s.strip_suffix("pm") {
        (x.trim().to_string(), true, false)
    } else if let Some(x) = s.strip_suffix("am") {
        (x.trim().to_string(), false, true)
    } else {
        (s, false, false)
    };
    let (h, m) = match s.split_once(':') {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None => (s.parse::<u32>().ok()?, 0),
    };
    let h = match (pm, am) {
        (true, _) if h < 12 => h + 12,
        (_, true) if h == 12 => 0,
        _ => h,
    };
    NaiveTime::from_hms_opt(h, m, 0)
}

fn parse_weekday(s: &str) -> Option<Weekday> {
    Some(match s.to_ascii_lowercase().get(..3)? {
        "mon" => Weekday::Mon,
        "tue" => Weekday::Tue,
        "wed" => Weekday::Wed,
        "thu" => Weekday::Thu,
        "fri" => Weekday::Fri,
        "sat" => Weekday::Sat,
        "sun" => Weekday::Sun,
        _ => return None,
    })
}

impl When {
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        if let Some(c) = Cron::parse(s) {
            return Some(When::Cron(c));
        }
        let words: Vec<&str> = s.split_whitespace().collect();
        match words.as_slice() {
            ["every", n] => {
                let n = n.to_ascii_lowercase();
                let (num, unit) = n.split_at(n.find(|c: char| !c.is_ascii_digit())?);
                let v: i64 = num.parse().ok().filter(|v| *v > 0)?;
                let d = match unit {
                    "m" | "min" | "mins" => Duration::minutes(v),
                    "h" | "hr" | "hour" | "hours" => Duration::hours(v),
                    "d" | "day" | "days" => Duration::days(v),
                    _ => return None,
                };
                (d >= Duration::minutes(5)).then_some(When::Every(d))
            }
            ["daily", t] | ["every", "day", t] => Some(When::Daily(parse_time(t)?)),
            ["weekdays", t] => Some(When::Weekdays(parse_time(t)?)),
            [day, t] => Some(When::Weekly(parse_weekday(day)?, parse_time(t)?)),
            _ => None,
        }
    }

    /// The next run after `from`.
    pub fn next_after(&self, from: DateTime<Local>) -> Option<DateTime<Local>> {
        let at = |date: chrono::NaiveDate, t: NaiveTime| {
            Local.from_local_datetime(&date.and_time(t)).earliest()
        };
        match self {
            When::Every(d) => Some(from + *d),
            When::Cron(c) => c.next_after(from),
            When::Daily(t) | When::Weekdays(t) | When::Weekly(_, t) => {
                let mut date = from.date_naive();
                for _ in 0..8 {
                    let ok = match self {
                        When::Weekdays(_) => !matches!(date.weekday(), Weekday::Sat | Weekday::Sun),
                        When::Weekly(w, _) => date.weekday() == *w,
                        _ => true,
                    };
                    if ok
                        && let Some(dt) = at(date, *t)
                        && dt > from
                    {
                        return Some(dt);
                    }
                    date = date.succ_opt()?;
                }
                None
            }
        }
    }
}

/// One scheduled prompt.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Schedule {
    pub id: u32,
    /// As written (`weekdays 09:00`).
    pub when: String,
    pub prompt: String,
    /// `plan` (read-only, the default) or `auto` (runs commands and edits;
    /// deletions and risky commands are still refused).
    #[serde(default = "plan")]
    pub mode: String,
    #[serde(default)]
    pub paused: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_run: Option<DateTime<Local>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run: Option<DateTime<Local>>,
    /// The last run's session, for `/resume`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_session: Option<String>,
}

fn plan() -> String {
    "plan".into()
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Schedules {
    #[serde(default)]
    pub items: Vec<Schedule>,
}

pub fn path(ws: &Path) -> PathBuf {
    ws.join(".dcode-ai").join("schedules.json")
}

impl Schedules {
    pub fn load(ws: &Path) -> Self {
        std::fs::read_to_string(path(ws))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, ws: &Path) -> std::io::Result<()> {
        let p = path(ws);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(
            p,
            serde_json::to_string_pretty(self).map_err(std::io::Error::other)?,
        )
    }

    /// Adds one; its id, or why `when` doesn't read.
    pub fn add(
        &mut self,
        when: &str,
        prompt: &str,
        mode: &str,
        now: DateTime<Local>,
    ) -> Result<u32, String> {
        let w = When::parse(when).ok_or_else(|| {
            format!("can't read “{when}” — daily 09:00 · weekdays 9am · mon 18:30 · every 2h · or cron like 0 9 * * 1-5")
        })?;
        let id = self.items.iter().map(|s| s.id).max().unwrap_or(0) + 1;
        self.items.push(Schedule {
            id,
            when: when.trim().to_string(),
            prompt: prompt.trim().to_string(),
            mode: mode.to_string(),
            paused: false,
            next_run: w.next_after(now),
            last_run: None,
            last_session: None,
        });
        Ok(id)
    }

    /// The schedules due at `now` (not paused, next run reached). A run
    /// missed while dcode-ai was closed counts once, not once per miss.
    pub fn due(&self, now: DateTime<Local>) -> Vec<u32> {
        self.items
            .iter()
            .filter(|s| !s.paused && s.next_run.is_some_and(|n| n <= now))
            .map(|s| s.id)
            .collect()
    }

    /// Record that `id` started now; its next run is the next one after now.
    pub fn started(&mut self, id: u32, session: &str, now: DateTime<Local>) {
        if let Some(s) = self.items.iter_mut().find(|s| s.id == id) {
            s.last_run = Some(now);
            s.last_session = Some(session.to_string());
            s.next_run = When::parse(&s.when).and_then(|w| w.next_after(now));
        }
    }
}

/// Starts a scheduled run: a background `dcode-ai run` session in `ws`.
/// Returns its session id and the process.
pub fn launch(ws: &Path, s: &Schedule) -> std::io::Result<(String, std::process::Child)> {
    let session_id = format!("session-{}", chrono::Utc::now().timestamp_millis());
    let sessions = ws.join(".dcode-ai").join("sessions");
    std::fs::create_dir_all(&sessions)?;
    let log = std::fs::File::create(sessions.join(format!("{session_id}.spawn.log")))?;
    let err = log.try_clone()?;
    let prompt = format!(
        "(Scheduled run #{} — {}. Nobody is watching: finish on your own and end with a short summary.)\n\n{}",
        s.id, s.when, s.prompt
    );
    let mode = if s.mode == "auto" { "auto" } else { "plan" };
    let child = std::process::Command::new(std::env::current_exe()?)
        .current_dir(ws)
        .args([
            "run",
            "--prompt",
            &prompt,
            "--stream",
            "ndjson",
            "--session-id",
            &session_id,
        ])
        .args(["--permission-mode", mode])
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(err)
        .spawn()?;
    Ok((session_id, child))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Local> {
        Local
            .with_ymd_and_hms(y, m, d, h, min, 0)
            .earliest()
            .unwrap()
    }

    #[test]
    fn plain_forms() {
        // 2026-11-13 is a Friday.
        let fri_10 = local(2026, 11, 13, 10, 0);
        let w = When::parse("weekdays 09:00").unwrap();
        assert_eq!(
            w.next_after(fri_10),
            Some(local(2026, 11, 16, 9, 0)),
            "over the weekend to Monday"
        );
        assert_eq!(
            When::parse("daily 9am").unwrap().next_after(fri_10),
            Some(local(2026, 11, 14, 9, 0))
        );
        assert_eq!(
            When::parse("daily 6:30pm").unwrap().next_after(fri_10),
            Some(local(2026, 11, 13, 18, 30))
        );
        assert_eq!(
            When::parse("mon 18:30").unwrap().next_after(fri_10),
            Some(local(2026, 11, 16, 18, 30))
        );
        assert_eq!(
            When::parse("every 2h").unwrap().next_after(fri_10),
            Some(local(2026, 11, 13, 12, 0))
        );
        assert!(When::parse("every 1m").is_none(), "too often");
        assert!(When::parse("sometimes").is_none());
    }

    #[test]
    fn cron_expressions() {
        let fri_10 = local(2026, 11, 13, 10, 0);
        let c = When::parse("0 9 * * 1-5").unwrap();
        assert_eq!(c.next_after(fri_10), Some(local(2026, 11, 16, 9, 0)));
        let every15 = When::parse("*/15 * * * *").unwrap();
        assert_eq!(
            every15.next_after(fri_10),
            Some(local(2026, 11, 13, 10, 15))
        );
        let first_of_month = When::parse("30 8 1 * *").unwrap();
        assert_eq!(
            first_of_month.next_after(fri_10),
            Some(local(2026, 12, 1, 8, 30))
        );
        assert!(Cron::parse("61 * * * *").is_none());
    }

    #[test]
    fn missed_runs_count_once_and_move_on() {
        let mut s = Schedules::default();
        let mon_8 = local(2026, 11, 16, 8, 0);
        let id = s
            .add(
                "daily 07:00",
                "summarize",
                "plan",
                local(2026, 11, 12, 8, 0),
            )
            .unwrap();
        // Closed for days: due once.
        assert_eq!(s.due(mon_8), vec![id]);
        s.started(id, "session-1", mon_8);
        assert!(s.due(mon_8).is_empty());
        assert_eq!(s.items[0].next_run, Some(local(2026, 11, 17, 7, 0)));
        s.items[0].paused = true;
        assert!(s.due(local(2026, 11, 18, 8, 0)).is_empty());
    }

    #[test]
    fn store_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Schedules::default();
        s.add("weekdays 09:00", "jira today", "plan", Local::now())
            .unwrap();
        s.save(dir.path()).unwrap();
        assert_eq!(Schedules::load(dir.path()), s);
    }
}
