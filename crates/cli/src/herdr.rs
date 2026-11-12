//! [herdr](https://herdr.dev) support: inside a herdr pane, dcode-ai reports
//! its own state (working / idle / blocked), what it's working on, and its
//! session (so herdr's session restore reopens it) — herdr's "add herdr
//! support to an agent" protocol, through the `herdr` binary it puts in
//! every pane. Outside herdr nothing happens.
//!
//! Reports run on a background thread with a short timeout; only the latest
//! state is sent and failures are ignored, so herdr never slows the TUI.

use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SOURCE: &str = "dcode-ai";
const AGENT: &str = "dcode-ai";
const TIMEOUT: Duration = Duration::from_secs(2);

/// What dcode-ai is doing, as herdr names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Working,
    Idle,
    Blocked,
}

impl State {
    fn as_str(self) -> &'static str {
        match self {
            State::Working => "working",
            State::Idle => "idle",
            State::Blocked => "blocked",
        }
    }
}

/// One report, as `herdr pane …` arguments (after the binary).
#[derive(Debug, Clone, PartialEq)]
enum Report {
    State {
        state: State,
        message: Option<String>,
        session: Option<String>,
    },
    Title(String),
    Release,
}

/// The pane dcode-ai runs in, when it runs inside herdr.
#[derive(Debug, Clone, PartialEq)]
pub struct Pane {
    pub bin: String,
    pub id: String,
}

impl Pane {
    /// From herdr's pane environment (`HERDR_ENV=1`, `HERDR_PANE_ID`,
    /// `HERDR_BIN_PATH`); `None` outside herdr.
    pub fn from_env(get: impl Fn(&str) -> Option<String>) -> Option<Self> {
        if get("HERDR_ENV").as_deref() != Some("1") {
            return None;
        }
        let id = get("HERDR_PANE_ID").filter(|s| !s.is_empty())?;
        let bin = get("HERDR_BIN_PATH").filter(|s| !s.is_empty())?;
        Some(Self { bin, id })
    }
}

/// Milliseconds since the epoch: increases across restarts, as herdr wants.
fn seq() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
        .to_string()
}

/// The `herdr` arguments for a report.
fn args(pane: &Pane, report: &Report, seq: &str) -> Vec<String> {
    let mut a: Vec<String> = vec!["pane".into()];
    let common = |a: &mut Vec<String>| {
        a.extend(
            ["--source", SOURCE, "--agent", AGENT]
                .iter()
                .map(|s| s.to_string()),
        );
    };
    match report {
        Report::State {
            state,
            message,
            session,
        } => {
            a.extend(["report-agent".into(), pane.id.clone()]);
            common(&mut a);
            a.extend([
                "--state".into(),
                state.as_str().into(),
                "--seq".into(),
                seq.into(),
            ]);
            if let Some(m) = message.as_ref().filter(|m| !m.is_empty()) {
                a.extend(["--message".into(), m.chars().take(200).collect()]);
            }
            // herdr's restore reopens the same conversation. The resume
            // command: a plain command name, no apostrophes, few arguments.
            if let Some(id) = session.as_ref().filter(|s| valid_session(s)) {
                a.extend([
                    "--agent-session-id".into(),
                    id.clone(),
                    "--".into(),
                    "dcode-ai".into(),
                    "--resume".into(),
                    id.clone(),
                ]);
            }
        }
        Report::Title(t) => {
            a.extend(["report-metadata".into(), pane.id.clone()]);
            common(&mut a);
            a.extend(["--title".into(), t.chars().take(80).collect()]);
        }
        Report::Release => {
            a.extend(["release-agent".into(), pane.id.clone()]);
            common(&mut a);
            a.extend(["--seq".into(), seq.into()]);
        }
    }
    a
}

fn valid_session(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Runs `bin args…`, waiting at most [`TIMEOUT`].
fn run(bin: &str, args: &[String]) {
    let Ok(mut child) = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) if start.elapsed() < TIMEOUT => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
        }
    }
}

/// Reports to herdr from a background thread. Cheap to call every frame:
/// only changes are sent, and a newer state replaces one still queued.
pub struct Reporter {
    pane: Pane,
    tx: mpsc::Sender<Report>,
    last_state: Option<(State, Option<String>, Option<String>)>,
    last_title: Option<String>,
}

impl Reporter {
    /// `None` outside herdr.
    pub fn from_env() -> Option<Self> {
        let pane = Pane::from_env(|k| std::env::var(k).ok())?;
        let (tx, rx) = mpsc::channel::<Report>();
        let worker = pane.clone();
        std::thread::spawn(move || {
            while let Ok(mut r) = rx.recv() {
                // Only the latest of each kind matters.
                let mut title = None;
                let mut state = None;
                let mut release = false;
                loop {
                    match r {
                        Report::Title(_) => title = Some(r),
                        Report::Release => release = true,
                        Report::State { .. } => state = Some(r),
                    }
                    match rx.try_recv() {
                        Ok(next) => r = next,
                        Err(_) => break,
                    }
                }
                for report in [state, title].into_iter().flatten() {
                    run(&worker.bin, &args(&worker, &report, &seq()));
                }
                if release {
                    run(&worker.bin, &args(&worker, &Report::Release, &seq()));
                    return;
                }
            }
        });
        Some(Self {
            pane,
            tx,
            last_state: None,
            last_title: None,
        })
    }

    /// The current state; sent only when it (or its message, or the
    /// session) changed.
    pub fn state(&mut self, state: State, message: Option<String>, session: Option<String>) {
        let now = (state, message.clone(), session.clone());
        if self.last_state.as_ref() == Some(&now) {
            return;
        }
        self.last_state = Some(now);
        let _ = self.tx.send(Report::State {
            state,
            message,
            session,
        });
    }

    /// What it's working on, for herdr's sidebar.
    pub fn title(&mut self, title: &str) {
        let t = title.trim();
        if t.is_empty() || self.last_title.as_deref() == Some(t) {
            return;
        }
        self.last_title = Some(t.to_string());
        let _ = self.tx.send(Report::Title(t.to_string()));
    }

    /// dcode-ai is quitting: hand the pane back (waits briefly so it goes out
    /// before the process ends).
    pub fn release(self) {
        run(&self.pane.bin, &args(&self.pane, &Report::Release, &seq()));
    }
}

/// The terminal title for a state: what tabs in tmux, WezTerm and Windows
/// Terminal show.
pub fn terminal_title(state: State, folder: &str, task: &str) -> String {
    let task: String = task.chars().take(40).collect();
    let task = task.trim();
    match (state, task.is_empty()) {
        (State::Working, false) => format!("✳ dcode-ai · {task}"),
        (State::Working, true) => format!("✳ dcode-ai · {folder}"),
        (State::Blocked, _) => format!("● dcode-ai needs you · {folder}"),
        (State::Idle, _) => format!("dcode-ai · {folder}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane() -> Pane {
        Pane {
            bin: "/opt/herdr".into(),
            id: "w1:p2".into(),
        }
    }

    #[test]
    fn only_inside_herdr() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        assert!(Pane::from_env(env(&[])).is_none());
        assert!(Pane::from_env(env(&[("HERDR_ENV", "1"), ("HERDR_PANE_ID", "p")])).is_none());
        assert_eq!(
            Pane::from_env(env(&[
                ("HERDR_ENV", "1"),
                ("HERDR_PANE_ID", "w1:p2"),
                ("HERDR_BIN_PATH", "/opt/herdr")
            ])),
            Some(pane())
        );
    }

    #[test]
    fn reports_follow_herdrs_protocol() {
        let a = args(
            &pane(),
            &Report::State {
                state: State::Blocked,
                message: Some("needs approval: edit_file".into()),
                session: Some("abc-123".into()),
            },
            "42",
        );
        assert_eq!(
            a,
            [
                "pane",
                "report-agent",
                "w1:p2",
                "--source",
                "dcode-ai",
                "--agent",
                "dcode-ai",
                "--state",
                "blocked",
                "--seq",
                "42",
                "--message",
                "needs approval: edit_file",
                "--agent-session-id",
                "abc-123",
                "--",
                "dcode-ai",
                "--resume",
                "abc-123"
            ]
        );
        let a = args(&pane(), &Report::Title("Fix the login".into()), "1");
        assert_eq!(a[1], "report-metadata");
        assert!(a.ends_with(&["--title".into(), "Fix the login".into()]));
        let a = args(&pane(), &Report::Release, "7");
        assert_eq!(&a[..3], ["pane", "release-agent", "w1:p2"]);
        // A session id that couldn't go on a command line is left out.
        let a = args(
            &pane(),
            &Report::State {
                state: State::Idle,
                message: None,
                session: Some("it's bad".into()),
            },
            "1",
        );
        assert!(!a.contains(&"--agent-session-id".to_string()));
    }

    #[test]
    fn titles_say_what_it_does() {
        assert_eq!(
            terminal_title(State::Working, "app", "Fix login"),
            "✳ dcode-ai · Fix login"
        );
        assert_eq!(
            terminal_title(State::Blocked, "app", ""),
            "● dcode-ai needs you · app"
        );
        assert_eq!(terminal_title(State::Idle, "app", "x"), "dcode-ai · app");
    }
}
