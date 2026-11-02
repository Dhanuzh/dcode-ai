//! Relay mode: join a session across networks without a VPN, through a tiny
//! relay you run on a machine both sides can reach
//! (`dcode-ai relay serve`). It pairs connections and copies bytes — the
//! invite's Noise handshake runs end to end through it, so the relay never
//! sees the key or anything readable.
//!
//! Line protocol before the bytes flow:
//! - host control link: `HOST <room> <secret>` → `OK`, then `CONN <n>` per
//!   guest waiting;
//! - host data link: `PAIR <room> <n> <secret>` → spliced to guest `n`;
//! - guest: `GUEST <room>` → `OK` once paired (or `NO <why>`).

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};

pub const DEFAULT_PORT: u16 = 8790;
const MAX_ROOMS: usize = 64;
const MAX_WAITING: usize = 16;
const PAIR_WITHIN: Duration = Duration::from_secs(30);

/// Reads one `\n`-ended line byte by byte (so nothing after it is taken).
pub async fn read_line(s: &mut TcpStream) -> std::io::Result<String> {
    let mut out = Vec::new();
    let mut b = [0u8; 1];
    loop {
        if s.read(&mut b).await? == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        if b[0] == b'\n' {
            break;
        }
        out.push(b[0]);
        if out.len() > 256 {
            return Err(std::io::Error::other("line too long"));
        }
    }
    Ok(String::from_utf8_lossy(&out).trim().to_string())
}

struct Room {
    secret: String,
    to_host: mpsc::Sender<String>,
    waiting: HashMap<u64, TcpStream>,
    next: u64,
}

type Rooms = Arc<Mutex<HashMap<String, Room>>>;

/// Runs the relay on `port` until the process ends.
pub async fn serve(port: u16) -> std::io::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port)).await?;
    eprintln!(
        "dcode-ai relay on port {} — hosts use /invite relay <this-host>:{}",
        listener.local_addr()?.port(),
        listener.local_addr()?.port()
    );
    serve_on(listener).await
}

pub async fn serve_on(listener: TcpListener) -> std::io::Result<()> {
    let rooms: Rooms = Arc::default();
    loop {
        let (stream, _) = listener.accept().await?;
        let rooms = rooms.clone();
        tokio::spawn(async move {
            let _ = handle(stream, rooms).await;
        });
    }
}

async fn handle(mut s: TcpStream, rooms: Rooms) -> std::io::Result<()> {
    let line = tokio::time::timeout(Duration::from_secs(10), read_line(&mut s))
        .await
        .map_err(|_| std::io::Error::other("no hello"))??;
    let words: Vec<&str> = line.split_whitespace().collect();
    match words.as_slice() {
        ["HOST", room, secret] => {
            let (tx, mut rx) = mpsc::channel::<String>(32);
            {
                let mut r = rooms.lock().await;
                if let Some(old) = r.get(*room)
                    && old.secret != *secret
                {
                    s.write_all(b"NO room taken\n").await?;
                    return Ok(());
                }
                if r.len() >= MAX_ROOMS && !r.contains_key(*room) {
                    s.write_all(b"NO relay full\n").await?;
                    return Ok(());
                }
                r.insert(
                    room.to_string(),
                    Room {
                        secret: secret.to_string(),
                        to_host: tx,
                        waiting: HashMap::new(),
                        next: 1,
                    },
                );
            }
            s.write_all(b"OK\n").await?;
            let room = room.to_string();
            let mut probe = [0u8; 1];
            loop {
                tokio::select! {
                    msg = rx.recv() => {
                        let Some(msg) = msg else { break };
                        if s.write_all(msg.as_bytes()).await.is_err() {
                            break;
                        }
                    }
                    // The host went away.
                    n = s.read(&mut probe) => {
                        if matches!(n, Ok(0) | Err(_)) {
                            break;
                        }
                    }
                }
            }
            rooms.lock().await.remove(&room);
            Ok(())
        }
        ["GUEST", room] => {
            let n = {
                let mut r = rooms.lock().await;
                let Some(room_entry) = r.get_mut(*room) else {
                    drop(r);
                    s.write_all(b"NO no such room (old line, or the host stopped)\n")
                        .await?;
                    return Ok(());
                };
                if room_entry.waiting.len() >= MAX_WAITING {
                    drop(r);
                    s.write_all(b"NO too many waiting\n").await?;
                    return Ok(());
                }
                let n = room_entry.next;
                room_entry.next += 1;
                let _ = room_entry.to_host.try_send(format!("CONN {n}\n"));
                room_entry.waiting.insert(n, s);
                n
            };
            // Not paired in time: drop it.
            tokio::time::sleep(PAIR_WITHIN).await;
            if let Some(room_entry) = rooms.lock().await.get_mut(*room) {
                room_entry.waiting.remove(&n);
            }
            Ok(())
        }
        ["PAIR", room, n, secret] => {
            let guest = {
                let mut r = rooms.lock().await;
                let Some(room_entry) = r.get_mut(*room).filter(|e| e.secret == *secret) else {
                    return Ok(());
                };
                n.parse::<u64>()
                    .ok()
                    .and_then(|n| room_entry.waiting.remove(&n))
            };
            let Some(mut guest) = guest else {
                return Ok(());
            };
            guest.write_all(b"OK\n").await?;
            let _ = tokio::io::copy_bidirectional(&mut s, &mut guest).await;
            Ok(())
        }
        _ => Ok(()),
    }
}

/// A guest connecting through a relay: `GUEST <room>`, then (once the host
/// pairs) a plain stream to the host.
pub async fn guest_connect(addr: &str, room: &str) -> Result<TcpStream, String> {
    let mut s = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(addr))
        .await
        .map_err(|_| format!("the relay {addr} didn't answer"))?
        .map_err(|e| format!("can't reach the relay {addr}: {e}"))?;
    s.write_all(format!("GUEST {room}\n").as_bytes())
        .await
        .map_err(|e| e.to_string())?;
    let answer = tokio::time::timeout(PAIR_WITHIN, read_line(&mut s))
        .await
        .map_err(|_| "the host didn't pick up through the relay".to_string())?
        .map_err(|e| e.to_string())?;
    if answer == "OK" {
        Ok(s)
    } else {
        Err(format!(
            "the relay said: {}",
            answer.trim_start_matches("NO ").trim()
        ))
    }
}
