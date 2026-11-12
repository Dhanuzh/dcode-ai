//! `/replay`: a session as a terminal recording — an asciinema v2 `.cast`
//! file (prompts typed, replies written out, tool calls as they ran), with
//! the real pauses shortened. `asciinema play`, `agg` (→ GIF) and
//! `dcode-ai replay <id> --play` read it.

use std::path::{Path, PathBuf};
use std::time::Duration;

use dcode_ai_common::event::{AgentEvent, EventEnvelope};

/// Longest pause kept between two moments, seconds.
const MAX_GAP: f64 = 1.5;
/// Typing a prompt: per character, and at most this long in all.
const TYPE_STEP: f64 = 0.03;
const TYPE_MAX: f64 = 2.0;
/// Writing a reply: per line, and at most this long in all.
const LINE_STEP: f64 = 0.04;
const REPLY_MAX: f64 = 4.0;

pub struct Options {
    pub width: usize,
    pub height: usize,
    /// 2.0 = twice as fast.
    pub speed: f64,
    pub title: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            width: 100,
            height: 30,
            speed: 1.0,
            title: "dcode-ai".into(),
        }
    }
}

struct Cast {
    t: f64,
    lines: Vec<String>,
    speed: f64,
}

impl Cast {
    fn out(&mut self, data: &str) {
        let v = serde_json::json!([(self.t * 1000.0).round() / 1000.0, "o", data]);
        self.lines.push(v.to_string());
    }
    fn wait(&mut self, secs: f64) {
        self.t += secs.max(0.0) / self.speed;
    }
}

fn crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}

fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        if line.is_empty() {
            out.push(String::new());
            continue;
        }
        for w in textwrap::wrap(line, width.max(20)) {
            out.push(w.into_owned());
        }
    }
    out
}

/// One line about a tool call, like the transcript shows it.
fn tool_line(tool: &str, input: &serde_json::Value) -> String {
    let arg = [
        "command",
        "path",
        "file_path",
        "pattern",
        "query",
        "url",
        "task",
        "goal",
    ]
    .iter()
    .find_map(|k| input.get(*k).and_then(|v| v.as_str()))
    .unwrap_or("");
    let arg: String = arg.lines().next().unwrap_or("").chars().take(70).collect();
    if arg.is_empty() {
        tool.to_string()
    } else {
        format!("{tool}({arg})")
    }
}

/// The recording for these events (as written to `<id>.events.jsonl`).
pub fn build_cast(events: &[EventEnvelope], opts: &Options) -> String {
    let mut c = Cast {
        t: 0.0,
        lines: Vec::new(),
        speed: opts.speed.clamp(0.25, 20.0),
    };
    let width = opts.width.saturating_sub(4).max(20);
    let mut last_ts: Option<chrono::DateTime<chrono::Utc>> = None;
    for env in events {
        // Real time between moments, shortened.
        if let (Some(prev), Some(ts)) = (last_ts, env.ts) {
            let gap = (ts - prev).num_milliseconds() as f64 / 1000.0;
            c.wait(gap.clamp(0.0, MAX_GAP));
        }
        if env.ts.is_some() {
            last_ts = env.ts;
        }
        match &env.event {
            AgentEvent::SessionStarted { model, .. } => {
                c.out(&format!(
                    "\x1b[38;5;173m✻ dcode-ai\x1b[0m \x1b[2m· {model}\x1b[0m\r\n"
                ));
            }
            AgentEvent::MessageReceived { role, content } if role == "user" => {
                c.out("\r\n\x1b[1m> \x1b[0m");
                let text: String = content.trim().chars().take(600).collect();
                let n = text.chars().count().max(1) as f64;
                let step = (TYPE_MAX / n).min(TYPE_STEP);
                for ch in text.chars() {
                    c.out(&crlf(&ch.to_string()));
                    c.wait(step);
                }
                c.out("\r\n");
                c.wait(0.3);
            }
            AgentEvent::MessageReceived { role, content } if role == "assistant" => {
                let lines = wrap(content.trim(), width);
                if lines.is_empty() {
                    continue;
                }
                c.out("\r\n");
                let step = (REPLY_MAX / lines.len() as f64).min(LINE_STEP);
                for (i, l) in lines.iter().enumerate() {
                    let lead = if i == 0 { "⏺ " } else { "  " };
                    c.out(&format!("{lead}{l}\r\n"));
                    c.wait(step);
                }
            }
            AgentEvent::ToolCallStarted { tool, input, .. } => {
                c.out(&format!(
                    "\r\n\x1b[32m⏺\x1b[0m \x1b[1m{}\x1b[0m\r\n",
                    tool_line(tool, input)
                ));
            }
            AgentEvent::ToolCallCompleted { output, .. } => {
                let first = if output.success {
                    output
                        .output
                        .lines()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("done")
                } else {
                    output.error.as_deref().unwrap_or("failed")
                };
                let first: String = first.chars().take(width.saturating_sub(4)).collect();
                let color = if output.success { "2" } else { "31" };
                c.out(&format!("  \x1b[{color}m⎿  {first}\x1b[0m\r\n"));
            }
            AgentEvent::ApprovalRequested { tool, .. } => {
                c.out(&format!("  \x1b[33m? approve {tool}\x1b[0m\r\n"));
            }
            AgentEvent::Error { message } => {
                let m: String = message.chars().take(width).collect();
                c.out(&format!("\x1b[31m✗ {m}\x1b[0m\r\n"));
            }
            _ => continue,
        }
    }
    c.wait(1.0);
    c.out("\r\n");
    let header = serde_json::json!({
        "version": 2,
        "width": opts.width,
        "height": opts.height,
        "title": opts.title,
        "env": {"TERM": "xterm-256color"},
    });
    let mut out = header.to_string();
    for l in c.lines {
        out.push('\n');
        out.push_str(&l);
    }
    out.push('\n');
    out
}

/// Reads a session's events from `<sessions>/<id>.events.jsonl`.
pub fn read_events(sessions_dir: &Path, id: &str) -> std::io::Result<Vec<EventEnvelope>> {
    let raw = std::fs::read_to_string(sessions_dir.join(format!("{id}.events.jsonl")))?;
    Ok(raw
        .lines()
        .filter_map(|l| serde_json::from_str::<EventEnvelope>(l).ok())
        .collect())
}

/// The most recently written session in `sessions_dir`.
pub fn latest_session(sessions_dir: &Path) -> Option<String> {
    std::fs::read_dir(sessions_dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let id = name.strip_suffix(".events.jsonl")?.to_string();
            Some((e.metadata().ok()?.modified().ok()?, id))
        })
        .max()
        .map(|(_, id)| id)
}

/// Writes `<workspace>/.dcode-ai/replays/<id>.cast`; with `gif`, also a GIF
/// through `agg` when it's installed. Returns what was written, and a note.
pub fn write(
    workspace: &Path,
    sessions_dir: &Path,
    id: &str,
    opts: &Options,
    gif: bool,
) -> anyhow::Result<(PathBuf, Option<PathBuf>, Option<String>)> {
    let events = read_events(sessions_dir, id)
        .map_err(|e| anyhow::anyhow!("no event log for session {id}: {e}"))?;
    let cast = build_cast(&events, opts);
    let dir = workspace.join(".dcode-ai/replays");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{id}.cast"));
    std::fs::write(&path, cast)?;
    if !gif {
        return Ok((path, None, None));
    }
    let gif_path = dir.join(format!("{id}.gif"));
    match std::process::Command::new("agg")
        .arg(&path)
        .arg(&gif_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
    {
        Ok(s) if s.success() => Ok((path, Some(gif_path), None)),
        Ok(_) => Ok((path, None, Some("agg couldn't convert it".into()))),
        Err(_) => Ok((
            path,
            None,
            Some(
                "for a GIF install agg (`cargo install --git https://github.com/asciinema/agg`), then run /replay --gif again"
                    .into(),
            ),
        )),
    }
}

/// Plays a `.cast` in this terminal.
pub fn play(path: &Path, speed: f64) -> anyhow::Result<()> {
    use std::io::Write;
    let raw = std::fs::read_to_string(path)?;
    let mut stdout = std::io::stdout();
    let mut last = 0.0;
    for line in raw.lines().skip(1) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let t = v[0].as_f64().unwrap_or(last);
        let data = v[2].as_str().unwrap_or("");
        let wait = ((t - last) / speed.max(0.1)).clamp(0.0, 5.0);
        std::thread::sleep(Duration::from_secs_f64(wait));
        last = t;
        stdout.write_all(data.as_bytes())?;
        stdout.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use dcode_ai_common::tool::ToolResult;

    fn env(secs: i64, event: AgentEvent) -> EventEnvelope {
        EventEnvelope {
            schema_version: 1,
            id: 0,
            ts: Some(chrono::DateTime::from_timestamp(1_800_000_000 + secs, 0).unwrap()),
            event,
        }
    }

    #[test]
    fn a_session_becomes_a_cast_with_short_pauses() {
        let events = vec![
            env(
                0,
                AgentEvent::MessageReceived {
                    role: "user".into(),
                    content: "fix it".into(),
                },
            ),
            env(
                300,
                AgentEvent::ToolCallStarted {
                    call_id: "c".into(),
                    tool: "read_file".into(),
                    input: serde_json::json!({"path": "src/a.rs"}),
                },
            ),
            env(
                301,
                AgentEvent::ToolCallCompleted {
                    call_id: "c".into(),
                    output: ToolResult {
                        call_id: "c".into(),
                        success: true,
                        output: "fn a() {}".into(),
                        error: None,
                    },
                },
            ),
            env(
                302,
                AgentEvent::MessageReceived {
                    role: "assistant".into(),
                    content: "Fixed.\nDone".into(),
                },
            ),
        ];
        let cast = build_cast(&events, &Options::default());
        let mut lines = cast.lines();
        let header: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(header["version"], 2);
        let frames: Vec<serde_json::Value> =
            lines.map(|l| serde_json::from_str(l).unwrap()).collect();
        let text: String = frames.iter().map(|f| f[2].as_str().unwrap()).collect();
        assert!(text.contains("> ") && text.contains("fix it"));
        assert!(text.contains("read_file(src/a.rs)"));
        assert!(text.contains("⏺ Fixed.\r\n  Done"));
        let end = frames.last().unwrap()[0].as_f64().unwrap();
        assert!(end < 10.0, "a 5-minute pause is shortened: {end}");
        // Times never go backwards.
        let ts: Vec<f64> = frames.iter().map(|f| f[0].as_f64().unwrap()).collect();
        assert!(ts.windows(2).all(|w| w[0] <= w[1]));
    }
}
