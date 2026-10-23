//! sudo password prompts for commands run from the TUI.
//!
//! Agent and `!` commands start without a controlling terminal and with
//! `SUDO_ASKPASS` pointing at a small script that runs `dcode-ai __askpass`.
//! sudo (1.9+) then asks that helper instead of writing a prompt onto the
//! TUI. The helper connects to this session over a private Unix socket (with
//! a random token), the TUI shows its masked password popup, and the answer
//! goes back to sudo on the helper's stdout. Nothing is logged or kept.

/// What the helper prints when the user cancelled.
#[cfg(unix)]
const CANCELLED: &str = "NO";

/// `dcode-ai __askpass <prompt>`: ask the running TUI and print the password.
/// Exits non-zero when cancelled or unreachable (sudo then gives up).
pub fn helper(prompt: Option<String>) -> i32 {
    #[cfg(unix)]
    {
        use std::io::{BufRead, BufReader, Write};
        let (Ok(sock), Ok(token)) = (
            std::env::var("DCODE_AI_ASKPASS_SOCK"),
            std::env::var("DCODE_AI_ASKPASS_TOKEN"),
        ) else {
            eprintln!("dcode-ai: no session to ask for the sudo password");
            return 1;
        };
        let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&sock) else {
            eprintln!("dcode-ai: the session that ran this command is gone");
            return 1;
        };
        let request = serde_json::json!({
            "token": token,
            "prompt": prompt.unwrap_or_default(),
        });
        if writeln!(stream, "{request}").is_err() {
            return 1;
        }
        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line).is_err() {
            return 1;
        }
        let code = match line.strip_prefix("OK ") {
            Some(password) => {
                let mut out = std::io::stdout();
                let _ = out.write_all(password.as_bytes());
                let _ = out.flush();
                0
            }
            None => 1,
        };
        crate::tui::sudo_prompt::wipe(&mut line);
        code
    }
    #[cfg(not(unix))]
    {
        let _ = prompt;
        1
    }
}

/// Start the askpass server for this TUI session and turn askpass on for
/// agent commands. Best effort: without it, sudo behaves as before.
#[cfg(unix)]
pub fn start(state: std::sync::Arc<std::sync::Mutex<crate::tui::state::TuiSessionState>>) {
    use std::os::unix::fs::PermissionsExt;
    let Some(home) = dcode_ai_common::config::dcode_ai_home_dir() else {
        return;
    };
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let run_dir = home.join("run");
    if std::fs::create_dir_all(&run_dir).is_err() {
        return;
    }
    let _ = std::fs::set_permissions(&run_dir, std::fs::Permissions::from_mode(0o700));
    let token = random_token();
    let sock = run_dir.join(format!("askpass-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&sock);
    let Ok(listener) = tokio::net::UnixListener::bind(&sock) else {
        return;
    };
    let _ = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600));
    // sudo runs SUDO_ASKPASS with the prompt as its only argument.
    let script = run_dir.join("askpass");
    let body = format!(
        "#!/bin/sh\nexec '{}' __askpass \"$@\"\n",
        exe.display().to_string().replace('\'', "'\\''")
    );
    if std::fs::write(&script, body).is_err()
        || std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).is_err()
    {
        return;
    }
    dcode_ai_common::shell::set_sudo_askpass_env(vec![
        ("SUDO_ASKPASS".into(), script.display().to_string()),
        ("DCODE_AI_ASKPASS_SOCK".into(), sock.display().to_string()),
        ("DCODE_AI_ASKPASS_TOKEN".into(), token.clone()),
    ]);
    tokio::spawn(serve(listener, token, state));
}

#[cfg(not(unix))]
pub fn start(_state: std::sync::Arc<std::sync::Mutex<crate::tui::state::TuiSessionState>>) {}

#[cfg(unix)]
async fn serve(
    listener: tokio::net::UnixListener,
    token: String,
    state: std::sync::Arc<std::sync::Mutex<crate::tui::state::TuiSessionState>>,
) {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    // sudo asks again right after a wrong password.
    let mut last_answer: Option<std::time::Instant> = None;
    while let Ok((stream, _)) = listener.accept().await {
        let (read, mut write) = stream.into_split();
        let mut line = String::new();
        let mut reader = BufReader::new(read.take(8 * 1024));
        if reader.read_line(&mut line).await.is_err() {
            continue;
        }
        let request: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
        if request["token"].as_str() != Some(token.as_str()) {
            let _ = write.write_all(format!("{CANCELLED}\n").as_bytes()).await;
            continue;
        }
        let prompt = request["prompt"].as_str().unwrap_or("").trim().to_string();
        let retry = last_answer.is_some_and(|t| t.elapsed() < std::time::Duration::from_secs(15));
        let (tx, rx) = tokio::sync::oneshot::channel();
        let opened = match state.lock() {
            Ok(mut g) if g.sudo_prompt.is_none() => {
                g.sudo_prompt = Some(Box::new(
                    crate::tui::sudo_prompt::SudoPrompt::new(
                        format!(
                            "A command needs sudo. {}",
                            if prompt.is_empty() {
                                "Password:"
                            } else {
                                prompt.as_str()
                            }
                        ),
                        retry.then(|| "Sorry, try again.".to_string()),
                    )
                    .action("run"),
                ));
                g.askpass_reply = Some(tx);
                true
            }
            _ => false,
        };
        let answer = if opened {
            match tokio::time::timeout(std::time::Duration::from_secs(300), rx).await {
                Ok(Ok(answer)) => answer,
                _ => {
                    // Timed out: close the popup.
                    if let Ok(mut g) = state.lock() {
                        g.sudo_prompt = None;
                        g.askpass_reply = None;
                    }
                    None
                }
            }
        } else {
            None
        };
        match answer {
            Some(secret) => {
                last_answer = Some(std::time::Instant::now());
                let mut reply = b"OK ".to_vec();
                reply.extend(secret.into_bytes());
                reply.push(b'\n');
                let _ = write.write_all(&reply).await;
                reply.fill(0);
            }
            None => {
                last_answer = None;
                let _ = write.write_all(format!("{CANCELLED}\n").as_bytes()).await;
            }
        }
    }
}

/// 32 hex characters from the OS random source.
#[cfg(unix)]
fn random_token() -> String {
    use std::io::Read;
    let mut buf = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut buf);
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}
