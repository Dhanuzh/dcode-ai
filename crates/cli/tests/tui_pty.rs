//! Drives the real `dcode-ai` terminal UI in a pseudo-terminal and checks
//! what is on screen: startup, typing, pastes (bracketed and as a key burst,
//! like the Windows console sends), mode cycling and the dashboard.
//! No request reaches a model: nothing is submitted.
#![cfg(unix)]

use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const COLS: u16 = 120;
const ROWS: u16 = 40;

struct Tui {
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    screen: Arc<Mutex<vt100::Parser>>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl Tui {
    fn start(home: &Path, workspace: &Path) -> Self {
        Self::start_with(home, workspace, &[])
    }

    fn start_with(home: &Path, workspace: &Path, args: &[&str]) -> Self {
        let pty = native_pty_system()
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open pty");
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_dcode-ai"));
        cmd.args(args);
        cmd.cwd(workspace);
        cmd.env("HOME", home);
        cmd.env("USERPROFILE", home);
        cmd.env("XDG_CONFIG_HOME", home.join(".config"));
        cmd.env("XDG_RUNTIME_DIR", runtime_dir(home));
        cmd.env("TERM", "xterm-256color");
        // Only ever sent to the local mock model in these tests.
        cmd.env("ANTHROPIC_AUTH_TOKEN", "test-token");
        let child = pty.slave.spawn_command(cmd).expect("spawn dcode-ai");
        drop(pty.slave);
        let mut reader = pty.master.try_clone_reader().expect("reader");
        let writer = Arc::new(Mutex::new(pty.master.take_writer().expect("writer")));
        let screen = Arc::new(Mutex::new(vt100::Parser::new(ROWS, COLS, 0)));
        let sink = screen.clone();
        let answer = writer.clone();
        std::thread::spawn(move || {
            let _keep_master = pty.master;
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                let chunk = &buf[..n];
                let mut parser = sink.lock().unwrap();
                parser.process(chunk);
                // Answer cursor-position queries like a real terminal.
                let queries = chunk.windows(4).filter(|w| w == b"\x1b[6n").count();
                if queries > 0 {
                    let (row, col) = parser.screen().cursor_position();
                    drop(parser);
                    let mut w = answer.lock().unwrap();
                    for _ in 0..queries {
                        let _ = write!(w, "\x1b[{};{}R", row + 1, col + 1);
                    }
                    let _ = w.flush();
                }
            }
        });
        Self {
            writer,
            screen,
            child,
        }
    }

    fn text(&self) -> String {
        self.screen.lock().unwrap().screen().contents()
    }

    fn send(&mut self, bytes: &[u8]) {
        let mut w = self.writer.lock().unwrap();
        w.write_all(bytes).unwrap();
        w.flush().unwrap();
    }

    /// Wait until the screen contains `needle`; panic with the screen if not.
    fn wait_for(&self, needle: &str) {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(20) {
            if self.text().contains(needle) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("screen never showed {needle:?}:\n{}", self.text());
    }
}

impl Drop for Tui {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// A short folder for session sockets: macOS limits socket paths to ~104
/// bytes and its temp folders are long, so sockets there would fail.
fn runtime_dir(home: &Path) -> std::path::PathBuf {
    let tag: String = home
        .to_string_lossy()
        .bytes()
        .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32))
        .to_string();
    let dir = std::path::PathBuf::from(format!("/tmp/dcai-{tag}"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn setup() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let ws = dir.path().join("work");
    std::fs::create_dir_all(home.join(".dcode-ai")).unwrap();
    std::fs::create_dir_all(home.join("run")).unwrap();
    std::fs::create_dir_all(ws.join(".dcode-ai")).unwrap();
    std::fs::write(
        home.join(".dcode-ai").join("config.toml"),
        "[provider]\ndefault = \"anthropic\"\n[provider.anthropic]\nbackend = \"api\"\napi_key = \"sk-ant-test-key\"\n[memory.context]\nauto_detect_context_window = false\nquery_provider_models_api = false\n",
    )
    .unwrap();
    (dir, home, ws)
}

impl Tui {
    /// Wait until `pred` holds for the screen text.
    fn wait_until(&self, what: &str, pred: impl Fn(&str) -> bool) {
        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(20) {
            if pred(&self.text()) {
                return;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("screen never showed {what}:\n{}", self.text());
    }

    /// The prompt line(s) between the two composer rules.
    fn composer(&self) -> String {
        let text = self.text();
        let lines: Vec<&str> = text.lines().collect();
        let rules: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.trim_start().starts_with("────"))
            .map(|(i, _)| i)
            .collect();
        match rules.as_slice() {
            [.., a, b] => lines[a + 1..*b].join("\n"),
            _ => String::new(),
        }
    }

    /// Nothing was sent to a model: no reply, no request error.
    fn nothing_submitted(&self) {
        std::thread::sleep(Duration::from_millis(800));
        let text = self.text().to_lowercase();
        for sign in [
            "thinking",
            "401",
            "authentication",
            "invalid x-api-key",
            "error",
        ] {
            assert!(
                !text.contains(sign),
                "looks submitted ({sign}):\n{}",
                self.text()
            );
        }
    }

    fn clear_prompt(&mut self) {
        // Ctrl+C on a non-empty prompt clears it.
        self.send(b"\x03");
        self.wait_until("an empty prompt", |_| true);
        std::thread::sleep(Duration::from_millis(300));
    }
}

#[test]
fn typing_pasting_modes_and_dashboard() {
    let (_dir, home, ws) = setup();
    let mut tui = Tui::start(&home, &ws);
    tui.wait_for("Welcome to dcode-ai");
    tui.wait_for("? for shortcuts");

    // Typing shows up in the prompt.
    tui.send(b"hello there");
    tui.wait_until("typed text in the prompt", |_| tui_has(&tui, "hello there"));
    tui.clear_prompt();

    // A bracketed paste with line breaks stays in the prompt.
    tui.send(b"\x1b[200~first line\nsecond line\x1b[201~");
    tui.wait_until("the pasted text", |t| {
        t.contains("second line") || t.contains("Pasted text")
    });
    tui.nothing_submitted();
    tui.clear_prompt();

    // A paste as a burst of keys (no bracketed paste, like the Windows
    // console): the Enter inside it is a new line, not "send".
    tui.send(b"alpha\rbeta");
    tui.wait_until("both burst lines", |_| {
        let c = tui.composer();
        (c.contains("alpha") && c.contains("beta")) || c.contains("Pasted text")
    });
    tui.nothing_submitted();
    tui.clear_prompt();

    // Shift+Tab cycles the permission mode.
    tui.send(b"\x1b[Z");
    tui.wait_for("accept edits");

    // Ctrl+\ opens the dashboard and closes it again.
    tui.send(b"\x1c");
    tui.wait_for("Alt+1-5");
    tui.send(b"\x1c");
    tui.wait_until("the inline view", |t| !t.contains("Alt+1-5"));

    // Ctrl+C twice quits.
    tui.send(b"\x03");
    std::thread::sleep(Duration::from_millis(200));
    tui.send(b"\x03");
    let start = Instant::now();
    loop {
        if let Ok(Some(_)) = tui.child.try_wait() {
            break;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "did not quit:\n{}",
            tui.text()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn tui_has(tui: &Tui, needle: &str) -> bool {
    tui.composer().contains(needle)
}

/// A stand-in for the Anthropic Messages API: answers every request with a
/// streamed `Reply to: <the last thing the user said>`.
fn spawn_mock_model() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            std::thread::spawn(move || answer(stream));
        }
    });
    format!("http://{addr}")
}

fn answer(mut stream: std::net::TcpStream) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    // Headers, then the body by Content-Length.
    let (head_end, len) = loop {
        let Ok(n) = stream.read(&mut chunk) else {
            return;
        };
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..i]).to_lowercase();
            let len = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:"))
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            break (i + 4, len);
        }
    };
    while buf.len() < head_end + len {
        let Ok(n) = stream.read(&mut chunk) else {
            return;
        };
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    let body: serde_json::Value =
        serde_json::from_slice(&buf[head_end..]).unwrap_or(serde_json::Value::Null);
    let said = body["messages"]
        .as_array()
        .and_then(|m| m.iter().rev().find(|m| m["role"] == "user"))
        .map(|m| match &m["content"] {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Array(parts) => parts
                .iter()
                .filter_map(|p| p["text"].as_str())
                .collect::<Vec<_>>()
                .join(" "),
            _ => String::new(),
        })
        .unwrap_or_default();
    // After a tool result: confirm. Asked to create the note: call write_file.
    let last = body["messages"]
        .as_array()
        .and_then(|m| m.last())
        .cloned()
        .unwrap_or_default();
    let after_tool = last["content"]
        .as_array()
        .is_some_and(|parts| parts.iter().any(|p| p["type"] == "tool_result"));
    let sse = if after_tool {
        text_sse("Wrote the note.")
    } else if said.contains("create the note") {
        tool_sse(
            "write_file",
            &serde_json::json!({"path": "note.txt", "content": "hello from the test"}),
        )
    } else {
        let said: String = said
            .split_whitespace()
            .take(3)
            .collect::<Vec<_>>()
            .join(" ");
        text_sse(&format!("Reply to: {said}"))
    };
    let _ = write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
    );
    // "slow" prompts stream their reply over ~4 seconds (for Esc / steering).
    if said.contains("slow") {
        for p in sse.split("\n\n").filter(|p| !p.is_empty()) {
            let _ = write!(stream, "{p}\n\n");
            let _ = stream.flush();
            if p.contains("content_block_start") {
                for i in 0..8 {
                    std::thread::sleep(Duration::from_millis(500));
                    let delta = serde_json::to_string(&format!(" part{i}")).unwrap();
                    let _ = write!(
                        stream,
                        "event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":{delta}}}}}\n\n"
                    );
                    let _ = stream.flush();
                }
            }
        }
    } else {
        let _ = write!(stream, "{sse}");
    }
}

fn tool_sse(name: &str, input: &serde_json::Value) -> String {
    let partial = serde_json::to_string(&input.to_string()).unwrap();
    format!(
        "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"usage\":{{\"input_tokens\":100}}}}}}\n\n\
event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"{name}\",\"input\":{{}}}}}}\n\n\
event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"input_json_delta\",\"partial_json\":{partial}}}}}\n\n\
event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n\
event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"tool_use\"}},\"usage\":{{\"output_tokens\":5}}}}\n\n\
event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"
    )
}

fn text_sse(reply: &str) -> String {
    let text = serde_json::to_string(reply).unwrap();
    format!(
        "event: message_start\ndata: {{\"type\":\"message_start\",\"message\":{{\"usage\":{{\"input_tokens\":100}}}}}}\n\n\
event: content_block_start\ndata: {{\"type\":\"content_block_start\",\"index\":0,\"content_block\":{{\"type\":\"text\",\"text\":\"\"}}}}\n\n\
event: content_block_delta\ndata: {{\"type\":\"content_block_delta\",\"index\":0,\"delta\":{{\"type\":\"text_delta\",\"text\":{text}}}}}\n\n\
event: content_block_stop\ndata: {{\"type\":\"content_block_stop\",\"index\":0}}\n\n\
event: message_delta\ndata: {{\"type\":\"message_delta\",\"delta\":{{\"stop_reason\":\"end_turn\"}},\"usage\":{{\"output_tokens\":5}}}}\n\n\
event: message_stop\ndata: {{\"type\":\"message_stop\"}}\n\n"
    )
}

fn setup_with_model() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let (dir, home, ws) = setup();
    let url = spawn_mock_model();
    std::fs::write(
        home.join(".dcode-ai").join("config.toml"),
        format!(
            "[provider]\ndefault = \"anthropic\"\n[provider.anthropic]\nbackend = \"api\"\napi_key = \"sk-ant-test-key\"\nbase_url = \"{url}\"\n[memory.context]\nauto_detect_context_window = false\nquery_provider_models_api = false\n"
        ),
    )
    .unwrap();
    (dir, home, ws)
}

impl Tui {
    /// Type a prompt, send it, and wait for the model's reply.
    fn ask(&mut self, prompt: &str) {
        self.send(prompt.as_bytes());
        std::thread::sleep(Duration::from_millis(150));
        self.send(b"\r");
        let first: String = prompt
            .split_whitespace()
            .take(3)
            .collect::<Vec<_>>()
            .join(" ");
        self.wait_for(&format!("Reply to: {first}"));
        self.wait_for("? for shortcuts");
    }

    fn quit(&mut self) {
        self.send(b"\x03");
        std::thread::sleep(Duration::from_millis(200));
        self.send(b"\x03");
        let start = Instant::now();
        while !matches!(self.child.try_wait(), Ok(Some(_))) {
            assert!(
                start.elapsed() < Duration::from_secs(15),
                "did not quit:\n{}",
                self.text()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Session files in the workspace: (ids with saved state, ids with only a log).
fn session_files(ws: &Path) -> (Vec<String>, Vec<String>) {
    let dir = ws.join(".dcode-ai").join("sessions");
    let mut states = Vec::new();
    let mut logs = Vec::new();
    for e in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if let Some(id) = name.strip_suffix(".events.jsonl") {
            logs.push(id.to_string());
        } else if let Some(id) = name.strip_suffix(".json") {
            states.push(id.to_string());
        }
    }
    states.sort();
    let orphans = logs.into_iter().filter(|l| !states.contains(l)).collect();
    (states, orphans)
}

fn log_of(ws: &Path, id: &str) -> String {
    std::fs::read_to_string(
        ws.join(".dcode-ai")
            .join("sessions")
            .join(format!("{id}.events.jsonl")),
    )
    .unwrap_or_default()
}

/// The whole life of a session: talk, quit, continue (`-c`) twice, then
/// switch sessions in-process with `/resume <id>`. Every resume must show
/// everything said so far, each session's events must stay in its own log,
/// and no log may exist without its saved state.
#[test]
fn history_survives_quit_continue_and_resume() {
    let (_dir, home, ws) = setup_with_model();

    // Run 1: a new session.
    let mut tui = Tui::start(&home, &ws);
    tui.wait_for("? for shortcuts");
    tui.ask("alpha first question");
    tui.quit();
    let (states, orphans) = session_files(&ws);
    assert_eq!(states.len(), 1, "one saved session: {states:?}");
    assert!(orphans.is_empty(), "logs without state: {orphans:?}");
    let first = states[0].clone();

    // Run 2: continue it.
    let mut tui = Tui::start_with(&home, &ws, &["-c"]);
    tui.wait_for("alpha first question");
    tui.wait_for("Reply to: alpha first question");
    tui.ask("beta second question");
    tui.quit();
    let (states, orphans) = session_files(&ws);
    assert_eq!(states, vec![first.clone()], "continuing adds no session");
    assert!(orphans.is_empty(), "logs without state: {orphans:?}");

    // Run 3: continue again — both earlier turns are there.
    let mut tui = Tui::start_with(&home, &ws, &["-c"]);
    tui.wait_for("alpha first question");
    tui.wait_for("beta second question");
    tui.wait_for("Reply to: beta second question");

    // In the same process: start a new session, then go back with /resume.
    tui.send(b"/new");
    std::thread::sleep(Duration::from_millis(200));
    tui.send(b"\r");
    std::thread::sleep(Duration::from_millis(800));
    tui.ask("gamma other topic");
    tui.send(format!("/resume {first}").as_bytes());
    std::thread::sleep(Duration::from_millis(200));
    tui.send(b"\r");
    std::thread::sleep(Duration::from_millis(1500));
    tui.ask("delta back again");
    tui.quit();

    // Each session's words are in its own log, and only there.
    let log = log_of(&ws, &first);
    for said in [
        "alpha first question",
        "beta second question",
        "delta back again",
    ] {
        assert!(
            log.contains(said),
            "{said:?} missing from the first session's log"
        );
    }
    assert!(
        !log.contains("gamma other topic"),
        "another session's turn leaked in"
    );
    let (states, orphans) = session_files(&ws);
    assert_eq!(
        states.len(),
        2,
        "the first session and the /new one: {states:?}"
    );
    assert!(orphans.is_empty(), "logs without state: {orphans:?}");
    let second = states.iter().find(|s| **s != first).unwrap();
    assert!(log_of(&ws, second).contains("gamma other topic"));
    assert!(!log_of(&ws, second).contains("delta back again"));

    // Run 4: resume by id — the whole history shows.
    let tui = Tui::start_with(&home, &ws, &["-r", &first]);
    for said in [
        "alpha first question",
        "beta second question",
        "Reply to: delta back again",
    ] {
        tui.wait_for(said);
    }
}

/// A tool call that needs approval: the prompt appears, `1` approves, the
/// file is written, and resuming shows the call and the reply.
#[test]
fn approved_edit_runs_and_shows_after_resume() {
    let (_dir, home, ws) = setup_with_model();
    let mut tui = Tui::start(&home, &ws);
    tui.wait_for("? for shortcuts");
    tui.send(b"please create the note");
    std::thread::sleep(Duration::from_millis(150));
    tui.send(b"\r");
    tui.wait_until("an approval prompt", |t| {
        t.contains("note.txt") && t.contains("1.")
    });
    tui.send(b"1");
    tui.wait_for("Wrote the note.");
    assert_eq!(
        std::fs::read_to_string(ws.join("note.txt")).unwrap_or_default(),
        "hello from the test"
    );
    tui.wait_for("? for shortcuts");
    tui.quit();

    let tui = Tui::start_with(&home, &ws, &["-c"]);
    tui.wait_for("please create the note");
    tui.wait_for("note.txt");
    tui.wait_for("Wrote the note.");
}

/// Exploration, not a check: runs every slash command in one session and
/// writes each screen to `target/e2e-screens.txt` for review. Run with
/// `cargo test -p dcode-ai-cli --test tui_pty explore -- --ignored`.
#[test]
#[ignore]
fn explore_every_slash_command() {
    let (_dir, home, ws) = setup_with_model();
    std::fs::write(ws.join("README.md"), "# demo\nA small demo project.\n").unwrap();
    std::fs::write(
        ws.join("main.rs"),
        "fn main() {\n    println!(\"hi\");\n}\n",
    )
    .unwrap();
    let _ = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&ws)
        .status();
    let mut tui = Tui::start(&home, &ws);
    tui.wait_for("? for shortcuts");
    tui.ask("alpha warm up");
    let skip = [
        "/exit",
        "/quit",
        "/q",
        "/delete",
        "/update",
        "/share",
        "/pr-comments",
        "/login",
        "/logout",
        "/setup",
        "/editor",
        "/set-editor",
        "/ide",
        "/web",
        "/feedback",
        "/vim",
        "/mention",
    ];
    // `DCODE_AI_EXPLORE_FROM=/cmd` starts the list there.
    let from = std::env::var("DCODE_AI_EXPLORE_FROM").unwrap_or_default();
    let cmds: Vec<&str> = dcode_ai_cli::slash_commands::SLASH_COMMANDS
        .iter()
        .copied()
        .filter(|c| !skip.contains(c))
        .skip_while(|c| !from.is_empty() && **c != from)
        .collect();
    let mut out = String::new();
    for cmd in cmds {
        tui.send(cmd.as_bytes());
        std::thread::sleep(Duration::from_millis(150));
        tui.send(b"\r");
        std::thread::sleep(Duration::from_millis(1800));
        let alive = !matches!(tui.child.try_wait(), Ok(Some(_)));
        out.push_str(&format!(
            "\n\n==================== {cmd} (alive: {alive})\n{}",
            tui.text()
        ));
        if !alive {
            break;
        }
        // Close whatever opened (one Esc: two on an empty prompt rewind).
        tui.send(b"\x1b");
        std::thread::sleep(Duration::from_millis(500));
        if tui.text().contains("Restore the code and/or conversation") {
            tui.send(b"\x1b");
            std::thread::sleep(Duration::from_millis(400));
        }
    }
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/e2e-screens.txt");
    std::fs::write(&path, out).unwrap();
    println!("screens written to {}", path.display());
}

/// Exploration of mid-turn behaviour; writes `target/e2e-turns.txt`.
#[test]
#[ignore]
fn explore_turn_controls() {
    let (_dir, home, ws) = setup_with_model();
    std::fs::write(ws.join("main.rs"), "fn main() {}\n").unwrap();
    let mut tui = Tui::start(&home, &ws);
    tui.wait_for("? for shortcuts");
    let mut out = String::new();
    let mut snap = |tui: &Tui, label: &str| {
        out.push_str(&format!("\n\n==================== {label}\n{}", tui.text()));
    };
    // Esc interrupts a streaming reply.
    tui.send(b"tell a slow story");
    std::thread::sleep(Duration::from_millis(150));
    tui.send(b"\r");
    std::thread::sleep(Duration::from_millis(1500));
    snap(&tui, "streaming");
    tui.send(b"\x1b");
    std::thread::sleep(Duration::from_millis(1500));
    snap(&tui, "after Esc");
    // Enter while working = steer; Tab = queue.
    tui.send(b"another slow story");
    std::thread::sleep(Duration::from_millis(150));
    tui.send(b"\r");
    std::thread::sleep(Duration::from_millis(1000));
    tui.send(b"steer now please");
    std::thread::sleep(Duration::from_millis(150));
    tui.send(b"\r");
    std::thread::sleep(Duration::from_millis(300));
    tui.send(b"queued later please");
    std::thread::sleep(Duration::from_millis(150));
    tui.send(b"\t");
    std::thread::sleep(Duration::from_millis(600));
    snap(&tui, "steer + queue while working");
    std::thread::sleep(Duration::from_millis(7000));
    snap(&tui, "after both");
    // Shell mode and a file mention.
    tui.send(b"!echo shell-ok");
    std::thread::sleep(Duration::from_millis(150));
    tui.send(b"\r");
    std::thread::sleep(Duration::from_millis(1500));
    snap(&tui, "! shell");
    tui.send(b"explain @main");
    std::thread::sleep(Duration::from_millis(800));
    snap(&tui, "@ picker");
    tui.send(b"\t");
    std::thread::sleep(Duration::from_millis(300));
    tui.send(b"\r");
    std::thread::sleep(Duration::from_millis(2000));
    snap(&tui, "@ sent");
    // Auto mode, then the dashboard with Alt+1/2/3.
    for _ in 0..3 {
        tui.send(b"\x1b[Z");
        std::thread::sleep(Duration::from_millis(300));
    }
    snap(&tui, "shift+tab x3");
    tui.send(b"\x1c");
    std::thread::sleep(Duration::from_millis(800));
    for k in [b"\x1b1", b"\x1b2", b"\x1b3"] {
        tui.send(k);
        std::thread::sleep(Duration::from_millis(600));
        snap(&tui, &format!("dashboard alt+{}", k[1] as char));
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/e2e-turns.txt");
    std::fs::write(&path, out).unwrap();
}

/// Everyday controls on the real UI: a file mention completes with Tab and
/// sends on the first Enter; Esc stops a streaming reply, keeps what was
/// shown and says it was interrupted; bypass needs to be asked for by name.
#[test]
fn mention_interrupt_and_bypass_controls() {
    let (_dir, home, ws) = setup_with_model();
    std::fs::write(ws.join("main.rs"), "fn main() {}\n").unwrap();
    let mut tui = Tui::start(&home, &ws);
    tui.wait_for("? for shortcuts");

    tui.send(b"explain @mai");
    std::thread::sleep(Duration::from_millis(600));
    tui.send(b"\t");
    std::thread::sleep(Duration::from_millis(300));
    tui.send(b"\r");
    tui.wait_for("Reply to: explain");
    tui.wait_for("? for shortcuts");

    tui.send(b"a slow story please");
    std::thread::sleep(Duration::from_millis(150));
    tui.send(b"\r");
    tui.wait_for("part1");
    tui.send(b"\x1b");
    tui.wait_for("Interrupted");
    let text = tui.text();
    let partial = text.find("part0").expect("partial reply kept");
    assert!(partial < text.find("Interrupted").unwrap(), "{text}");
    tui.wait_for("? for shortcuts");

    tui.send(b"/permission-bypass");
    std::thread::sleep(Duration::from_millis(150));
    tui.send(b"\r");
    tui.wait_for("/permission-bypass on");
    assert!(
        !tui.text().contains("bypass permissions on"),
        "{}",
        tui.text()
    );
}

/// `-p` in a pipe prints only the answer; `--json` prints one JSON result.
#[test]
fn one_shot_output_for_scripts() {
    let (_dir, home, ws) = setup_with_model();
    let run = |args: &[&str]| {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_dcode-ai"))
            .args(args)
            .current_dir(&ws)
            .env("HOME", &home)
            .env("XDG_RUNTIME_DIR", runtime_dir(&home))
            .env("ANTHROPIC_AUTH_TOKEN", "test-token")
            .output()
            .expect("run dcode-ai");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    assert_eq!(
        run(&["-p", "hello piped world"]).trim(),
        "Reply to: hello piped world"
    );
    let json: serde_json::Value =
        serde_json::from_str(&run(&["-p", "json result please", "--json"])).expect("json");
    assert_eq!(json["output"], "Reply to: json result please");
    assert_eq!(json["end_reason"], "completed");
}

/// Seeing and sharing: after the agent edits a file, `/files` lists it,
/// `/timeline` opens and closes, `/watch` serves a live page (and refuses
/// a request without its token), `/handoff` writes a session file with the
/// note, and a message from `dcode-ai join` reaches the session.
#[test]
fn see_and_share_a_session() {
    let (_dir, home, ws) = setup_with_model();
    let mut tui = Tui::start(&home, &ws);
    tui.wait_for("? for shortcuts");
    tui.send(b"please create the note");
    std::thread::sleep(Duration::from_millis(150));
    tui.send(b"\r");
    tui.wait_until("an approval prompt", |t| {
        t.contains("note.txt") && t.contains("1.")
    });
    tui.send(b"1");
    tui.wait_for("Wrote the note.");
    tui.wait_for("? for shortcuts");

    let run = |tui: &mut Tui, cmd: &str| {
        tui.send(cmd.as_bytes());
        std::thread::sleep(Duration::from_millis(200));
        tui.send(b"\r");
    };
    run(&mut tui, "/files");
    tui.wait_for("✎ note.txt");

    // The dashboard's files tab shows it too.
    tui.send(b"\x1c");
    tui.wait_for("Alt+1-5");
    tui.send(b"\x1b4");
    tui.wait_until("the files tab", |t| {
        t.contains("1 edited") && t.contains("note.txt")
    });
    tui.send(b"\x1c");
    tui.wait_until("the inline view", |t| !t.contains("Alt+1-5"));

    run(&mut tui, "/office");
    tui.wait_until("the office", |t| {
        t.contains("q close") && t.contains("dcode-ai")
    });
    tui.send(b"q");
    tui.wait_for("? for shortcuts");

    run(&mut tui, "/timeline");
    tui.wait_until("the timeline", |t| {
        t.contains("steps") && t.contains("write_file")
    });
    tui.send(b"q");
    tui.wait_for("? for shortcuts");

    run(&mut tui, "/watch");
    tui.wait_for("[watch] live, read-only:");
    let text = tui.text();
    let url = text
        .split_whitespace()
        .find(|w| w.starts_with("http://127.0.0.1:"))
        .expect("watch url")
        .to_string();
    let fetch = |url: &str| -> String {
        let rest = url.strip_prefix("http://").unwrap();
        let (host, path) = rest.split_once('/').unwrap();
        let mut s = std::net::TcpStream::connect(host).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        write!(s, "GET /{path} HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut out = String::new();
        let _ = s.read_to_string(&mut out);
        out
    };
    let page = fetch(&url);
    assert!(
        page.contains("200 OK") && page.contains("dcode-ai live"),
        "{page}"
    );
    let bare = url.split('?').next().unwrap();
    assert!(fetch(bare).contains("403"));

    run(&mut tui, "/handoff login works, logout next");
    tui.wait_for("[handoff] wrote");
    let handoff = std::fs::read_dir(&ws)
        .unwrap()
        .flatten()
        .find(|e| e.file_name().to_string_lossy().ends_with(".dcode-session"))
        .expect("handoff file");
    let bundle = std::fs::read_to_string(handoff.path()).unwrap();
    assert!(bundle.contains("login works, logout next") && bundle.contains("note.txt"));

    // Another terminal joins and sends a message.
    let id = handoff
        .file_name()
        .to_string_lossy()
        .trim_end_matches(".dcode-session")
        .to_string();
    let mut join = std::process::Command::new(env!("CARGO_BIN_EXE_dcode-ai"))
        .args(["join", &id])
        .current_dir(&ws)
        .env("HOME", &home)
        .env("XDG_RUNTIME_DIR", runtime_dir(&home))
        .env("ANTHROPIC_AUTH_TOKEN", "test-token")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("join");
    std::thread::sleep(Duration::from_millis(1500));
    if let Some(stdin) = join.stdin.as_mut() {
        writeln!(stdin, "hello from beside you").unwrap();
    }
    tui.wait_for("from a joined terminal: hello from beside you");
    tui.wait_for("Reply to: hello from beside");
    let _ = join.kill();
    let _ = join.wait();
}

impl Tui {
    /// The screen with colours, as JSON rows of `[symbol, fg, bg]`.
    fn cells_json(&self) -> serde_json::Value {
        let p = self.screen.lock().unwrap();
        let screen = p.screen();
        let col = |c: vt100::Color, default: [u8; 3]| match c {
            vt100::Color::Rgb(r, g, b) => serde_json::json!([r, g, b]),
            vt100::Color::Idx(i) => {
                let v = if i < 16 {
                    (i as u32 % 8) * 30 + 40
                } else {
                    120
                };
                serde_json::json!([v, v, v])
            }
            vt100::Color::Default => serde_json::json!(default),
        };
        let rows: Vec<Vec<serde_json::Value>> = (0..ROWS)
            .map(|y| {
                (0..COLS)
                    .map(|x| match screen.cell(y, x) {
                        Some(c) => serde_json::json!([
                            c.contents(),
                            col(c.fgcolor(), [230, 230, 230]),
                            col(c.bgcolor(), [24, 22, 20])
                        ]),
                        None => serde_json::json!([" ", [0, 0, 0], [24, 22, 20]]),
                    })
                    .collect()
            })
            .collect();
        serde_json::json!(rows)
    }
}

/// Screenshots of `/office` in the real terminal UI, for review:
/// `target/office-screens.json` → `cells_to_png.py`.
#[test]
#[ignore]
fn office_screenshots() {
    let (_dir, home, ws) = setup_with_model();
    let mut tui = Tui::start(&home, &ws);
    tui.wait_for("? for shortcuts");
    let mut shots = Vec::new();
    // While a reply is streaming: the dashboard's office tab.
    tui.send(b"a slow story please");
    std::thread::sleep(Duration::from_millis(150));
    tui.send(b"\r");
    tui.wait_for("part0");
    tui.send(b"\x1c");
    tui.wait_for("Alt+1-5");
    tui.send(b"\x1b5");
    std::thread::sleep(Duration::from_millis(900));
    shots.push(tui.cells_json());
    tui.send(b"\x1c");
    tui.wait_until("the inline view", |t| !t.contains("Alt+1-5"));
    tui.wait_for("? for shortcuts");
    // Waiting for an approval.
    tui.send(b"please create the note");
    std::thread::sleep(Duration::from_millis(150));
    tui.send(b"\r");
    tui.wait_until("an approval prompt", |t| {
        t.contains("note.txt") && t.contains("1.")
    });
    tui.send(b"\x1c");
    std::thread::sleep(Duration::from_millis(900));
    shots.push(tui.cells_json());
    tui.send(b"\x1c");
    std::thread::sleep(Duration::from_millis(600));
    tui.send(b"1");
    tui.wait_for("Wrote the note.");
    tui.wait_for("? for shortcuts");
    // `/office` full screen.
    tui.send(b"/office");
    std::thread::sleep(Duration::from_millis(150));
    tui.send(b"\r");
    tui.wait_for("q close");
    std::thread::sleep(Duration::from_millis(400));
    shots.push(tui.cells_json());
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/office-screens.json");
    std::fs::write(path, serde_json::to_string(&shots).unwrap()).unwrap();
}
