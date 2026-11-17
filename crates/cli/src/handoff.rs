//! Moving a session to another computer: one file (gzipped JSON) with the
//! session's state and event log, token-shaped secrets masked. Importing it
//! into a checkout of the same project there adds it to the project's
//! sessions (paths re-pointed at the new folder); `/resume` continues it.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

const FORMAT: &str = "dcode-ai-session";

fn sessions_dir(ws: &Path) -> PathBuf {
    ws.join(".dcode-ai/sessions")
}

/// Writes session `id` of workspace `ws` into `out`.
pub fn export(ws: &Path, id: &str, out: &Path) -> Result<usize, String> {
    let dir = sessions_dir(ws);
    let state = std::fs::read_to_string(dir.join(format!("{id}.json")))
        .map_err(|e| format!("session {id}: {e}"))?;
    let events =
        std::fs::read_to_string(dir.join(format!("{id}.events.jsonl"))).unwrap_or_default();
    let mask = dcode_ai_common::redact::redact_secrets;
    let bundle = json!({
        "format": FORMAT,
        "version": 1,
        "dcode_ai": env!("CARGO_PKG_VERSION"),
        "session_id": id,
        "workspace": ws.display().to_string(),
        "state": mask(&state),
        "events": mask(&events),
    });
    let file = std::fs::File::create(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut gz = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    gz.write_all(bundle.to_string().as_bytes())
        .and_then(|()| gz.finish().map(|_| ()))
        .map_err(|e| e.to_string())?;
    Ok(events.lines().count())
}

/// Adds the session in `bundle` to workspace `ws`; returns its id.
pub fn import(ws: &Path, bundle: &Path) -> Result<String, String> {
    let mut text = String::new();
    flate2::read::GzDecoder::new(
        std::fs::File::open(bundle).map_err(|e| format!("{}: {e}", bundle.display()))?,
    )
    .read_to_string(&mut text)
    .map_err(|_| "that isn't a dcode-ai session file".to_string())?;
    let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if v["format"] != FORMAT {
        return Err("that isn't a dcode-ai session file".into());
    }
    let id = v["session_id"]
        .as_str()
        .ok_or("no session id in the file")?;
    if id.is_empty() || id.contains(['/', '\\']) || id.contains("..") {
        return Err("bad session id in the file".into());
    }
    let old_ws = v["workspace"].as_str().unwrap_or("");
    let new_ws = ws.display().to_string();
    let repoint = |s: &str| {
        if old_ws.is_empty() || old_ws == new_ws {
            s.to_string()
        } else {
            // Paths inside JSON strings are escaped the same way on both
            // sides; replace both forms.
            let esc = |p: &str| serde_json::to_string(p).unwrap_or_default();
            let (o, n) = (esc(old_ws), esc(&new_ws));
            s.replace(o.trim_matches('"'), n.trim_matches('"'))
                .replace(old_ws, &new_ws)
        }
    };
    let mut state: Value = serde_json::from_str(&repoint(v["state"].as_str().unwrap_or("{}")))
        .map_err(|e| e.to_string())?;
    // It isn't running here.
    if let Some(meta) = state.get_mut("meta").and_then(Value::as_object_mut) {
        meta.insert("socket_path".into(), Value::Null);
    }
    let dir = sessions_dir(ws);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let state_path = dir.join(format!("{id}.json"));
    if state_path.exists() {
        return Err(format!("session {id} is already in this project"));
    }
    std::fs::write(
        &state_path,
        serde_json::to_string(&state).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(
        dir.join(format!("{id}.events.jsonl")),
        repoint(v["events"].as_str().unwrap_or("")),
    )
    .map_err(|e| e.to_string())?;
    Ok(id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_moves_to_another_folder() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let dir = sessions_dir(a.path());
        std::fs::create_dir_all(&dir).unwrap();
        let ws_a = a.path().display().to_string();
        std::fs::write(
            dir.join("session-1.json"),
            json!({"meta": {"id": "session-1", "workspace_root": ws_a, "socket_path": "/tmp/x.sock"},
                   "messages": [{"text": "key ghp_abcdefghijklmnopqrstuvwxyz0123456789"}]})
                .to_string(),
        )
        .unwrap();
        std::fs::write(
            dir.join("session-1.events.jsonl"),
            format!("{}\n", json!({"path": format!("{ws_a}/src/main.rs")})),
        )
        .unwrap();
        let file = a.path().join("s.dcode-session");
        assert_eq!(export(a.path(), "session-1", &file).unwrap(), 1);
        let id = import(b.path(), &file).unwrap();
        assert_eq!(id, "session-1");
        let state = std::fs::read_to_string(sessions_dir(b.path()).join("session-1.json")).unwrap();
        let v: Value = serde_json::from_str(&state).unwrap();
        assert_eq!(v["meta"]["workspace_root"], b.path().display().to_string());
        assert!(v["meta"]["socket_path"].is_null());
        assert!(!state.contains("ghp_abcdefghij"), "secrets masked: {state}");
        let ev =
            std::fs::read_to_string(sessions_dir(b.path()).join("session-1.events.jsonl")).unwrap();
        let ev: Value = serde_json::from_str(ev.trim()).unwrap();
        assert!(
            ev["path"]
                .as_str()
                .unwrap()
                .starts_with(&b.path().display().to_string()),
            "{ev}"
        );
        assert!(import(b.path(), &file).unwrap_err().contains("already"));
    }
}
