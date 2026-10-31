//! `/watch`: a read-only live web page for the current session, to share
//! with a teammate (or watch from a phone). It serves one page and one
//! server-sent-events stream that replays the session's event log and then
//! follows it as it grows. There is no route that sends anything to the
//! session.
//!
//! - Bound to 127.0.0.1 by default; `lan` binds every interface so someone
//!   on the same network can open it. Nothing goes through an outside
//!   service.
//! - Every request needs the random per-watch token (`?t=`).

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

/// A running watch page; dropping it (or `stop`) ends it.
pub struct WatchHandle {
    pub url: String,
    pub lan: bool,
    pub port: u16,
    stop: tokio::sync::watch::Sender<bool>,
}

impl WatchHandle {
    pub fn stop(&self) {
        let _ = self.stop.send(true);
    }
}

impl Drop for WatchHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// What someone on another device needs before a LAN watch link opens, when
/// it is more than "allow it when Windows asks": WSL's mirrored networking
/// blocks incoming connections in the Hyper-V firewall, and WSL's NAT mode
/// hides the address behind Windows altogether.
pub fn lan_help(port: u16) -> Option<String> {
    let wsl = std::env::var_os("WSL_DISTRO_NAME").is_some();
    if !wsl {
        return None;
    }
    let nat = lan_address().is_some_and(|ip| ip.to_string().starts_with("172."));
    if nat {
        return Some(format!(
            "WSL is in NAT mode, so other devices can't reach it directly. Either set \
             `networkingMode=mirrored` under [wsl2] in %UserProfile%\\.wslconfig (then `wsl --shutdown`), \
             or forward the port from Windows (admin PowerShell): \
             netsh interface portproxy add v4tov4 listenport={port} connectport={port} connectaddress=$(wsl hostname -I)"
        ));
    }
    Some(format!(
        "WSL (mirrored networking) blocks incoming connections until the port is allowed. Once, in an admin PowerShell:\n\
         New-NetFirewallHyperVRule -Name dcode-ai-watch-{port} -DisplayName \"dcode-ai watch {port}\" -Direction Inbound -VMCreatorId \"{{40E0AC32-46A5-438A-A0B2-2B479E8F2E90}}\" -Protocol TCP -LocalPorts {port}\n\
         New-NetFirewallRule -DisplayName \"dcode-ai watch {port}\" -Direction Inbound -Protocol TCP -LocalPort {port} -Action Allow -Profile Private"
    ))
}

/// This machine's address on the local network (no packet is sent).
fn lan_address() -> Option<std::net::IpAddr> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    Some(socket.local_addr().ok()?.ip())
}

/// Start serving `log_path` (the session's `<id>.events.jsonl`).
pub async fn start(log_path: PathBuf, title: String, lan: bool) -> std::io::Result<WatchHandle> {
    start_on(log_path, title, lan, 0).await
}

/// [`start`] on a fixed `port` (0 = any free one). A fixed port lets a
/// firewall rule be made once.
pub async fn start_on(
    log_path: PathBuf,
    title: String,
    lan: bool,
    port: u16,
) -> std::io::Result<WatchHandle> {
    let host = if lan { "0.0.0.0" } else { "127.0.0.1" };
    let listener = TcpListener::bind((host, port)).await?;
    let port = listener.local_addr()?.port();
    let token = format!("{:032x}", rand::random::<u128>());
    let host = if lan {
        lan_address()
            .map(|ip| ip.to_string())
            .unwrap_or_else(|| "127.0.0.1".into())
    } else {
        "127.0.0.1".into()
    };
    let url = format!("http://{host}:{port}/?t={token}");
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let mut accept_stop = stop_rx.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = accept_stop.changed() => break,
                conn = listener.accept() => {
                    let Ok((stream, _)) = conn else { continue };
                    let log = log_path.clone();
                    let token = token.clone();
                    let title = title.clone();
                    let stop = stop_rx.clone();
                    tokio::spawn(async move {
                        let _ = serve(stream, &log, &token, &title, stop).await;
                    });
                }
            }
        }
    });
    Ok(WatchHandle {
        url,
        lan,
        port,
        stop: stop_tx,
    })
}

fn query_token(target: &str) -> Option<&str> {
    let query = target.split_once('?')?.1;
    query.split('&').find_map(|kv| kv.strip_prefix("t="))
}

async fn serve(
    stream: TcpStream,
    log_path: &std::path::Path,
    token: &str,
    title: &str,
    stop: tokio::sync::watch::Receiver<bool>,
) -> std::io::Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read);
    let mut request_line = String::new();
    reader.read_line(&mut request_line).await?;
    // Drain the headers.
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 || line == "\r\n" || line == "\n" {
            break;
        }
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("/");
    let path = target.split('?').next().unwrap_or("/");
    if method != "GET" {
        return respond(
            &mut write,
            "405 Method Not Allowed",
            "text/plain",
            "read-only",
        )
        .await;
    }
    if query_token(target) != Some(token) {
        return respond(
            &mut write,
            "403 Forbidden",
            "text/plain",
            "this watch link needs its token",
        )
        .await;
    }
    match path {
        "/" => {
            let page = PAGE
                .replace("__TITLE__", &html_escape(title))
                .replace("__TOKEN__", token);
            respond(&mut write, "200 OK", "text/html; charset=utf-8", &page).await
        }
        "/events" => stream_log(&mut write, log_path, stop).await,
        _ => respond(&mut write, "404 Not Found", "text/plain", "not found").await,
    }
}

async fn respond(
    write: &mut tokio::net::tcp::OwnedWriteHalf,
    status: &str,
    content_type: &str,
    body: &str,
) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nReferrer-Policy: no-referrer\r\nConnection: close\r\n\r\n",
        body.len()
    );
    write.write_all(head.as_bytes()).await?;
    write.write_all(body.as_bytes()).await?;
    write.flush().await
}

/// The log so far (without streaming deltas), then every new line as it is
/// written.
async fn stream_log(
    write: &mut tokio::net::tcp::OwnedWriteHalf,
    log_path: &std::path::Path,
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> std::io::Result<()> {
    write
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: keep-alive\r\n\r\n",
        )
        .await?;
    let mut offset = 0u64;
    let mut partial = String::new();
    let mut first = true;
    let mut idle = 0u32;
    loop {
        if *stop.borrow() {
            return Ok(());
        }
        let mut chunk = String::new();
        if let Ok(mut file) = tokio::fs::File::open(log_path).await {
            let len = file.metadata().await.map(|m| m.len()).unwrap_or(0);
            if len < offset {
                offset = 0; // the log was replaced
            }
            if len > offset {
                file.seek(std::io::SeekFrom::Start(offset)).await?;
                file.read_to_string(&mut chunk).await?;
                offset = len;
            }
        }
        if !chunk.is_empty() {
            partial.push_str(&chunk);
            let mut out = String::new();
            while let Some(i) = partial.find('\n') {
                let line: String = partial.drain(..=i).collect();
                let line = line.trim();
                // History arrives whole; streaming deltas only matter live.
                if line.is_empty() || (first && line.contains("\"TokensStreamed\"")) {
                    continue;
                }
                out.push_str("data: ");
                out.push_str(line);
                out.push_str("\n\n");
            }
            if !out.is_empty() {
                write.write_all(out.as_bytes()).await?;
                write.flush().await?;
            }
            idle = 0;
        } else {
            idle += 1;
            if idle.is_multiple_of(50) {
                write.write_all(b": ping\n\n").await?;
                write.flush().await?;
            }
        }
        first = false;
        tokio::select! {
            _ = stop.changed() => return Ok(()),
            _ = tokio::time::sleep(Duration::from_millis(300)) => {}
        }
    }
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

const PAGE: &str = include_str!("watch_page.html");

#[cfg(test)]
mod tests {
    use super::*;

    async fn get(url: &str) -> (String, String) {
        let rest = url.strip_prefix("http://").unwrap();
        let (host, path) = rest.split_once('/').unwrap();
        let mut s = TcpStream::connect(host).await.unwrap();
        s.write_all(format!("GET /{path} HTTP/1.1\r\nHost: x\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut buf = vec![0u8; 65536];
        let mut got = String::new();
        // Read what arrives within a short window (the event stream stays open).
        let deadline = tokio::time::Instant::now() + Duration::from_millis(900);
        while let Ok(Ok(n)) = tokio::time::timeout_at(deadline, s.read(&mut buf)).await {
            if n == 0 {
                break;
            }
            got.push_str(&String::from_utf8_lossy(&buf[..n]));
        }
        let (head, body) = got.split_once("\r\n\r\n").unwrap_or((&got, ""));
        (head.to_string(), body.to_string())
    }

    #[tokio::test]
    async fn serves_the_page_and_the_log_only_with_the_token() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("s.events.jsonl");
        std::fs::write(
            &log,
            "{\"id\":1,\"event\":{\"type\":\"MessageReceived\",\"role\":\"user\",\"content\":\"hi <b>\"}}\n\
             {\"id\":2,\"event\":{\"type\":\"TokensStreamed\",\"delta\":\"old\"}}\n",
        )
        .unwrap();
        let w = start(log.clone(), "demo <session>".into(), false)
            .await
            .unwrap();
        assert!(w.url.starts_with("http://127.0.0.1:"));

        let (head, body) = get(&w.url).await;
        assert!(head.contains("200 OK"), "{head}");
        assert!(body.contains("demo &lt;session&gt;"), "title is escaped");

        let no_token = w.url.split('?').next().unwrap().to_string();
        assert!(get(&no_token).await.0.contains("403"));

        let events = w.url.replace("/?t=", "/events?t=");
        // A line written after the stream opened arrives too.
        let log2 = log.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(400)).await;
            let mut f = std::fs::OpenOptions::new().append(true).open(log2).unwrap();
            use std::io::Write;
            writeln!(
                f,
                "{{\"id\":3,\"event\":{{\"type\":\"TokensStreamed\",\"delta\":\"live\"}}}}"
            )
            .unwrap();
        });
        let (head, body) = get(&events).await;
        assert!(head.contains("text/event-stream"), "{head}");
        assert!(body.contains("hi <b>"), "history: {body}");
        assert!(!body.contains("\"old\""), "old deltas are skipped: {body}");
        assert!(body.contains("\"live\""), "new lines stream: {body}");
        w.stop();
    }
}
