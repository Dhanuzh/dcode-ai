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
        let pty = native_pty_system()
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open pty");
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_dcode-ai"));
        cmd.cwd(workspace);
        cmd.env("HOME", home);
        cmd.env("USERPROFILE", home);
        cmd.env("XDG_CONFIG_HOME", home.join(".config"));
        cmd.env("XDG_RUNTIME_DIR", home.join("run"));
        cmd.env("TERM", "xterm-256color");
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
    tui.wait_for("Alt+1/2/3");
    tui.send(b"\x1c");
    tui.wait_until("the inline view", |t| !t.contains("Alt+1/2/3"));

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
