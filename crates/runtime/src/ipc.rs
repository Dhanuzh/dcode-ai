use dcode_ai_common::event::{AgentCommand, EventEnvelope};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc};

// ── Message framing ────────────────────────────────────────────────
//
// Wire format (v2): each message is a 4-byte big-endian length prefix followed
// by exactly that many bytes of JSON. This prevents desync on payloads with
// embedded newlines/binary and bounds per-message memory.
//
// Backward compatibility: readers auto-detect per connection. Frames are
// capped at 16 MiB, so a framed stream's first byte is always 0x00; legacy
// NDJSON always starts with '{'. Writers emit frames unless
// `DCODE_AI_IPC_LEGACY=1` is set (escape hatch for external NDJSON consumers).

/// Max frame payload. Also guarantees the first length byte is 0x00, which is
/// how readers distinguish framed streams from legacy NDJSON.
const MAX_FRAME_BYTES: u32 = 16 * 1024 * 1024;

fn legacy_writes() -> bool {
    static LEGACY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *LEGACY.get_or_init(|| {
        std::env::var("DCODE_AI_IPC_LEGACY")
            .is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"))
    })
}

async fn write_message(
    writer: &mut (impl AsyncWrite + Unpin),
    payload: &[u8],
) -> std::io::Result<()> {
    if legacy_writes() {
        writer.write_all(payload).await?;
        writer.write_all(b"\n").await?;
        return Ok(());
    }
    let len = u32::try_from(payload.len())
        .ok()
        .filter(|len| *len <= MAX_FRAME_BYTES)
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "IPC frame too large")
        })?;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(payload).await?;
    Ok(())
}

enum WireMode {
    Framed,
    LegacyLines,
}

/// Reads whole JSON messages from either wire format, decided by the first
/// byte of the stream (0x00 = framed, anything else = legacy NDJSON).
struct MessageReader<R> {
    inner: BufReader<R>,
    mode: Option<WireMode>,
}

impl<R: AsyncRead + Unpin> MessageReader<R> {
    fn new(stream: R) -> Self {
        Self {
            inner: BufReader::new(stream),
            mode: None,
        }
    }

    async fn next_message(&mut self) -> std::io::Result<Option<String>> {
        if self.mode.is_none() {
            let buf = self.inner.fill_buf().await?;
            if buf.is_empty() {
                return Ok(None); // clean EOF before any data
            }
            self.mode = Some(if buf[0] == 0 {
                WireMode::Framed
            } else {
                WireMode::LegacyLines
            });
        }
        match self.mode.as_ref().expect("mode set above") {
            WireMode::Framed => {
                let mut len_bytes = [0u8; 4];
                match self.inner.read_exact(&mut len_bytes).await {
                    Ok(_) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::UnexpectedEof => {
                        return Ok(None); // EOF between frames
                    }
                    Err(err) => return Err(err),
                }
                let len = u32::from_be_bytes(len_bytes);
                if len > MAX_FRAME_BYTES {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "IPC frame exceeds 16 MiB cap",
                    ));
                }
                let mut payload = vec![0u8; len as usize];
                self.inner.read_exact(&mut payload).await?;
                Ok(Some(String::from_utf8_lossy(&payload).into_owned()))
            }
            WireMode::LegacyLines => {
                let mut line = String::new();
                let n = self.inner.read_line(&mut line).await?;
                if n == 0 {
                    return Ok(None);
                }
                while line.ends_with('\n') || line.ends_with('\r') {
                    line.pop();
                }
                Ok(Some(line))
            }
        }
    }
}

/// IPC server that broadcasts AgentEvents and receives AgentCommands
/// over a platform endpoint.
///
/// Unix uses Unix domain sockets. Windows uses loopback TCP because Unix domain
/// sockets are not universally available or ergonomic there.
pub struct IpcServer {
    session_id: String,
}

impl IpcServer {
    pub fn new(session_id: &str) -> Self {
        Self {
            session_id: session_id.to_string(),
        }
    }

    /// Best-effort endpoint path: the live endpoint file when one exists,
    /// otherwise the legacy deterministic location. On Windows the real port
    /// is only known once the server binds (ephemeral), so prefer discovery.
    pub fn socket_path(&self) -> PathBuf {
        find_ipc_endpoint(&self.session_id).unwrap_or_else(|| ipc_endpoint_path(&self.session_id))
    }

    /// Start listening for client connections.
    pub async fn start(&self) -> Result<IpcHandle, IpcError> {
        let (event_tx, _) = broadcast::channel::<String>(256);
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        let socket_path =
            start_transport_accept_loop(&self.session_id, event_tx.clone(), command_tx).await?;

        Ok(IpcHandle {
            socket_path,
            event_tx,
            command_rx,
        })
    }
}

/// Find the live endpoint recorded for a session (Windows writes
/// `<sid>.<port>.tcp` marker files with an ephemeral port; Unix sockets sit
/// at a fixed path).
pub fn find_ipc_endpoint(session_id: &str) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let prefix = format!("{session_id}.");
        let dir = runtime_ipc_dir();
        let entries = std::fs::read_dir(&dir).ok()?;
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(&prefix) && name.ends_with(".tcp") {
                return Some(dir.join(name));
            }
        }
        None
    }
    #[cfg(not(windows))]
    {
        let path = ipc_endpoint_path(session_id);
        path.exists().then_some(path)
    }
}

pub struct IpcHandle {
    socket_path: PathBuf,
    event_tx: broadcast::Sender<String>,
    command_rx: mpsc::UnboundedReceiver<AgentCommand>,
}

impl IpcHandle {
    pub fn socket_path(&self) -> &PathBuf {
        &self.socket_path
    }

    pub async fn broadcast(&self, event: &EventEnvelope) -> Result<(), IpcError> {
        let line = serde_json::to_string(event)
            .map_err(|err| IpcError::ConnectionFailed(err.to_string()))?;
        let _ = self.event_tx.send(line);
        Ok(())
    }

    pub async fn recv_command(&mut self) -> Option<AgentCommand> {
        self.command_rx.recv().await
    }

    /// Split into parts for separate tasks: event broadcast and command receiver.
    pub fn into_parts(
        self,
    ) -> (
        broadcast::Sender<String>,
        mpsc::UnboundedReceiver<AgentCommand>,
    ) {
        (self.event_tx, self.command_rx)
    }
}

/// IPC client for connecting to a running session socket (events, approvals, shutdown).
pub struct IpcClient {
    socket_path: PathBuf,
}

impl IpcClient {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    pub async fn connect(&self) -> Result<mpsc::Receiver<EventEnvelope>, IpcError> {
        let (tx, rx) = mpsc::channel(128);
        spawn_event_reader(&self.socket_path, tx).await?;
        Ok(rx)
    }

    pub async fn send_command(&self, cmd: &AgentCommand) -> Result<(), IpcError> {
        send_command_to_endpoint(&self.socket_path, cmd).await
    }
}

#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),
}

async fn handle_connection(
    stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
    mut event_rx: broadcast::Receiver<String>,
    command_tx: mpsc::UnboundedSender<AgentCommand>,
) {
    let (reader, mut writer) = tokio::io::split(stream);
    let read_task = tokio::spawn(async move {
        let mut messages = MessageReader::new(reader);
        while let Ok(Some(message)) = messages.next_message().await {
            if let Ok(command) = serde_json::from_str::<AgentCommand>(&message) {
                let _ = command_tx.send(command);
            }
        }
    });

    let write_task = tokio::spawn(async move {
        // Heartbeat: an empty frame (bare newline in legacy mode) every 15s.
        // Readers skip empty messages; clients treat a long silence as a dead
        // runtime (see spawn_event_reader_for_stream).
        let mut heartbeat = tokio::time::interval(HEARTBEAT_INTERVAL);
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                received = event_rx.recv() => match received {
                    Ok(line) => {
                        if write_message(&mut writer, line.as_bytes()).await.is_err() {
                            break;
                        }
                    }
                    // Backpressure: a slow consumer lagged the broadcast ring.
                    // Tell it what it missed and keep the connection alive
                    // instead of silently dropping it (previous behavior).
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        let notice = serde_json::json!({
                            "schema_version": 1,
                            "id": 0,
                            "ts": null,
                            "event": {
                                "type": "Error",
                                "message": format!(
                                    "event stream lagged: {skipped} event(s) skipped (consumer too slow)"
                                ),
                            },
                        })
                        .to_string();
                        if write_message(&mut writer, notice.as_bytes()).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                },
                _ = heartbeat.tick() => {
                    if write_message(&mut writer, b"").await.is_err() {
                        break;
                    }
                }
            }
        }
    });

    let _ = tokio::join!(read_task, write_task);
}

/// Server → client keepalive cadence.
const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);
/// A client that hears nothing (events or heartbeats) for this long treats the
/// runtime as gone. 4× the heartbeat so a single delayed tick can't false-fire.
const STALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

pub fn runtime_ipc_dir() -> PathBuf {
    #[cfg(windows)]
    {
        std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("dcode-ai")
    }
    #[cfg(not(windows))]
    {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("dcode-ai")
    }
}

pub fn ipc_endpoint_path(session_id: &str) -> PathBuf {
    #[cfg(windows)]
    {
        runtime_ipc_dir().join(format!(
            "{session_id}.{}.tcp",
            deterministic_loopback_port(session_id)
        ))
    }
    #[cfg(not(windows))]
    {
        runtime_ipc_dir().join(format!("{session_id}.sock"))
    }
}

#[cfg(unix)]
async fn start_transport_accept_loop(
    session_id: &str,
    event_tx: broadcast::Sender<String>,
    command_tx: mpsc::UnboundedSender<AgentCommand>,
) -> Result<PathBuf, IpcError> {
    use tokio::net::UnixListener;

    let socket_path = &ipc_endpoint_path(session_id);
    if let Some(parent) = socket_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|err| IpcError::ConnectionFailed(err.to_string()))?;
    }
    if socket_path.exists() {
        let _ = tokio::fs::remove_file(socket_path).await;
    }

    let listener = UnixListener::bind(socket_path)
        .map_err(|err| IpcError::ConnectionFailed(err.to_string()))?;
    let socket_path = socket_path.to_path_buf();
    let cleanup_path = socket_path.clone();

    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(handle_connection(
                stream,
                event_tx.subscribe(),
                command_tx.clone(),
            ));
        }
        let _ = tokio::fs::remove_file(cleanup_path).await;
    });
    Ok(socket_path)
}

#[cfg(windows)]
async fn start_transport_accept_loop(
    session_id: &str,
    event_tx: broadcast::Sender<String>,
    command_tx: mpsc::UnboundedSender<AgentCommand>,
) -> Result<PathBuf, IpcError> {
    use tokio::net::TcpListener;

    let dir = runtime_ipc_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|err| IpcError::ConnectionFailed(err.to_string()))?;

    // Ephemeral port: a fixed hash-derived port collides when a session is
    // resumed while the old process is alive (WSAEADDRINUSE 10048). The real
    // port is recorded in the `<sid>.<port>.tcp` marker consumed by clients.
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|err| IpcError::ConnectionFailed(err.to_string()))?;
    let port = listener
        .local_addr()
        .map_err(|err| IpcError::ConnectionFailed(err.to_string()))?
        .port();

    // Drop stale markers from previous runs of this session id.
    let prefix = format!("{session_id}.");
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with(&prefix) && name.ends_with(".tcp") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
    let socket_path = dir.join(format!("{session_id}.{port}.tcp"));
    tokio::fs::write(&socket_path, b"")
        .await
        .map_err(|err| IpcError::ConnectionFailed(err.to_string()))?;

    let marker = socket_path.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            tokio::spawn(handle_connection(
                stream,
                event_tx.subscribe(),
                command_tx.clone(),
            ));
        }
        let _ = std::fs::remove_file(marker);
    });
    Ok(socket_path)
}

#[cfg(unix)]
async fn spawn_event_reader(
    socket_path: &std::path::Path,
    tx: mpsc::Sender<EventEnvelope>,
) -> Result<(), IpcError> {
    let stream = tokio::net::UnixStream::connect(socket_path)
        .await
        .map_err(|err| IpcError::ConnectionFailed(err.to_string()))?;
    spawn_event_reader_for_stream(stream, tx);
    Ok(())
}

#[cfg(windows)]
async fn spawn_event_reader(
    socket_path: &std::path::Path,
    tx: mpsc::Sender<EventEnvelope>,
) -> Result<(), IpcError> {
    let stream = tokio::net::TcpStream::connect(tcp_addr_from_endpoint(socket_path)?)
        .await
        .map_err(|err| IpcError::ConnectionFailed(err.to_string()))?;
    spawn_event_reader_for_stream(stream, tx);
    Ok(())
}

/// Synthetic envelope injected by the client when the runtime goes away, so
/// every consumer (attach, web SSE) renders an explicit alert instead of a
/// silently frozen stream.
fn disconnect_notice(reason: &str) -> EventEnvelope {
    EventEnvelope::new(
        0,
        dcode_ai_common::event::AgentEvent::Error {
            message: format!("runtime disconnected: {reason}"),
        },
    )
}

fn spawn_event_reader_for_stream(
    stream: impl AsyncRead + Unpin + Send + 'static,
    tx: mpsc::Sender<EventEnvelope>,
) {
    tokio::spawn(async move {
        let mut messages = MessageReader::new(stream);
        let mut session_ended = false;
        loop {
            let next = tokio::time::timeout(STALL_TIMEOUT, messages.next_message()).await;
            match next {
                // Heartbeats arrive every 15s, so a 60s silence means the
                // runtime is hung or the transport is dead.
                Err(_elapsed) => {
                    let _ = tx
                        .send(disconnect_notice("no events or heartbeats for 60s"))
                        .await;
                    break;
                }
                Ok(Ok(Some(message))) => {
                    if message.is_empty() {
                        continue; // heartbeat
                    }
                    if let Ok(event) = serde_json::from_str::<EventEnvelope>(&message) {
                        if matches!(
                            event.event,
                            dcode_ai_common::event::AgentEvent::SessionEnded { .. }
                        ) {
                            session_ended = true;
                        }
                        if tx.send(event).await.is_err() {
                            break;
                        }
                    }
                }
                // EOF without a SessionEnded first = the runtime died rather
                // than finished; say so. A clean end stays quiet.
                Ok(Ok(None)) => {
                    if !session_ended {
                        let _ = tx
                            .send(disconnect_notice("connection closed unexpectedly"))
                            .await;
                    }
                    break;
                }
                Ok(Err(err)) => {
                    let _ = tx.send(disconnect_notice(&err.to_string())).await;
                    break;
                }
            }
        }
    });
}

#[cfg(unix)]
async fn send_command_to_endpoint(
    socket_path: &std::path::Path,
    cmd: &AgentCommand,
) -> Result<(), IpcError> {
    let stream = tokio::net::UnixStream::connect(socket_path)
        .await
        .map_err(|err| IpcError::ConnectionFailed(err.to_string()))?;
    write_command(stream, cmd).await
}

#[cfg(windows)]
async fn send_command_to_endpoint(
    socket_path: &std::path::Path,
    cmd: &AgentCommand,
) -> Result<(), IpcError> {
    let stream = tokio::net::TcpStream::connect(tcp_addr_from_endpoint(socket_path)?)
        .await
        .map_err(|err| IpcError::ConnectionFailed(err.to_string()))?;
    write_command(stream, cmd).await
}

async fn write_command(
    mut stream: impl AsyncWrite + Unpin,
    cmd: &AgentCommand,
) -> Result<(), IpcError> {
    let line =
        serde_json::to_string(cmd).map_err(|err| IpcError::ConnectionFailed(err.to_string()))?;
    write_message(&mut stream, line.as_bytes())
        .await
        .map_err(|err| IpcError::ConnectionFailed(err.to_string()))?;
    Ok(())
}

#[cfg(windows)]
fn tcp_addr_from_endpoint(path: &std::path::Path) -> Result<String, IpcError> {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| IpcError::ConnectionFailed("invalid TCP IPC endpoint".into()))?;
    let port = file_name
        .strip_suffix(".tcp")
        .and_then(|stem| stem.rsplit('.').next())
        .and_then(|port| port.parse::<u16>().ok())
        .ok_or_else(|| IpcError::ConnectionFailed("invalid TCP IPC endpoint port".into()))?;
    Ok(format!("127.0.0.1:{port}"))
}

#[cfg(windows)]
fn deterministic_loopback_port(session_id: &str) -> u16 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in session_id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    40_000 + (hash % 10_000) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn framed_messages_round_trip() {
        let (client, server) = tokio::io::duplex(4096);
        let (_, mut client_writer) = tokio::io::split(client);
        let (server_reader, _) = tokio::io::split(server);

        let payloads = [
            r#"{"type":"Cancel"}"#.to_string(),
            // Embedded newline inside a JSON string — the case NDJSON framing
            // could never carry raw.
            format!("{{\"note\":\"{}\"}}", "x".repeat(100)),
        ];
        for p in &payloads {
            write_message(&mut client_writer, p.as_bytes())
                .await
                .expect("write");
        }
        drop(client_writer);

        let mut reader = MessageReader::new(server_reader);
        for expected in &payloads {
            let got = reader.next_message().await.expect("read").expect("some");
            assert_eq!(&got, expected);
        }
        assert!(reader.next_message().await.expect("eof").is_none());
    }

    #[tokio::test]
    async fn legacy_ndjson_streams_still_parse() {
        let (client, server) = tokio::io::duplex(4096);
        let (_, mut client_writer) = tokio::io::split(client);
        let (server_reader, _) = tokio::io::split(server);

        // Simulate an old peer writing plain NDJSON.
        client_writer
            .write_all(b"{\"type\":\"Cancel\"}\n{\"type\":\"Shutdown\"}\n")
            .await
            .expect("write");
        drop(client_writer);

        let mut reader = MessageReader::new(server_reader);
        assert_eq!(
            reader.next_message().await.unwrap().as_deref(),
            Some(r#"{"type":"Cancel"}"#)
        );
        assert_eq!(
            reader.next_message().await.unwrap().as_deref(),
            Some(r#"{"type":"Shutdown"}"#)
        );
        assert!(reader.next_message().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn writer_refuses_oversized_payloads() {
        let (client, _server) = tokio::io::duplex(64);
        let (_, mut client_writer) = tokio::io::split(client);
        // The write side enforces the cap; the read side can never even see an
        // over-cap frame because any length > 16 MiB has a nonzero first byte
        // and is sniffed as legacy instead. (The reader's cap check stays as
        // defense in depth.)
        let huge = vec![b'x'; (MAX_FRAME_BYTES as usize) + 1];
        let err = write_message(&mut client_writer, &huge)
            .await
            .expect_err("over-cap frame must be refused");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn truncated_frame_reports_eof_not_hang() {
        let (client, server) = tokio::io::duplex(64);
        let (_, mut client_writer) = tokio::io::split(client);
        let (server_reader, _) = tokio::io::split(server);

        // Valid framed header claiming 100 bytes, but only 3 arrive then EOF.
        let mut partial = 100u32.to_be_bytes().to_vec();
        partial.extend_from_slice(b"abc");
        client_writer.write_all(&partial).await.expect("write");
        drop(client_writer);

        let mut reader = MessageReader::new(server_reader);
        let result = reader.next_message().await;
        assert!(result.is_err(), "truncated payload must surface an error");
    }

    #[test]
    fn ipc_endpoint_uses_runtime_dir() {
        let endpoint = ipc_endpoint_path("session-1");
        assert!(endpoint.starts_with(runtime_ipc_dir()));
        #[cfg(unix)]
        assert_eq!(
            endpoint.extension().and_then(|ext| ext.to_str()),
            Some("sock")
        );
        #[cfg(windows)]
        assert_eq!(
            endpoint.extension().and_then(|ext| ext.to_str()),
            Some("tcp")
        );
    }
}
