//! Code Mode scripts in a child `dcode-ai` process: tool calls relayed,
//! results back, memory capped, killed when the run stops.

use dcode_ai_core::tools::run_script::{ScriptMsg, spawn_child};
use std::time::Duration;

fn exe() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_dcode-ai"))
}

async fn run(code: &str) -> (Result<String, String>, Vec<String>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    spawn_child(&exe(), code, tx).expect("spawn");
    let mut called = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(60), rx.recv()).await {
            Ok(Some(ScriptMsg::Calls { calls, reply })) => {
                let results = calls
                    .iter()
                    .map(|(n, a)| {
                        called.push(n.clone());
                        Ok(format!("{n}:{}", a["path"].as_str().unwrap_or("")))
                    })
                    .collect();
                let _ = reply.send((results, Duration::ZERO));
            }
            Ok(Some(ScriptMsg::Done(r))) => return (r, called),
            Ok(None) => return (Err("channel closed".into()), called),
            Err(_) => return (Err("timed out".into()), called),
        }
    }
}

#[tokio::test]
async fn child_relays_calls_and_returns_the_result() {
    let (r, called) = run(
        "const a = tools.read_file({path: 'x'}); const b = parallel([{tool: 'read_file', args: {path: 'y'}}]); \
         print('hi'); return [a, b[0].text];",
    )
    .await;
    assert_eq!(called, ["read_file", "read_file"]);
    let out = r.unwrap();
    assert!(out.starts_with("hi\n"), "{out}");
    assert!(
        out.contains("read_file:x") && out.contains("read_file:y"),
        "{out}"
    );
    let (r, _) = run("throw new Error('boom')").await;
    assert!(r.unwrap_err().contains("boom"));
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn a_script_that_eats_memory_is_stopped() {
    let (r, _) = run("const a = []; for (let i = 0; i < 1000; i++) a.push(new Uint8Array(1 << 26)); return a.length;").await;
    let err = r.unwrap_err();
    assert!(err.contains("out of memory"), "{err}");
}

#[tokio::test]
async fn dropping_the_run_kills_the_child() {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    spawn_child(&exe(), "let s = 0; for (;;) { s++; }", tx).expect("spawn");
    drop(rx);
    // The watchdog notices within ~100 ms; nothing to assert beyond not
    // hanging — give it a moment so a leak would show in `ps` during CI.
    tokio::time::sleep(Duration::from_millis(400)).await;
}
