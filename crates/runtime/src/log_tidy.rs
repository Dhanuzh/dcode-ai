//! Session logs without the streaming pieces: dropping the live-only lines
//! (`AgentEvent::is_live_only`) that older versions wrote — most of a long
//! session's log, and never used by resume.

use dcode_ai_common::event::LIVE_ONLY_TYPES;
use std::path::Path;

/// Logs at least this big are tidied when their session is opened.
pub const TIDY_OVER_BYTES: u64 = 1024 * 1024;

fn live_only_line(line: &str) -> bool {
    let mut end = line.len().min(200);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    let head = &line[..end];
    LIVE_ONLY_TYPES
        .iter()
        .any(|t| head.contains(&format!("\"type\":\"{t}\"")))
}

/// Rewrites `path` without live-only lines, and with oversized tool results
/// capped (temp file, then rename).
/// Returns the size before and after; unchanged files aren't rewritten.
/// Only call it while nothing has the log open for writing.
pub fn tidy_log(path: &Path) -> std::io::Result<(u64, u64)> {
    let raw = std::fs::read_to_string(path)?;
    let before = raw.len() as u64;
    let mut kept = String::with_capacity(raw.len() / 3);
    let mut dropped = false;
    for line in raw.lines() {
        if live_only_line(line) {
            dropped = true;
            continue;
        }
        // Tool results from before outputs were capped (one was 22 MB):
        // store them the way new logs do, and resume shows them.
        if line.len() > 64 * 1024
            && line.contains("\"ToolCallCompleted\"")
            && let Ok(mut env) = serde_json::from_str::<dcode_ai_common::event::EventEnvelope>(line)
            && let dcode_ai_common::event::AgentEvent::ToolCallCompleted { output, .. } =
                &mut env.event
        {
            output.output = dcode_ai_core::agent::cap_tool_output(&output.output);
            if let Ok(capped) = serde_json::to_string(&env)
                && capped.len() < line.len()
            {
                kept.push_str(&capped);
                kept.push('\n');
                dropped = true;
                continue;
            }
        }
        kept.push_str(line);
        kept.push('\n');
    }
    if !dropped {
        return Ok((before, before));
    }
    let tmp = path.with_extension("jsonl.tidy");
    std::fs::write(&tmp, &kept)?;
    std::fs::rename(&tmp, path)?;
    Ok((before, kept.len() as u64))
}

/// Whether a session is open in another dcode-ai right now (its IPC
/// endpoint answers): its log must not be rewritten under it.
pub fn session_is_open(session_id: &str) -> bool {
    let Some(endpoint) = crate::ipc::find_ipc_endpoint(session_id) else {
        return false;
    };
    #[cfg(unix)]
    {
        std::os::unix::net::UnixStream::connect(&endpoint).is_ok()
    }
    #[cfg(not(unix))]
    {
        // `<sid>.<port>.tcp`
        endpoint
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.rsplit('.').nth(1))
            .and_then(|p| p.parse::<u16>().ok())
            .is_some_and(|port| {
                std::net::TcpStream::connect_timeout(
                    &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
                    std::time::Duration::from_millis(300),
                )
                .is_ok()
            })
    }
}

/// Tidies a session's log when it's big and nobody else has it open.
pub fn tidy_on_open(sessions_dir: &Path, session_id: &str) -> Option<(u64, u64)> {
    let log = sessions_dir.join(format!("{session_id}.events.jsonl"));
    let size = std::fs::metadata(&log).ok()?.len();
    if size < TIDY_OVER_BYTES || session_is_open(session_id) {
        return None;
    }
    tidy_log(&log).ok().filter(|(a, b)| b < a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tidy_drops_streaming_pieces_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("s.events.jsonl");
        let keep = r#"{"id":1,"event":{"type":"MessageReceived","role":"user","content":"hi TokensStreamed"}}"#;
        let mut text = String::new();
        text.push_str(keep);
        text.push('\n');
        for i in 0..50 {
            text.push_str(&format!(
                "{{\"id\":{i},\"event\":{{\"type\":\"TokensStreamed\",\"delta\":\"x\"}}}}\n"
            ));
            text.push_str(&format!(
                "{{\"id\":{i},\"event\":{{\"type\":\"ThinkingDelta\",\"delta\":\"y\"}}}}\n"
            ));
        }
        text.push_str(
            r#"{"id":99,"event":{"type":"MessageReceived","role":"assistant","content":"done"}}"#,
        );
        text.push('\n');
        std::fs::write(&log, &text).unwrap();
        let (before, after) = tidy_log(&log).unwrap();
        assert!(after < before / 3, "{before} → {after}");
        let left = std::fs::read_to_string(&log).unwrap();
        assert_eq!(left.lines().count(), 2);
        assert!(
            left.contains("hi TokensStreamed"),
            "a message mentioning it stays"
        );
        // Nothing to drop: the file isn't touched.
        let (a, b) = tidy_log(&log).unwrap();
        assert_eq!(a, b);
        // A huge old tool result is capped.
        let huge = "x".repeat(400_000);
        std::fs::write(
            &log,
            format!(
                "{{\"id\":1,\"event\":{{\"type\":\"ToolCallCompleted\",\"call_id\":\"c\",\"output\":{{\"call_id\":\"c\",\"success\":true,\"output\":\"{huge}\",\"error\":null}}}}}}\n"
            ),
        )
        .unwrap();
        let (before, after) = tidy_log(&log).unwrap();
        assert!(after < before / 4, "{before} → {after}");
        let left = std::fs::read_to_string(&log).unwrap();
        assert!(left.contains("ToolCallCompleted"));
    }
}
