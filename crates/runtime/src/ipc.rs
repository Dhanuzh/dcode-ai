use dcode_ai_common::event::{AgentCommand, EventEnvelope};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, mpsc};

/// IPC server that broadcasts AgentEvents and receives AgentCommands
/// over a platform endpoint.
///
/// Unix uses Unix domain sockets. Windows uses loopback TCP because Unix domain
/// sockets are not universally available or ergonomic there.
pub struct IpcServer {
    socket_path: PathBuf,
}

impl IpcServer {
    pub fn new(session_id: &str) -> Self {
        let socket_path = ipc_endpoint_path(session_id);
        Self { socket_path }
    }

    pub fn socket_path(&self) -> PathBuf {
        self.socket_path.clone()
    }

    /// Start listening for client connections.
    pub async fn start(&self) -> Result<IpcHandle, IpcError> {
        let (event_tx, _) = broadcast::channel::<String>(256);
        let (command_tx, command_rx) = mpsc::unbounded_channel();
        start_transport_accept_loop(&self.socket_path, event_tx.clone(), command_tx).await?;

        Ok(IpcHandle {
            socket_path: self.socket_path.clone(),
            event_tx,
            command_rx,
        })
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
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(command) = serde_json::from_str::<AgentCommand>(&line) {
                let _ = command_tx.send(command);
            }
        }
    });

    let write_task = tokio::spawn(async move {
        while let Ok(line) = event_rx.recv().await {
            if writer.write_all(line.as_bytes()).await.is_err() {
                break;
            }
            if writer.write_all(b"\n").await.is_err() {
                break;
            }
        }
    });

    let _ = tokio::join!(read_task, write_task);
}

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
    socket_path: &std::path::Path,
    event_tx: broadcast::Sender<String>,
    command_tx: mpsc::UnboundedSender<AgentCommand>,
) -> Result<(), IpcError> {
    use tokio::net::UnixListener;

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
    let socket_path = socket_path.clone();

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
        let _ = tokio::fs::remove_file(socket_path).await;
    });
    Ok(())
}

#[cfg(windows)]
async fn start_transport_accept_loop(
    socket_path: &std::path::Path,
    event_tx: broadcast::Sender<String>,
    command_tx: mpsc::UnboundedSender<AgentCommand>,
) -> Result<(), IpcError> {
    use tokio::net::TcpListener;

    if let Some(parent) = socket_path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|err| IpcError::ConnectionFailed(err.to_string()))?;
    }
    let listener = TcpListener::bind(tcp_addr_from_endpoint(socket_path)?)
        .await
        .map_err(|err| IpcError::ConnectionFailed(err.to_string()))?;

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
    });
    Ok(())
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

fn spawn_event_reader_for_stream(
    stream: impl AsyncRead + Unpin + Send + 'static,
    tx: mpsc::Sender<EventEnvelope>,
) {
    tokio::spawn(async move {
        let reader = BufReader::new(stream);
        let mut lines = reader.lines();
        while let Ok(Some(line)) = lines.next_line().await {
            if let Ok(event) = serde_json::from_str::<EventEnvelope>(&line)
                && tx.send(event).await.is_err()
            {
                break;
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
    stream
        .write_all(line.as_bytes())
        .await
        .map_err(|err| IpcError::ConnectionFailed(err.to_string()))?;
    stream
        .write_all(b"\n")
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
