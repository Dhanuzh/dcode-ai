//! `/invite`: let someone on another computer join this session with one
//! line, `dcode-ai join <host:port/key> --name <them>`, no SSH.
//!
//! - The host listens on the local network. Each invite has a random key;
//!   the guest's connection is encrypted and authenticated with it (Noise
//!   `NNpsk0`), so nobody on the network can read it or get in without it.
//! - Knowing the key only lets you knock: the host is asked "Priya wants to
//!   join — let them in?" for every guest.
//! - The host side is a proxy onto the session's own IPC socket: it says
//!   hello, keeps the guest present, forwards their messages (as
//!   `(from Name)`) and sends them the session's events. Approvals and
//!   stopping a turn stay with the host unless the invite allows them.
//! - `/invite web` also serves the `/watch` page with a message box on the
//!   same port (plain HTTP).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use dcode_ai_common::event::{AgentCommand, AgentEvent, EventEnvelope, QuestionSelection};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};

/// The first bytes a guest sends (anything else is HTTP).
const MAGIC: &[u8; 5] = b"DCAI1";
const PATTERN: &str = "Noise_NNpsk0_25519_ChaChaPoly_BLAKE2s";
/// Plaintext per Noise message (a message is at most 65535 bytes).
const CHUNK: usize = 60_000;
/// Largest JSON message accepted.
const MAX_MESSAGE: usize = 8 * 1024 * 1024;
/// Guests connected or knocking at once.
const MAX_GUESTS: usize = 8;
/// The default port, fixed so a firewall rule is made once.
pub const DEFAULT_PORT: u16 = 8788;

/// `host:port/key`, what `/invite` prints. `host` may list several
/// addresses (`192.168.0.9,100.101.2.3`): the LAN one and the Tailscale
/// one; `join` tries each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    pub host: String,
    pub port: u16,
    pub key: String,
}

impl Invite {
    /// Parses `host:port/key` (also with a leading `dcai://`).
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().trim_start_matches("dcai://");
        let (addr, key) = s.split_once('/')?;
        let (host, port) = addr.rsplit_once(':')?;
        let port: u16 = port.parse().ok()?;
        let key = key.trim_end_matches('/');
        if host.is_empty() || key.len() < 16 || !key.chars().all(|c| c.is_ascii_alphanumeric()) {
            return None;
        }
        let hosts: Vec<&str> = host
            .split(',')
            .map(|h| h.trim().trim_matches(['[', ']']))
            .filter(|h| !h.is_empty())
            .collect();
        if hosts.is_empty() {
            return None;
        }
        Some(Self {
            host: hosts.join(","),
            port,
            key: key.to_string(),
        })
    }

    /// Every address to try, in order.
    pub fn hosts(&self) -> Vec<String> {
        self.host
            .split(',')
            .map(str::trim)
            .filter(|h| !h.is_empty())
            .map(str::to_string)
            .collect()
    }

    pub fn as_string(&self) -> String {
        let host = self
            .hosts()
            .iter()
            .map(|h| {
                if h.contains(':') {
                    format!("[{h}]")
                } else {
                    h.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(",");
        format!("{host}:{}/{}", self.port, self.key)
    }
}

/// Why a host couldn't be reached, in words: different networks, or
/// something on the way blocking it.
pub fn unreachable_hint(hosts: &[String]) -> String {
    let mine = crate::watch::lan_address().map(|ip| ip.to_string());
    let prefix = |ip: &str| ip.rsplit_once('.').map(|x| x.0.to_string());
    let lan_hosts: Vec<&String> = hosts.iter().filter(|h| !h.starts_with("100.")).collect();
    if let (Some(me), Some(h)) = (mine.as_deref(), lan_hosts.first())
        && prefix(me).is_some()
        && prefix(me) != prefix(h)
    {
        return format!(
            "you're on {}.x and the host is on {}.x — different networks. Join the same Wi-Fi, \
             share a phone hotspot, or use Tailscale on both (dcode-ai setup tailscale).",
            prefix(me).unwrap_or_default(),
            prefix(h).unwrap_or_default()
        );
    }
    "same network, but nothing answered: the host's firewall may block the port, or this Wi-Fi \
     keeps devices apart (office and guest Wi-Fi often do) — try a phone hotspot or Tailscale \
     (dcode-ai setup tailscale)."
        .into()
}

/// A fresh invite key: 24 random letters and digits (~142 bits).
pub fn new_key() -> String {
    use rand::Rng;
    const ABC: &[u8] = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut rng = rand::thread_rng();
    (0..24)
        .map(|_| ABC[rng.gen_range(0..ABC.len())] as char)
        .collect()
}

fn psk(key: &str) -> [u8; 32] {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(b"dcode-ai invite v1");
    h.update(key.as_bytes());
    h.finalize().into()
}

// ── Encrypted framing ────────────────────────────────────────────────

fn io_err(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, data: &[u8]) -> std::io::Result<()> {
    let len = u16::try_from(data.len()).map_err(io_err)?;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(data).await?;
    Ok(())
}

async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> std::io::Result<Vec<u8>> {
    let mut len = [0u8; 2];
    r.read_exact(&mut len).await?;
    let mut buf = vec![0u8; u16::from_be_bytes(len) as usize];
    r.read_exact(&mut buf).await?;
    Ok(buf)
}

/// One side's session keys after the handshake.
#[derive(Clone)]
struct Cipher(Arc<Mutex<snow::TransportState>>);

impl Cipher {
    /// Encrypts `value` as one or more frames and writes them.
    async fn send<W: AsyncWrite + Unpin>(&self, w: &mut W, value: &Value) -> std::io::Result<()> {
        let bytes = serde_json::to_vec(value).map_err(io_err)?;
        let chunks: Vec<&[u8]> = if bytes.is_empty() {
            vec![&[]]
        } else {
            bytes.chunks(CHUNK).collect()
        };
        let mut out = Vec::new();
        for (i, chunk) in chunks.iter().enumerate() {
            let mut plain = Vec::with_capacity(chunk.len() + 1);
            plain.push(u8::from(i + 1 < chunks.len()));
            plain.extend_from_slice(chunk);
            let mut ct = vec![0u8; plain.len() + 16];
            let n = self
                .0
                .lock()
                .map_err(io_err)?
                .write_message(&plain, &mut ct)
                .map_err(io_err)?;
            out.extend_from_slice(&(n as u16).to_be_bytes());
            out.extend_from_slice(&ct[..n]);
        }
        w.write_all(&out).await?;
        w.flush().await
    }

    /// Reads and decrypts one message (however many frames it spans).
    async fn recv<R: AsyncRead + Unpin>(&self, r: &mut R) -> std::io::Result<Value> {
        let mut msg = Vec::new();
        loop {
            let ct = read_frame(r).await?;
            let mut plain = vec![0u8; ct.len()];
            let n = self
                .0
                .lock()
                .map_err(io_err)?
                .read_message(&ct, &mut plain)
                .map_err(io_err)?;
            let Some((&more, data)) = plain[..n].split_first() else {
                return Err(io_err("empty frame"));
            };
            msg.extend_from_slice(data);
            if msg.len() > MAX_MESSAGE {
                return Err(io_err("message too large"));
            }
            if more == 0 {
                break;
            }
        }
        serde_json::from_slice(&msg).map_err(io_err)
    }
}

/// One side of a guest connection (TCP, or a peer-to-peer QUIC stream).
pub(crate) type LinkRead = Box<dyn AsyncRead + Unpin + Send>;
pub(crate) type LinkWrite = Box<dyn AsyncWrite + Unpin + Send>;

async fn handshake_guest(
    read: &mut LinkRead,
    write: &mut LinkWrite,
    key: &str,
) -> std::io::Result<Cipher> {
    let mut noise = snow::Builder::new(PATTERN.parse().map_err(io_err)?)
        .psk(0, &psk(key))
        .build_initiator()
        .map_err(io_err)?;
    write.write_all(MAGIC).await?;
    let mut buf = vec![0u8; 1024];
    let n = noise.write_message(&[], &mut buf).map_err(io_err)?;
    write_frame(write, &buf[..n]).await?;
    write.flush().await?;
    let reply = read_frame(read).await?;
    noise.read_message(&reply, &mut buf).map_err(io_err)?;
    Ok(Cipher(Arc::new(Mutex::new(
        noise.into_transport_mode().map_err(io_err)?,
    ))))
}

/// After [`MAGIC`] was read.
async fn handshake_host(
    read: &mut LinkRead,
    write: &mut LinkWrite,
    key: &str,
) -> std::io::Result<Cipher> {
    let mut noise = snow::Builder::new(PATTERN.parse().map_err(io_err)?)
        .psk(0, &psk(key))
        .build_responder()
        .map_err(io_err)?;
    let mut buf = vec![0u8; 1024];
    let first = read_frame(read).await?;
    noise.read_message(&first, &mut buf).map_err(io_err)?;
    let n = noise.write_message(&[], &mut buf).map_err(io_err)?;
    write_frame(write, &buf[..n]).await?;
    write.flush().await?;
    Ok(Cipher(Arc::new(Mutex::new(
        noise.into_transport_mode().map_err(io_err)?,
    ))))
}

// ── Knocking: the host decides ───────────────────────────────────────

/// How the host lets someone in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admit {
    No,
    /// Sees everything, chats with people, joins calls — nothing they send
    /// reaches the agent.
    Watch,
    /// Can also ask the agent to do things (their messages become prompts,
    /// under the host's permission mode; approvals stay with the host).
    Drive,
}

impl Admit {
    pub fn admitted(self) -> bool {
        self != Admit::No
    }
}

/// Someone asks to join. Answer on `reply` (dropping it means no).
pub struct Knock {
    pub name: String,
    pub from: String,
    pub reply: oneshot::Sender<Admit>,
}

/// Knocks waiting for an answer in the TUI, by question id.
static KNOCKS: LazyLock<Mutex<HashMap<String, oneshot::Sender<Admit>>>> =
    LazyLock::new(Default::default);

/// The question id prefix of a knock shown in the TUI.
pub const KNOCK_QUESTION: &str = "invite-knock-";

/// Wait for the TUI's answer to the knock shown as question `id`.
pub fn register_knock(id: &str, reply: oneshot::Sender<Admit>) {
    if let Ok(mut m) = KNOCKS.lock() {
        m.insert(id.to_string(), reply);
    }
}

/// The TUI answered question `id`: if it is a knock, let them in or not.
/// Returns whether it was one.
pub fn answer_knock(id: &str, selection: &QuestionSelection) -> bool {
    if !id.starts_with(KNOCK_QUESTION) {
        return false;
    }
    let reply = KNOCKS.lock().ok().and_then(|mut m| m.remove(id));
    if let Some(reply) = reply {
        let _ = reply.send(admit_for(selection));
    }
    true
}

/// What the host picked in the knock prompt.
pub fn admit_for(selection: &QuestionSelection) -> Admit {
    match selection {
        QuestionSelection::Option { option_id } => match option_id.as_str() {
            "drive" | "allow" => Admit::Drive,
            "watch" => Admit::Watch,
            _ => Admit::No,
        },
        _ => Admit::No,
    }
}

/// Unanswered after a while: no. Returns whether it was still waiting.
pub fn expire_knock(id: &str) -> bool {
    KNOCKS
        .lock()
        .ok()
        .and_then(|mut m| m.remove(id))
        .map(|r| r.send(Admit::No))
        .is_some()
}

/// Ask the host; `No` when they say no, don't answer, or are gone.
pub async fn ask_host(knocks: &mpsc::Sender<Knock>, name: &str, from: &str) -> Admit {
    let (tx, rx) = oneshot::channel();
    let knock = Knock {
        name: name.to_string(),
        from: from.to_string(),
        reply: tx,
    };
    if knocks.send(knock).await.is_err() {
        return Admit::No;
    }
    match tokio::time::timeout(Duration::from_secs(90), rx).await {
        Ok(Ok(a)) => a,
        _ => Admit::No,
    }
}

// ── Host ─────────────────────────────────────────────────────────────

pub struct HostOptions {
    /// 0 = [`DEFAULT_PORT`].
    pub port: u16,
    pub web: bool,
    pub let_approve: bool,
    /// The session's IPC socket.
    pub socket: PathBuf,
    /// The session's event log (for the web page).
    pub log_path: PathBuf,
    pub title: String,
    pub knocks: mpsc::Sender<Knock>,
    /// Where web watchers are announced.
    pub presence: Option<crate::watch::Presence>,
    /// Shown to guests: who is hosting, the project, the session, its model.
    pub host_name: String,
    pub project: String,
    pub session_id: String,
    pub model: String,
    /// Let people in without asking (the private local invite: only your
    /// own user can read its key).
    pub auto_admit: bool,
    /// The call to share with other invites (one call for everyone).
    pub hub: Option<CallHubHandle>,
}

/// One voice call shared by every invite of a session.
pub type CallHubHandle = Arc<Mutex<CallHub>>;

pub fn new_call_hub() -> CallHubHandle {
    Arc::new(Mutex::new(CallHub::default()))
}

/// Someone in through this invite (for `/people` and `/kick`).
#[derive(Debug, Clone)]
pub struct GuestInfo {
    pub name: String,
    pub from: String,
}

/// A running invite; dropping it (or `stop`) ends it and disconnects
/// everyone who came in through it.
pub struct InviteHandle {
    pub invite: Invite,
    /// This machine's Tailscale address, when the line includes one.
    pub tailscale: Option<String>,
    pub web_url: Option<String>,
    pub let_approve: bool,
    /// The line has a peer-to-peer id: joins work from any network.
    pub p2p: bool,
    host: Arc<Host>,
    stop: tokio::sync::watch::Sender<bool>,
}

impl InviteHandle {
    pub fn port(&self) -> u16 {
        self.invite.port
    }
    /// What to send the guest.
    pub fn command(&self) -> String {
        format!(
            "dcode-ai join {} --name <your name>",
            self.invite.as_string()
        )
    }
    pub fn stop(&self) {
        let _ = self.stop.send(true);
    }
    /// Guests connected right now (terminal and browser).
    pub fn guests(&self) -> Vec<GuestInfo> {
        let mut out: Vec<GuestInfo> = self
            .host
            .connected
            .lock()
            .map(|m| {
                m.values()
                    .map(|c| GuestInfo {
                        name: c.name.clone(),
                        from: c.from.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        if let Some(chat) = &self.host.web_chat
            && let Ok(g) = chat.guests.lock()
        {
            out.extend(g.values().map(|(_, name)| GuestInfo {
                name: name.clone(),
                from: "the browser".into(),
            }));
        }
        out
    }
    /// Removes every guest called `name` (any case); how many.
    pub fn kick(&self, name: &str) -> usize {
        self.host.kick(name)
    }

    /// The host joins the call: their voiced frames (`mic`) go to every
    /// guest in it, and what guests say plays on `call`.
    pub fn call_join(&self, call: crate::voice::Call, mut mic: mpsc::UnboundedReceiver<Vec<u8>>) {
        if let Ok(mut h) = self.host.call.lock() {
            h.host = Some(call);
        }
        self.host.announce(AgentEvent::CallJoined {
            person: HOST_PERSON.into(),
            name: self.host.host_name.clone(),
        });
        let host = self.host.clone();
        tokio::spawn(async move {
            while let Some(packet) = mic.recv().await {
                let still = host.call.lock().map(|h| h.host.is_some()).unwrap_or(false);
                if !still {
                    break;
                }
                host.forward(HOST_PERSON, &packet);
            }
        });
    }

    /// The host leaves the call.
    pub fn call_leave(&self) {
        let call = self.host.call.lock().ok().and_then(|mut h| h.host.take());
        if let Some(call) = call {
            call.hang_up();
            self.host.announce(AgentEvent::CallLeft {
                person: HOST_PERSON.into(),
                name: self.host.host_name.clone(),
            });
        }
    }

    /// Guests in the call now (person ids).
    pub fn in_call(&self) -> Vec<String> {
        self.host
            .call
            .lock()
            .map(|h| h.guests.keys().cloned().collect())
            .unwrap_or_default()
    }
}

impl Drop for InviteHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Start listening on every interface.
pub async fn host(opts: HostOptions) -> std::io::Result<InviteHandle> {
    let port = if opts.port == 0 {
        DEFAULT_PORT
    } else {
        opts.port
    };
    let listener = TcpListener::bind(("0.0.0.0", port)).await?;
    let port = listener.local_addr()?.port();
    let mut handle = host_on(Some(listener), opts, port, None).await?;
    // Reachable over Tailscale too: put its address in the line.
    let ts = tokio::task::spawn_blocking(crate::tailscale::ip)
        .await
        .ok()
        .flatten();
    if let Some(ts) = ts
        && !handle.invite.hosts().contains(&ts)
    {
        handle.invite.host = format!("{},{ts}", handle.invite.host);
        handle.tailscale = Some(ts);
    }
    Ok(handle)
}

/// [`host`], plus a direct peer-to-peer id in the line, so people on other
/// networks join without a VPN or a relay of yours.
pub async fn host_with_p2p(opts: HostOptions) -> std::io::Result<InviteHandle> {
    let mut handle = host(opts).await?;
    attach_p2p(&mut handle).await;
    Ok(handle)
}

async fn attach_p2p(handle: &mut InviteHandle) {
    match crate::p2p::listen().await {
        Ok(ep) => {
            // A moment to reach the network, so the first knock finds us.
            let _ = tokio::time::timeout(Duration::from_secs(4), ep.online()).await;
            handle.invite.host =
                format!("{},{}", handle.invite.host, crate::p2p::encode_id(&ep.id()));
            handle.p2p = true;
            tokio::spawn(p2p_accept(handle.host.clone(), ep, handle.stop.subscribe()));
        }
        Err(e) => tracing::warn!("peer-to-peer invite unavailable: {e}"),
    }
}

/// Takes guests arriving over the peer-to-peer endpoint until the invite
/// stops.
async fn p2p_accept(
    shared: Arc<Host>,
    ep: iroh::Endpoint,
    stop_rx: tokio::sync::watch::Receiver<bool>,
) {
    let mut stop = stop_rx.clone();
    loop {
        let next = tokio::select! {
            _ = stop.changed() => break,
            n = crate::p2p::accept(&ep) => n,
        };
        let Some(next) = next else { break };
        let shared = shared.clone();
        let stop = stop_rx.clone();
        tokio::spawn(async move {
            let Ok((mut read, write, conn)) = next else {
                return;
            };
            let mut first = [0u8; 5];
            let magic =
                tokio::time::timeout(Duration::from_secs(10), read.read_exact(&mut first)).await;
            if !matches!(magic, Ok(Ok(_))) || &first != MAGIC {
                return;
            }
            if shared.guests.fetch_add(1, Ordering::SeqCst) >= MAX_GUESTS {
                shared.guests.fetch_sub(1, Ordering::SeqCst);
                return;
            }
            let _ = shared
                .guest(read, write, "a direct link".into(), stop)
                .await;
            shared.guests.fetch_sub(1, Ordering::SeqCst);
            drop(conn);
        });
    }
    ep.close().await;
}

/// The private local invite: 127.0.0.1 on a free port, nobody asked (only
/// your own user can read the key from [`local_join_file`]).
pub async fn host_local(mut opts: HostOptions) -> std::io::Result<InviteHandle> {
    opts.auto_admit = true;
    opts.web = false;
    let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let handle = host_on(Some(listener), opts, port, None).await?;
    Ok(handle)
}

/// Where a session's private local invite is written for `dcode-ai join`.
pub fn local_join_file(session_id: &str) -> PathBuf {
    dcode_ai_runtime::ipc::runtime_ipc_dir().join(format!("{session_id}.join"))
}

/// Writes the local invite where only this user can read it.
pub fn write_local_join(session_id: &str, invite: &Invite) -> std::io::Result<()> {
    let path = local_join_file(session_id);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let local = Invite {
        host: "127.0.0.1".into(),
        ..invite.clone()
    };
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        f.write_all(local.as_string().as_bytes())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(&path, local.as_string())
    }
}

/// The local invite for a running session, if its host wrote one.
pub fn read_local_join(session_id: &str) -> Option<Invite> {
    let text = std::fs::read_to_string(local_join_file(session_id)).ok()?;
    Invite::parse(text.trim())
}

/// Where guests reach this invite through a relay.
struct RelayTarget {
    /// `host:port` of the relay.
    addr: String,
    room: String,
    secret: String,
}

/// An invite reached through a relay (`dcode-ai relay serve` somewhere both
/// sides can reach): for people on other networks without a VPN. The relay
/// only pairs connections; the handshake and everything after it stay
/// end-to-end encrypted with the invite's key, which it never sees.
pub async fn host_relay(mut opts: HostOptions, relay: &str) -> std::io::Result<InviteHandle> {
    let relay = relay.trim();
    let addr = if relay
        .rsplit_once(':')
        .is_some_and(|(_, p)| p.parse::<u16>().is_ok())
    {
        relay.to_string()
    } else {
        format!("{relay}:{}", crate::relay::DEFAULT_PORT)
    };
    // Check it answers before printing a line that wouldn't work.
    tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(&addr))
        .await
        .map_err(|_| io_err(format!("the relay {addr} didn't answer")))??;
    opts.web = false;
    let port = addr
        .rsplit_once(':')
        .and_then(|(_, p)| p.parse().ok())
        .unwrap_or(crate::relay::DEFAULT_PORT);
    let target = RelayTarget {
        addr,
        room: new_key(),
        secret: new_key(),
    };
    host_on(None, opts, port, Some(target)).await
}

async fn host_on(
    listener: Option<TcpListener>,
    opts: HostOptions,
    port: u16,
    relay: Option<RelayTarget>,
) -> std::io::Result<InviteHandle> {
    let ip = crate::watch::lan_address()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "127.0.0.1".into());
    let key = new_key();
    let invite_host = match &relay {
        Some(r) => format!(
            "{}@{}",
            r.room,
            r.addr.rsplit_once(':').map(|(h, _)| h).unwrap_or(&r.addr)
        ),
        None => ip.clone(),
    };
    let invite = Invite {
        host: invite_host,
        port,
        key: key.clone(),
    };
    // The web page has its own token: knowing the page's link doesn't give
    // the CLI key away, and the other way round.
    let web_token = opts.web.then(new_key);
    let web_url = web_token
        .as_ref()
        .map(|t| format!("http://{ip}:{port}/?t={t}"));
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let web_chat = opts.web.then(|| {
        let mut chat = crate::watch::WebChat::new(opts.knocks.clone(), opts.socket.clone());
        chat.let_approve = opts.let_approve;
        Arc::new(chat)
    });
    let shared = Arc::new(Host {
        web_chat,
        key,
        web_token,
        let_approve: opts.let_approve,
        socket: opts.socket,
        log_path: opts.log_path,
        title: opts.title,
        knocks: opts.knocks,
        presence: opts.presence,
        host_name: opts.host_name,
        project: opts.project,
        session_id: opts.session_id,
        model: opts.model,
        guests: AtomicUsize::new(0),
        rejoin: Default::default(),
        connected: Default::default(),
        pending_bye: Default::default(),
        call: opts.hub.unwrap_or_else(new_call_hub),
        auto_admit: opts.auto_admit,
    });
    if let Some(r) = relay {
        tokio::spawn(relay_control(shared.clone(), r, stop_rx.clone()));
    }
    let accept = shared.clone();
    let mut stop = stop_rx.clone();
    tokio::spawn(async move {
        let Some(listener) = listener else { return };
        loop {
            tokio::select! {
                _ = stop.changed() => break,
                conn = listener.accept() => {
                    let Ok((stream, peer)) = conn else { continue };
                    let shared = accept.clone();
                    if shared.guests.fetch_add(1, Ordering::SeqCst) >= MAX_GUESTS {
                        shared.guests.fetch_sub(1, Ordering::SeqCst);
                        continue;
                    }
                    let stop = stop_rx.clone();
                    tokio::spawn(async move {
                        let _ = shared.connection(stream, peer.ip().to_string(), stop).await;
                        shared.guests.fetch_sub(1, Ordering::SeqCst);
                    });
                }
            }
        }
    });
    Ok(InviteHandle {
        invite,
        tailscale: None,
        web_url,
        let_approve: opts.let_approve,
        p2p: false,
        host: shared,
        stop: stop_tx,
    })
}

/// Keeps the invite registered with the relay, and opens a data link for
/// each guest it announces (reconnecting if the relay drops).
async fn relay_control(
    shared: Arc<Host>,
    r: RelayTarget,
    stop_rx: tokio::sync::watch::Receiver<bool>,
) {
    let mut stop = stop_rx.clone();
    loop {
        if *stop.borrow() {
            return;
        }
        if let Ok(mut s) = TcpStream::connect(&r.addr).await
            && s.write_all(format!("HOST {} {}\n", r.room, r.secret).as_bytes())
                .await
                .is_ok()
            && crate::relay::read_line(&mut s).await.ok().as_deref() == Some("OK")
        {
            loop {
                tokio::select! {
                    _ = stop.changed() => return,
                    line = crate::relay::read_line(&mut s) => {
                        let Ok(line) = line else { break };
                        let Some(n) = line.strip_prefix("CONN ").map(str::to_string) else {
                            continue;
                        };
                        let shared = shared.clone();
                        let stop = stop_rx.clone();
                        let (addr, room, secret) = (r.addr.clone(), r.room.clone(), r.secret.clone());
                        tokio::spawn(async move {
                            let Ok(mut data) = TcpStream::connect(&addr).await else { return };
                            if data
                                .write_all(format!("PAIR {room} {n} {secret}\n").as_bytes())
                                .await
                                .is_err()
                            {
                                return;
                            }
                            if shared.guests.fetch_add(1, Ordering::SeqCst) >= MAX_GUESTS {
                                shared.guests.fetch_sub(1, Ordering::SeqCst);
                                return;
                            }
                            let _ = shared.connection(data, "the relay".into(), stop).await;
                            shared.guests.fetch_sub(1, Ordering::SeqCst);
                        });
                    }
                }
            }
        }
        tokio::select! {
            _ = stop.changed() => return,
            _ = tokio::time::sleep(Duration::from_secs(3)) => {}
        }
    }
}

/// A guest connected now.
struct Connected {
    name: String,
    from: String,
    kick: tokio::sync::watch::Sender<bool>,
    /// Which connection (a reconnect replaces the entry).
    conn: u64,
}

/// How long a dropped guest stays "here" waiting to reconnect.
const REJOIN_GRACE: Duration = Duration::from_secs(20);

struct Host {
    key: String,
    web_token: Option<String>,
    let_approve: bool,
    socket: PathBuf,
    log_path: PathBuf,
    title: String,
    knocks: mpsc::Sender<Knock>,
    presence: Option<crate::watch::Presence>,
    web_chat: Option<Arc<crate::watch::WebChat>>,
    host_name: String,
    project: String,
    session_id: String,
    model: String,
    guests: AtomicUsize,
    /// Rejoin token → (person id, name, role), while the invite is open.
    rejoin: Mutex<HashMap<String, (String, String, Admit)>>,
    /// Person id → who is connected now.
    connected: Mutex<HashMap<String, Connected>>,
    /// Person id → their delayed "left" (cancelled when they come back).
    pending_bye: Mutex<HashMap<String, tokio::task::AbortHandle>>,
    /// The voice call (shared with the session's other invites).
    call: CallHubHandle,
    auto_admit: bool,
}

/// Who is in the call, and where to send them audio. The host forwards
/// each person's audio to everyone else; nobody's audio comes back to them.
#[derive(Default)]
pub struct CallHub {
    /// Guests in the call → their connection's audio queue (bounded: a
    /// guest who falls behind loses audio instead of hearing it late).
    guests: HashMap<String, mpsc::Sender<Value>>,
    /// The host, when they're in the call.
    host: Option<crate::voice::Call>,
}

/// Audio frames (20 ms each) that may wait for one slow connection.
const AUDIO_BACKLOG: usize = 25;

/// The host's person id in calls and the office.
pub const HOST_PERSON: &str = "owner";

/// Why a guest's connection ended.
enum Ended {
    /// Dropped: they may reconnect.
    Dropped,
    /// They left, were kicked, or the invite/session ended.
    Gone,
}

impl Host {
    fn ipc(&self) -> dcode_ai_runtime::ipc::IpcClient {
        dcode_ai_runtime::ipc::IpcClient::new(self.socket.clone())
    }

    fn announce(&self, ev: AgentEvent) {
        if let Some(tx) = &self.presence {
            let _ = tx.try_send(ev);
        }
    }

    /// Audio from `from` to everyone else in the call.
    fn forward(&self, from: &str, packet: &[u8]) {
        use base64::Engine as _;
        let Ok(hub) = self.call.lock() else { return };
        if hub.guests.is_empty() && hub.host.is_none() {
            return;
        }
        if from != HOST_PERSON
            && let Some(host) = &hub.host
        {
            host.play(from, packet);
        }
        let msg = json!({
            "t": "audio",
            "from": from,
            "d": base64::engine::general_purpose::STANDARD.encode(packet),
        });
        for (who, tx) in &hub.guests {
            if who != from {
                // Full: they're behind (weak Wi-Fi) — drop rather than delay.
                let _ = tx.try_send(msg.clone());
            }
        }
    }

    fn call_join(&self, person: &str, name: &str, tx: mpsc::Sender<Value>) {
        let fresh = self
            .call
            .lock()
            .map(|mut h| h.guests.insert(person.to_string(), tx).is_none())
            .unwrap_or(false);
        if fresh {
            self.announce(AgentEvent::CallJoined {
                person: person.to_string(),
                name: name.to_string(),
            });
        }
    }

    fn call_leave(&self, person: &str, name: &str) {
        let was = self
            .call
            .lock()
            .map(|mut h| {
                if let Some(host) = &h.host {
                    host.forget(person);
                }
                h.guests.remove(person).is_some()
            })
            .unwrap_or(false);
        if was {
            self.announce(AgentEvent::CallLeft {
                person: person.to_string(),
                name: name.to_string(),
            });
        }
    }

    fn kick(&self, name: &str) -> usize {
        let want = name.trim().to_lowercase();
        let mut n = 0;
        if let Ok(m) = self.connected.lock() {
            for c in m.values().filter(|c| c.name.to_lowercase() == want) {
                let _ = c.kick.send(true);
                n += 1;
            }
        }
        if let Ok(mut r) = self.rejoin.lock() {
            r.retain(|_, (_, nm, _)| nm.to_lowercase() != want);
        }
        if let Some(chat) = &self.web_chat {
            let gone: Vec<String> = chat
                .guests
                .lock()
                .map(|mut g| {
                    let ids: Vec<String> = g
                        .iter()
                        .filter(|(_, (_, nm))| nm.to_lowercase() == want)
                        .map(|(_, (p, _))| p.clone())
                        .collect();
                    g.retain(|_, (_, nm)| nm.to_lowercase() != want);
                    ids
                })
                .unwrap_or_default();
            n += gone.len();
            for person in gone {
                let ipc = self.ipc();
                tokio::spawn(async move {
                    let _ = ipc.send_command(&AgentCommand::Bye { person }).await;
                });
            }
        }
        n
    }

    async fn connection(
        self: &Arc<Self>,
        mut stream: TcpStream,
        from: String,
        stop: tokio::sync::watch::Receiver<bool>,
    ) -> std::io::Result<()> {
        // A guest says DCAI1 first; a browser starts with "GET /" or "POST".
        let mut first = [0u8; 5];
        let peeked = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let n = stream.peek(&mut first).await?;
                if n == 0 {
                    return Err(io_err("closed"));
                }
                if n >= first.len() || !MAGIC.starts_with(&first[..n]) {
                    return Ok::<_, std::io::Error>(());
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        if !matches!(peeked, Ok(Ok(()))) {
            return Ok(());
        }
        if &first == MAGIC {
            stream.read_exact(&mut first).await?;
            let (read, write) = stream.into_split();
            return self
                .guest(Box::new(read), Box::new(write), from, stop)
                .await;
        }
        match (&self.web_token, &self.web_chat) {
            (Some(token), Some(chat)) => {
                crate::watch::serve(
                    stream,
                    &self.log_path,
                    token,
                    &self.title,
                    stop,
                    self.presence.clone(),
                    Some(chat.clone()),
                )
                .await
            }
            _ => Ok(()),
        }
    }

    /// A guest that sent [`MAGIC`]: handshake, knock, then the session.
    pub(crate) async fn guest(
        self: &Arc<Self>,
        mut read: LinkRead,
        mut write: LinkWrite,
        from: String,
        mut stop: tokio::sync::watch::Receiver<bool>,
    ) -> std::io::Result<()> {
        let cipher = tokio::time::timeout(
            Duration::from_secs(10),
            handshake_host(&mut read, &mut write, &self.key),
        )
        .await
        .map_err(io_err)??;
        let hello = tokio::time::timeout(Duration::from_secs(10), cipher.recv(&mut read))
            .await
            .map_err(io_err)??;
        let asked_name = dcode_ai_runtime::supervisor::clean_person_name(
            hello.get("name").and_then(Value::as_str).unwrap_or(""),
        );

        // Coming back after a drop: no knock, same person.
        let rejoining = hello.get("rejoin").and_then(Value::as_str).map(|t| {
            (
                t.to_string(),
                self.rejoin.lock().ok().and_then(|m| m.get(t).cloned()),
            )
        });
        let (person, name, token, resumed, role) = match rejoining {
            Some((token, Some((person, name, role)))) => (person, name, token, true, role),
            Some((_, None)) => {
                let _ = cipher
                    .send(
                        &mut write,
                        &json!({"t": "refused", "reason": "you can't rejoin (removed, or the invite was renewed)", "final": true}),
                    )
                    .await;
                return Ok(());
            }
            None => {
                let role = if self.auto_admit {
                    Admit::Drive
                } else {
                    tokio::select! {
                        a = ask_host(&self.knocks, &asked_name, &from) => a,
                        _ = stop.changed() => Admit::No,
                        _ = read.read_u8() => Admit::No,
                    }
                };
                if !role.admitted() {
                    let _ = cipher
                        .send(
                            &mut write,
                            &json!({"t": "refused", "reason": "the host said no"}),
                        )
                        .await;
                    return Ok(());
                }
                let person = crate::people::new_person_id("guest");
                let token = new_key();
                if let Ok(mut m) = self.rejoin.lock() {
                    m.insert(token.clone(), (person.clone(), asked_name.clone(), role));
                }
                (person, asked_name, token, false, role)
            }
        };

        let ipc = self.ipc();
        let mut events = ipc.connect().await.map_err(io_err)?;
        cipher
            .send(
                &mut write,
                &json!({
                    "t": "admitted",
                    "title": self.title,
                    "let_approve": self.let_approve && role == Admit::Drive,
                    "role": if role == Admit::Drive { "drive" } else { "watch" },
                    "host": self.host_name,
                    "project": self.project,
                    "session_id": self.session_id,
                    "model": self.model,
                    "you": person,
                    "rejoin": token,
                    "resumed": resumed,
                }),
            )
            .await?;
        if !resumed {
            // The conversation so far, then live events.
            let log = self.log_path.clone();
            let history = tokio::task::spawn_blocking(move || {
                crate::tui::replay::history_events(&log, 6 * 1024 * 1024)
            })
            .await
            .unwrap_or_default();
            cipher
                .send(&mut write, &json!({"t": "history", "events": history}))
                .await?;
        }
        // Back within the grace period: they never left.
        let came_back = self
            .pending_bye
            .lock()
            .ok()
            .and_then(|mut m| m.remove(&person))
            .map(|h| h.abort())
            .is_some();
        if !came_back {
            let _ = ipc
                .send_command(&AgentCommand::Hello {
                    person: person.clone(),
                    name: name.clone(),
                })
                .await;
        }
        static CONN: AtomicUsize = AtomicUsize::new(1);
        let conn = CONN.fetch_add(1, Ordering::Relaxed) as u64;
        let (kick_tx, mut kicked) = tokio::sync::watch::channel(false);
        if let Ok(mut m) = self.connected.lock() {
            m.insert(
                person.clone(),
                Connected {
                    name: name.clone(),
                    from: from.clone(),
                    kick: kick_tx,
                    conn,
                },
            );
        }

        // Their messages, read on their own task (a read cut off by
        // `select!` would lose part of a frame).
        let (msg_tx, mut msgs) = mpsc::channel::<Value>(16);
        let reader_cipher = cipher.clone();
        let reader = tokio::spawn(async move {
            while let Ok(m) = reader_cipher.recv(&mut read).await {
                if msg_tx.send(m).await.is_err() {
                    break;
                }
            }
        });
        let fin = |text: &str| json!({"t": "note", "text": text, "final": true});
        // Audio (and call notes) for this guest from the call hub.
        let (audio_tx, mut audio_rx) = mpsc::channel::<Value>(AUDIO_BACKLOG);
        let mut beat = tokio::time::interval(Duration::from_secs(10));
        beat.tick().await;
        let ended = loop {
            tokio::select! {
                _ = stop.changed() => {
                    let _ = cipher.send(&mut write, &fin("the host ended the invite")).await;
                    break Ended::Gone;
                }
                _ = kicked.changed() => {
                    let _ = cipher.send(&mut write, &fin("the host removed you from the session")).await;
                    break Ended::Gone;
                }
                ev = events.recv() => {
                    let Some(env) = ev else {
                        let _ = cipher.send(&mut write, &fin("the session ended")).await;
                        break Ended::Gone;
                    };
                    // (PersonSeen too: without it, everyone who joined
                    // would seem to leave after 25 s on the guest's screen.)
                    if cipher.send(&mut write, &json!({"t": "event", "e": env})).await.is_err() {
                        break Ended::Dropped;
                    }
                }
                _ = beat.tick() => {
                    let _ = ipc.send_command(&AgentCommand::Heartbeat { person: person.clone() }).await;
                }
                a = audio_rx.recv() => {
                    if let Some(a) = a
                        && cipher.send(&mut write, &a).await.is_err()
                    {
                        break Ended::Dropped;
                    }
                }
                m = msgs.recv() => {
                    let Some(m) = m else { break Ended::Dropped };
                    match m.get("t").and_then(Value::as_str) {
                        Some("bye") => break Ended::Gone,
                        Some("call_join") => {
                            self.call_join(&person, &name, audio_tx.clone());
                            continue;
                        }
                        Some("call_leave") => {
                            self.call_leave(&person, &name);
                            continue;
                        }
                        Some("audio") => {
                            use base64::Engine as _;
                            let in_call = self
                                .call
                                .lock()
                                .map(|h| h.guests.contains_key(&person))
                                .unwrap_or(false);
                            if in_call
                                && let Some(packet) = m
                                    .get("d")
                                    .and_then(Value::as_str)
                                    .and_then(|d| base64::engine::general_purpose::STANDARD.decode(d).ok())
                                    .filter(|p| p.len() <= 512)
                            {
                                self.forward(&person, &packet);
                            }
                            continue;
                        }
                        _ => {}
                    }
                    let note = self.carry_out(&ipc, &person, &name, role, &m).await;
                    if let Some(text) = note {
                        let _ = cipher.send(&mut write, &json!({"t": "note", "text": text})).await;
                    }
                }
            }
        };
        reader.abort();
        // A dropped or departed guest is out of the call (they rejoin it
        // after reconnecting).
        self.call_leave(&person, &name);
        // Only forget them if this connection is still theirs (a reconnect
        // may already have replaced it).
        let still_ours = self
            .connected
            .lock()
            .map(|mut m| {
                let ours = m.get(&person).is_some_and(|c| c.conn == conn);
                if ours {
                    m.remove(&person);
                }
                ours
            })
            .unwrap_or(false);
        // Replaced by a newer connection of theirs: nothing to announce.
        if !still_ours {
            return Ok(());
        }
        match ended {
            Ended::Gone => {
                if *kicked.borrow()
                    && let Ok(mut r) = self.rejoin.lock()
                {
                    r.remove(&token);
                }
                let _ = ipc.send_command(&AgentCommand::Bye { person }).await;
            }
            Ended::Dropped => {
                // Say they left only if they don't come back soon.
                let host = self.clone();
                let who = person.clone();
                let task = tokio::spawn(async move {
                    tokio::time::sleep(REJOIN_GRACE).await;
                    if let Ok(mut m) = host.pending_bye.lock() {
                        m.remove(&who);
                    }
                    let _ = host
                        .ipc()
                        .send_command(&AgentCommand::Bye { person: who })
                        .await;
                });
                if let Ok(mut m) = self.pending_bye.lock() {
                    m.insert(person, task.abort_handle());
                }
            }
        }
        Ok(())
    }

    /// Carries out what a guest sent; a note back when it isn't allowed.
    async fn carry_out(
        &self,
        ipc: &dcode_ai_runtime::ipc::IpcClient,
        person: &str,
        name: &str,
        role: Admit,
        m: &Value,
    ) -> Option<String> {
        let text = |k: &str| m.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let kind = m.get("t").and_then(Value::as_str)?;
        // Watching: chat and calls only — nothing reaches the agent.
        if role != Admit::Drive
            && matches!(
                kind,
                "say" | "todo" | "todo_done" | "approve" | "deny" | "stop"
            )
        {
            return Some(
                "you're watching: /chat talks to the people here; ask the host to let you drive to ask the agent"
                    .into(),
            );
        }
        let person = person.to_string();
        let name = Some(name.to_string());
        let cmd = match m.get("t").and_then(Value::as_str)? {
            "say" => {
                let content = text("text");
                if content.trim().is_empty() {
                    return None;
                }
                AgentCommand::Say {
                    person,
                    content: content.chars().take(8000).collect(),
                    name,
                }
            }
            "chat" => AgentCommand::Chat {
                person,
                text: text("text"),
                name,
                image: m
                    .get("image")
                    .cloned()
                    .and_then(|i| serde_json::from_value(i).ok()),
            },
            "todo" => AgentCommand::TodoAdd {
                person,
                text: text("text"),
                name,
            },
            "todo_done" => AgentCommand::TodoDone {
                person,
                id: text("id"),
                name,
            },
            "approve" | "deny" | "stop" if !self.let_approve => {
                return Some("only the host can approve, deny or stop on this invite".into());
            }
            "approve" => AgentCommand::ApproveToolCall {
                call_id: text("call_id"),
            },
            "deny" => AgentCommand::DenyToolCall {
                call_id: text("call_id"),
            },
            "stop" => AgentCommand::Cancel,
            _ => return None,
        };
        ipc.send_command(&cmd)
            .await
            .err()
            .map(|e| format!("could not reach the session: {e}"))
    }
}

// ── Guest ────────────────────────────────────────────────────────────

/// What the guest's terminal shows.
pub enum GuestUpdate {
    /// The conversation before they joined (sent once, first).
    History(Vec<AgentEvent>),
    /// Someone's voice in the call.
    Audio {
        from: String,
        packet: Vec<u8>,
    },
    Event(Box<EventEnvelope>),
    Note(String),
}

/// Sends for a guest from anywhere; queued while reconnecting.
#[derive(Clone)]
pub struct GuestSender(mpsc::UnboundedSender<Value>);

impl GuestSender {
    fn send(&self, v: Value) -> bool {
        self.0.send(v).is_ok()
    }
    pub fn say(&self, text: &str) -> bool {
        self.send(json!({"t": "say", "text": text}))
    }
    pub fn chat(&self, text: &str) -> bool {
        self.send(json!({"t": "chat", "text": text}))
    }
    /// A picture to the people (shrunk already, see [`crate::chat_image`]).
    pub fn chat_image(&self, img: dcode_ai_common::event::ChatImage) -> bool {
        self.send(json!({"t": "chat", "text": "", "image": img}))
    }
    pub fn todo(&self, text: &str) -> bool {
        self.send(json!({"t": "todo", "text": text}))
    }
    pub fn todo_done(&self, id: &str) -> bool {
        self.send(json!({"t": "todo_done", "id": id}))
    }
    pub fn approve(&self, call_id: &str, yes: bool) -> bool {
        let t = if yes { "approve" } else { "deny" };
        self.send(json!({"t": t, "call_id": call_id}))
    }
    pub fn stop_turn(&self) -> bool {
        self.send(json!({"t": "stop"}))
    }
    pub fn call_join(&self) -> bool {
        self.send(json!({"t": "call_join"}))
    }
    pub fn call_leave(&self) -> bool {
        self.send(json!({"t": "call_leave"}))
    }
    pub fn audio(&self, packet: &[u8]) -> bool {
        use base64::Engine as _;
        self.send(
            json!({"t": "audio", "d": base64::engine::general_purpose::STANDARD.encode(packet)}),
        )
    }
    /// Leaving on purpose (no "reconnecting").
    pub fn bye(&self) -> bool {
        self.send(json!({"t": "bye"}))
    }
}

/// A guest, once let in. The connection is kept up in the background:
/// dropped, it reconnects with the rejoin token (no knock).
pub struct Guest {
    pub title: String,
    pub let_approve: bool,
    /// Let in to drive (ask the agent) — else to watch and chat.
    pub can_drive: bool,
    /// The host's name, their project, the session, its model, and this
    /// guest's person id.
    pub host: String,
    pub project: String,
    pub session_id: String,
    pub model: String,
    pub me: String,
    pub updates: mpsc::Receiver<GuestUpdate>,
    send: GuestSender,
}

/// One live connection.
struct Link {
    cipher: Cipher,
    read: LinkRead,
    write: LinkWrite,
    admitted: Value,
    /// A peer-to-peer link's connection, kept open as long as the link.
    _p2p: Option<iroh::endpoint::Connection>,
}

enum LinkError {
    /// Not reachable / no handshake: worth retrying.
    Unreachable(String),
    /// The host said no (or can't take them back): stop.
    Refused(String),
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LinkError::Unreachable(s) | LinkError::Refused(s) => f.write_str(s),
        }
    }
}

async fn open_link(
    invite: &Invite,
    hello: Value,
    on_wait: impl FnOnce(),
) -> Result<Link, LinkError> {
    // Try each of the host's addresses (LAN, Tailscale…), first answer wins.
    let hosts = invite.hosts();
    let per_host = if hosts.len() > 1 { 5 } else { 10 };
    let mut found: Option<(
        LinkRead,
        LinkWrite,
        String,
        Option<iroh::endpoint::Connection>,
    )> = None;
    let mut relay_err = None;
    // Direct addresses first (fast when they work), peer-to-peer last.
    let mut ordered: Vec<&String> = hosts.iter().filter(|h| !crate::p2p::is_id(h)).collect();
    ordered.extend(hosts.iter().filter(|h| crate::p2p::is_id(h)));
    for h in ordered {
        if crate::p2p::is_id(h) {
            match crate::p2p::dial(h).await {
                Ok((read, write, conn)) => {
                    found = Some((read, write, "a direct peer-to-peer link".into(), Some(conn)));
                    break;
                }
                Err(e) => {
                    tracing::debug!("p2p dial failed: {e}");
                    continue;
                }
            }
        }
        // `ROOM@relayhost`: through a relay.
        if let Some((room, relay_host)) = h.split_once('@') {
            let addr = format!("{relay_host}:{}", invite.port);
            match crate::relay::guest_connect(&addr, room).await {
                Ok(s) => {
                    let (r, w) = s.into_split();
                    found = Some((Box::new(r), Box::new(w), addr, None));
                    break;
                }
                Err(e) => {
                    relay_err = Some(e);
                    continue;
                }
            }
        }
        let addr = if h.contains(':') {
            format!("[{h}]:{}", invite.port)
        } else {
            format!("{h}:{}", invite.port)
        };
        if let Ok(Ok(s)) =
            tokio::time::timeout(Duration::from_secs(per_host), TcpStream::connect(&addr)).await
        {
            let (r, w) = s.into_split();
            found = Some((Box::new(r), Box::new(w), addr, None));
            break;
        }
    }
    if found.is_none()
        && let Some(e) = relay_err
    {
        return Err(LinkError::Refused(e));
    }
    let Some((mut read, mut write, addr, p2p)) = found else {
        let direct: Vec<String> = hosts
            .iter()
            .filter(|h| !crate::p2p::is_id(h))
            .cloned()
            .collect();
        let p2p_note = if direct.len() < hosts.len() {
            " The direct peer-to-peer link didn't connect either (the host may have closed the invite)."
        } else {
            ""
        };
        return Err(LinkError::Unreachable(format!(
            "no answer from {} on port {} — {}{p2p_note}",
            direct.join(" or "),
            invite.port,
            unreachable_hint(&direct)
        )));
    };
    let cipher = tokio::time::timeout(
        Duration::from_secs(10),
        handshake_guest(&mut read, &mut write, &invite.key),
    )
    .await
    .map_err(|_| LinkError::Unreachable(format!("{addr} didn't answer the invite")))?
    .map_err(|_| LinkError::Refused("the invite was refused (old or mistyped key?)".into()))?;
    cipher
        .send(&mut write, &hello)
        .await
        .map_err(|e| LinkError::Unreachable(e.to_string()))?;
    on_wait();
    let answer = cipher
        .recv(&mut read)
        .await
        .map_err(|_| LinkError::Unreachable("the host closed the invite".into()))?;
    if answer.get("t").and_then(Value::as_str) != Some("admitted") {
        return Err(LinkError::Refused(format!(
            "not let in: {}",
            answer
                .get("reason")
                .and_then(Value::as_str)
                .unwrap_or("the host said no")
        )));
    }
    Ok(Link {
        cipher,
        read,
        write,
        admitted: answer,
        _p2p: p2p,
    })
}

/// Reads one link until it ends; whether the host ended it for good.
async fn read_link(cipher: Cipher, mut read: LinkRead, tx: mpsc::Sender<GuestUpdate>) -> bool {
    while let Ok(m) = cipher.recv(&mut read).await {
        let fin = m.get("final").and_then(Value::as_bool).unwrap_or(false);
        let update = match m.get("t").and_then(Value::as_str) {
            Some("audio") => {
                use base64::Engine as _;
                let from = m
                    .get("from")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                match m
                    .get("d")
                    .and_then(Value::as_str)
                    .and_then(|d| base64::engine::general_purpose::STANDARD.decode(d).ok())
                {
                    Some(packet) => GuestUpdate::Audio { from, packet },
                    None => continue,
                }
            }
            Some("event") => match m
                .get("e")
                .cloned()
                .and_then(|e| serde_json::from_value::<EventEnvelope>(e).ok())
            {
                Some(env) => GuestUpdate::Event(Box::new(env)),
                None => continue,
            },
            Some("history") => GuestUpdate::History(
                m.get("events")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(|e| serde_json::from_value::<AgentEvent>(e.clone()).ok())
                            .collect()
                    })
                    .unwrap_or_default(),
            ),
            Some("note") => GuestUpdate::Note(
                m.get("text")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
            ),
            _ => continue,
        };
        if tx.send(update).await.is_err() {
            return true;
        }
        if fin {
            return true;
        }
    }
    false
}

/// How long and how often a dropped guest tries to come back.
const RECONNECT_EVERY: Duration = Duration::from_secs(2);
const RECONNECT_TRIES: usize = 60;

impl Guest {
    /// Connect, knock, and wait until the host decides (`on_wait` runs once
    /// the knock is delivered).
    pub async fn connect(
        invite: &Invite,
        name: &str,
        on_wait: impl FnOnce(),
    ) -> anyhow::Result<Self> {
        let link = open_link(invite, json!({"t": "hello", "name": name}), on_wait)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let field = |k: &str| {
            link.admitted
                .get(k)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let title = field("title");
        let host = field("host");
        let project = field("project");
        let session_id = field("session_id");
        let model = field("model");
        let me = field("you");
        let rejoin = field("rejoin");
        let let_approve = link
            .admitted
            .get("let_approve")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        // Hosts before 1.0.50 didn't say: they let everyone drive.
        let can_drive = link.admitted.get("role").and_then(Value::as_str) != Some("watch");
        let (tx, updates) = mpsc::channel(256);
        let (out_tx, out_rx) = mpsc::unbounded_channel::<Value>();
        let invite = invite.clone();
        let name = name.to_string();
        tokio::spawn(keep_up(link, invite, name, rejoin, out_rx, tx));
        Ok(Self {
            title,
            let_approve,
            can_drive,
            host,
            project,
            session_id,
            model,
            me,
            updates,
            send: GuestSender(out_tx),
        })
    }

    pub fn sender(&self) -> GuestSender {
        self.send.clone()
    }

    /// The sender and the updates, for a UI that sends from one place and
    /// draws from another.
    pub fn into_parts(self) -> (GuestSender, mpsc::Receiver<GuestUpdate>) {
        (self.send, self.updates)
    }
}

/// Owns the connection: sends what the guest queues, and reconnects with
/// the rejoin token when it drops.
async fn keep_up(
    mut link: Link,
    invite: Invite,
    name: String,
    rejoin: String,
    mut out: mpsc::UnboundedReceiver<Value>,
    tx: mpsc::Sender<GuestUpdate>,
) {
    // Whether we're in the call (to rejoin it after a reconnect).
    let mut in_call = false;
    loop {
        // `_p2p` held to the end of this round: dropping it closes the link.
        let Link {
            cipher,
            read,
            mut write,
            _p2p,
            ..
        } = link;
        let mut reader = tokio::spawn(read_link(cipher.clone(), read, tx.clone()));
        let for_good = loop {
            tokio::select! {
                m = out.recv() => {
                    let Some(m) = m else {
                        // The guest closed: say bye and stop.
                        let _ = cipher.send(&mut write, &json!({"t": "bye"})).await;
                        reader.abort();
                        return;
                    };
                    let kind = m.get("t").and_then(Value::as_str);
                    // Behind (weak Wi-Fi, or just reconnected): old audio is
                    // dropped, not sent late.
                    if kind == Some("audio") && out.len() > AUDIO_BACKLOG {
                        continue;
                    }
                    let leaving = kind == Some("bye");
                    match kind {
                        Some("call_join") => in_call = true,
                        Some("call_leave") => in_call = false,
                        _ => {}
                    }
                    if cipher.send(&mut write, &m).await.is_err() {
                        reader.abort();
                        break false;
                    }
                    if leaving {
                        reader.abort();
                        return;
                    }
                }
                done = &mut reader => break done.unwrap_or(false),
            }
        };
        if for_good || rejoin.is_empty() {
            return;
        }
        let _ = tx
            .send(GuestUpdate::Note("connection lost — reconnecting…".into()))
            .await;
        let mut back = None;
        for _ in 0..RECONNECT_TRIES {
            tokio::time::sleep(RECONNECT_EVERY).await;
            if tx.is_closed() {
                return;
            }
            let hello = json!({"t": "hello", "name": name, "rejoin": rejoin});
            match open_link(&invite, hello, || {}).await {
                Ok(l) => {
                    back = Some(l);
                    break;
                }
                Err(LinkError::Refused(why)) => {
                    let _ = tx.send(GuestUpdate::Note(why)).await;
                    return;
                }
                Err(LinkError::Unreachable(_)) => continue,
            }
        }
        match back {
            Some(mut l) => {
                let _ = tx.send(GuestUpdate::Note("reconnected".into())).await;
                if in_call {
                    let _ = l
                        .cipher
                        .send(&mut l.write, &json!({"t": "call_join"}))
                        .await;
                }
                link = l;
            }
            None => {
                let _ = tx
                    .send(GuestUpdate::Note(
                        "couldn't reconnect — the host may have stopped the invite".into(),
                    ))
                    .await;
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invite_strings_parse() {
        let i = Invite::parse("192.168.1.9:8788/abcdefghijkmnpqrstuvwxyz").unwrap();
        assert_eq!(i.host, "192.168.1.9");
        assert_eq!(i.port, 8788);
        assert_eq!(Invite::parse(&i.as_string()), Some(i.clone()));
        assert_eq!(
            Invite::parse("dcai://192.168.1.9:8788/abcdefghijkmnpqrstuvwxyz/"),
            Some(i)
        );
        assert!(Invite::parse("session-1793477206982552-0").is_none());
        // LAN and Tailscale in one line; join tries each.
        let two = Invite::parse("192.168.0.9,100.101.2.3:8788/abcdefghijkmnpqrstuvwxyz").unwrap();
        assert_eq!(two.hosts(), ["192.168.0.9", "100.101.2.3"]);
        assert_eq!(Invite::parse(&two.as_string()), Some(two));
        assert!(Invite::parse("1.2.3.4:88/short").is_none());
        assert!(Invite::parse("1.2.3.4/abcdefghijkmnpqrstuvwxyz").is_none());
        let six = Invite::parse("[fe80::1]:8788/abcdefghijkmnpqrstuvwxyz").unwrap();
        assert_eq!(six.host, "fe80::1");
        assert!(six.as_string().starts_with("[fe80::1]:8788/"));
        assert_eq!(new_key().len(), 24);
    }

    fn halves(s: TcpStream) -> (LinkRead, LinkWrite) {
        let (r, w) = s.into_split();
        (Box::new(r), Box::new(w))
    }

    #[tokio::test]
    async fn messages_round_trip_across_frames() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut m = [0u8; 5];
            s.read_exact(&mut m).await.unwrap();
            let (mut r, mut w) = halves(s);
            let c = handshake_host(&mut r, &mut w, "k3y-for-tests-only-123")
                .await
                .unwrap();
            let got = c.recv(&mut r).await.unwrap();
            c.send(&mut w, &got).await.unwrap();
        });
        let (mut r, mut w) = halves(TcpStream::connect(addr).await.unwrap());
        let c = handshake_guest(&mut r, &mut w, "k3y-for-tests-only-123")
            .await
            .unwrap();
        let big = json!({"t": "say", "text": "x".repeat(150_000)});
        c.send(&mut w, &big).await.unwrap();
        assert_eq!(c.recv(&mut r).await.unwrap(), big);
        server.await.unwrap();
    }

    #[tokio::test]
    async fn a_wrong_key_is_refused() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut s, _) = listener.accept().await.unwrap();
            let mut m = [0u8; 5];
            s.read_exact(&mut m).await.unwrap();
            let (mut r, mut w) = halves(s);
            handshake_host(&mut r, &mut w, "the-right-key-000000")
                .await
                .is_err()
        });
        let (mut r, mut w) = halves(TcpStream::connect(addr).await.unwrap());
        let _ = handshake_guest(&mut r, &mut w, "a-wrong-key-00000000").await;
        assert!(server.await.unwrap(), "host must refuse the wrong key");
    }

    /// A pretend session: an IPC server whose commands are recorded and
    /// whose events can be pushed.
    async fn fake_session() -> (
        PathBuf,
        mpsc::UnboundedReceiver<AgentCommand>,
        tokio::sync::broadcast::Sender<String>,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let id = format!("invite-test-{}", rand::random::<u32>());
        let server = dcode_ai_runtime::ipc::IpcServer::new(&id);
        let handle = server.start().await.unwrap();
        let socket = handle.socket_path().clone();
        let (events, commands) = handle.into_parts();
        (socket, commands, events, dir)
    }

    #[tokio::test]
    async fn a_guest_knocks_is_let_in_and_talks() {
        let (socket, mut commands, events, dir) = fake_session().await;
        std::fs::write(
            dir.path().join("x.events.jsonl"),
            "{\"id\":1,\"event\":{\"type\":\"MessageReceived\",\"role\":\"user\",\"content\":\"earlier\"}}\n",
        )
        .unwrap();
        let (knock_tx, mut knock_rx) = mpsc::channel::<Knock>(4);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = host_on(
            Some(listener),
            HostOptions {
                port,
                web: false,
                let_approve: false,
                socket,
                log_path: dir.path().join("x.events.jsonl"),
                title: "demo".into(),
                knocks: knock_tx,
                presence: None,
                host_name: "Dhanush".into(),
                project: "d-note".into(),
                session_id: "s-1".into(),
                model: "m".into(),
                auto_admit: false,
                hub: None,
            },
            port,
            None,
        )
        .await
        .unwrap();
        let invite = Invite {
            host: "127.0.0.1".into(),
            ..handle.invite.clone()
        };
        // The host says yes to Priya.
        tokio::spawn(async move {
            while let Some(k) = knock_rx.recv().await {
                let a = match k.name.as_str() {
                    "Priya" => Admit::Drive,
                    "Wendy" => Admit::Watch,
                    _ => Admit::No,
                };
                let _ = k.reply.send(a);
            }
        });
        let mut guest = Guest::connect(&invite, "Priya", || {}).await.unwrap();
        assert!(guest.can_drive);
        assert_eq!(guest.title, "demo");
        assert_eq!(guest.host, "Dhanush");
        assert!(guest.me.starts_with("guest-"));
        // The conversation so far comes first.
        let first = tokio::time::timeout(Duration::from_secs(5), guest.updates.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(first, GuestUpdate::History(ref h) if h.iter().any(|e| matches!(e,
            AgentEvent::MessageReceived { content, .. } if content == "earlier")))
        );
        let hello = tokio::time::timeout(Duration::from_secs(5), commands.recv())
            .await
            .unwrap()
            .unwrap();
        let AgentCommand::Hello { person, name } = hello else {
            panic!("expected hello, got {hello:?}");
        };
        assert_eq!(name, "Priya");
        assert_eq!(person, guest.me);

        // Session events reach the guest.
        tokio::time::sleep(Duration::from_millis(200)).await;
        let env = EventEnvelope::new(
            1,
            AgentEvent::MessageReceived {
                role: "assistant".into(),
                content: "hi Priya".into(),
            },
        );
        let _ = events.send(serde_json::to_string(&env).unwrap());
        let got = tokio::time::timeout(Duration::from_secs(5), guest.updates.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(got, GuestUpdate::Event(e) if matches!(&e.event,
            AgentEvent::MessageReceived { content, .. } if content == "hi Priya")));

        // What they say reaches the session as them.
        let send = guest.sender();
        assert!(send.say("try staging"));
        let said = tokio::time::timeout(Duration::from_secs(5), commands.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(said, AgentCommand::Say { person: ref p, ref content, .. }
            if *p == person && content == "try staging")
        );

        // Approvals stay with the host.
        assert!(send.approve("call-1", true));
        let note = tokio::time::timeout(Duration::from_secs(5), guest.updates.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(note, GuestUpdate::Note(t) if t.contains("only the host")));

        // Leaving says bye.
        drop(send);
        drop(guest);
        let bye = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match commands.recv().await {
                    Some(AgentCommand::Bye { person: p }) => break p,
                    Some(_) => continue,
                    None => panic!("closed"),
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(bye, person);

        // Let in to watch: chat reaches people, nothing reaches the agent.
        let mut wendy = Guest::connect(&invite, "Wendy", || {}).await.unwrap();
        assert!(!wendy.can_drive);
        let ws = wendy.sender();
        assert!(ws.say("rm -rf everything"));
        let note = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match wendy.updates.recv().await {
                    Some(GuestUpdate::Note(t)) => break t,
                    Some(_) => continue,
                    None => panic!("closed"),
                }
            }
        })
        .await
        .unwrap();
        assert!(note.contains("you're watching"), "{note}");
        assert!(ws.chat("hello all"));
        let got = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match commands.recv().await {
                    Some(AgentCommand::Say { .. }) => panic!("a watcher reached the agent"),
                    Some(AgentCommand::Chat { text, .. }) => break text,
                    Some(_) => continue,
                    None => panic!("closed"),
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(got, "hello all");
        // A picture travels the same way (watchers may send them too).
        let pic = dcode_ai_common::event::ChatImage {
            name: "bug.png".into(),
            mime: "image/png".into(),
            w: 2,
            h: 2,
            data: "iVBORw0KGgo=".into(),
        };
        assert!(ws.chat_image(pic.clone()));
        let got = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match commands.recv().await {
                    Some(AgentCommand::Chat { image, .. }) => break image,
                    Some(_) => continue,
                    None => panic!("closed"),
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(got, Some(pic));
        drop(ws);
        drop(wendy);

        // Someone the host turns down doesn't get in.
        let refused = Guest::connect(&invite, "Mallory", || {}).await;
        assert!(refused.err().unwrap().to_string().contains("not let in"));
        // A wrong key doesn't even get to knock.
        let wrong = Invite {
            key: new_key(),
            ..invite.clone()
        };
        assert!(Guest::connect(&wrong, "Eve", || {}).await.is_err());
        drop(handle);
    }

    #[tokio::test]
    async fn a_guest_joins_over_a_direct_peer_to_peer_link() {
        let _ = crate::p2p::TEST_LOOKUP.set(iroh::address_lookup::MemoryLookup::new());
        let (socket, mut commands, _events, dir) = fake_session().await;
        let (knock_tx, mut knock_rx) = mpsc::channel::<Knock>(4);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut handle = host_on(
            Some(listener),
            HostOptions {
                port,
                web: false,
                let_approve: false,
                socket,
                log_path: dir.path().join("x.events.jsonl"),
                title: "demo".into(),
                knocks: knock_tx,
                presence: None,
                host_name: "Dhanush".into(),
                project: "d-note".into(),
                session_id: "s-1".into(),
                model: "m".into(),
                auto_admit: false,
                hub: None,
            },
            port,
            None,
        )
        .await
        .unwrap();
        attach_p2p(&mut handle).await;
        assert!(handle.p2p);
        // Only the peer-to-peer id: no address on this network works.
        let id = handle
            .invite
            .hosts()
            .into_iter()
            .find(|h| crate::p2p::is_id(h))
            .unwrap();
        let invite = Invite {
            host: id,
            ..handle.invite.clone()
        };
        assert!(Invite::parse(&invite.as_string()).is_some());
        tokio::spawn(async move {
            while let Some(k) = knock_rx.recv().await {
                assert_eq!(k.from, "a direct link");
                let _ = k.reply.send(Admit::Drive);
            }
        });
        let guest = Guest::connect(&invite, "Arun", || {}).await.unwrap();
        assert_eq!(guest.title, "demo");
        assert!(guest.sender().say("hello from far away"));
        let said = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match commands.recv().await {
                    Some(AgentCommand::Say { content, .. }) => break content,
                    Some(_) => continue,
                    None => panic!("closed"),
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(said, "hello from far away");
        drop(guest);
        drop(handle);
    }

    /// Forwards to `target`; `cut()` drops every connection so far (a
    /// Wi-Fi blip) while still accepting new ones.
    struct Proxy {
        port: u16,
        live: Arc<Mutex<Vec<tokio::task::AbortHandle>>>,
    }

    impl Proxy {
        async fn start(target: u16) -> Self {
            let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = l.local_addr().unwrap().port();
            let live: Arc<Mutex<Vec<tokio::task::AbortHandle>>> = Default::default();
            let tasks = live.clone();
            tokio::spawn(async move {
                while let Ok((mut a, _)) = l.accept().await {
                    let t = tokio::spawn(async move {
                        if let Ok(mut b) = TcpStream::connect(("127.0.0.1", target)).await {
                            let _ = tokio::io::copy_bidirectional(&mut a, &mut b).await;
                        }
                    });
                    tasks.lock().unwrap().push(t.abort_handle());
                }
            });
            Self { port, live }
        }
        fn cut(&self) {
            for t in self.live.lock().unwrap().drain(..) {
                t.abort();
            }
        }
    }

    #[tokio::test]
    async fn a_dropped_guest_comes_back_without_knocking_and_can_be_kicked() {
        let (socket, mut commands, _events, dir) = fake_session().await;
        let (knock_tx, mut knock_rx) = mpsc::channel::<Knock>(4);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = host_on(
            Some(listener),
            HostOptions {
                port,
                web: false,
                let_approve: false,
                socket,
                log_path: dir.path().join("none.events.jsonl"),
                title: "demo".into(),
                knocks: knock_tx,
                presence: None,
                host_name: "Dhanush".into(),
                project: "d-note".into(),
                session_id: "s-1".into(),
                model: "m".into(),
                auto_admit: false,
                hub: None,
            },
            port,
            None,
        )
        .await
        .unwrap();
        let knocks = Arc::new(AtomicUsize::new(0));
        let counted = knocks.clone();
        tokio::spawn(async move {
            while let Some(k) = knock_rx.recv().await {
                counted.fetch_add(1, Ordering::SeqCst);
                let _ = k.reply.send(Admit::Drive);
            }
        });
        let proxy = Proxy::start(port).await;
        let invite = Invite {
            host: "127.0.0.1".into(),
            port: proxy.port,
            key: handle.invite.key.clone(),
        };
        let mut guest = Guest::connect(&invite, "Priya", || {}).await.unwrap();
        assert_eq!(guest.project, "d-note");
        let person = guest.me.clone();
        let next_cmd = async |rx: &mut mpsc::UnboundedReceiver<AgentCommand>| {
            tokio::time::timeout(Duration::from_secs(10), rx.recv())
                .await
                .expect("a command")
                .expect("open")
        };
        assert!(matches!(
            next_cmd(&mut commands).await,
            AgentCommand::Hello { .. }
        ));

        // Chat and to-dos pass through as them.
        let send = guest.sender();
        send.chat("lunch?");
        send.todo("fix login");
        assert!(matches!(next_cmd(&mut commands).await,
            AgentCommand::Chat { person: ref p, ref text, .. } if *p == person && text == "lunch?"));
        assert!(matches!(next_cmd(&mut commands).await,
            AgentCommand::TodoAdd { ref text, .. } if text == "fix login"));

        // A blip: the guest comes back by itself, no knock, same person,
        // and the session never hears that they left.
        proxy.cut();
        let note = async |g: &mut Guest, want: &str| {
            tokio::time::timeout(Duration::from_secs(15), async {
                loop {
                    match g.updates.recv().await {
                        Some(GuestUpdate::Note(t)) if t.contains(want) => break,
                        Some(_) => continue,
                        None => panic!("updates closed before {want:?}"),
                    }
                }
            })
            .await
            .unwrap_or_else(|_| panic!("no note {want:?}"));
        };
        note(&mut guest, "reconnecting").await;
        note(&mut guest, "reconnected").await;
        assert_eq!(knocks.load(Ordering::SeqCst), 1, "no second knock");
        send.chat("back");
        assert!(
            matches!(next_cmd(&mut commands).await,
            AgentCommand::Chat { person: ref p, ref text, .. } if *p == person && text == "back"),
            "no Bye or second Hello in between, same person"
        );

        // Kick: told, removed, can't come back.
        assert_eq!(handle.guests().len(), 1);
        assert_eq!(handle.kick("priya"), 1);
        note(&mut guest, "removed you").await;
        assert!(
            matches!(next_cmd(&mut commands).await, AgentCommand::Bye { person: ref p } if *p == person)
        );
        assert!(handle.host.rejoin.lock().unwrap().is_empty());
        drop(handle);
    }

    #[tokio::test]
    async fn the_host_forwards_voice_between_people_in_the_call() {
        // SAFETY: tests that touch this set the same value.
        unsafe { std::env::set_var("DCODE_AI_FAKE_AUDIO", "tone") };
        let (socket, _commands, _events, dir) = fake_session().await;
        let (knock_tx, mut knock_rx) = mpsc::channel::<Knock>(4);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = host_on(
            Some(listener),
            HostOptions {
                port,
                web: false,
                let_approve: false,
                socket,
                log_path: dir.path().join("none.events.jsonl"),
                title: "demo".into(),
                knocks: knock_tx,
                presence: None,
                host_name: "Dhanush".into(),
                project: "d-note".into(),
                session_id: "s-1".into(),
                model: "m".into(),
                auto_admit: false,
                hub: None,
            },
            port,
            None,
        )
        .await
        .unwrap();
        tokio::spawn(async move {
            while let Some(k) = knock_rx.recv().await {
                let _ = k.reply.send(Admit::Drive);
            }
        });
        let invite = Invite {
            host: "127.0.0.1".into(),
            ..handle.invite.clone()
        };
        let mut guest = Guest::connect(&invite, "Priya", || {}).await.unwrap();
        let send = guest.sender();
        send.call_join();
        // The host joins with a (fake) microphone playing a tone.
        let (host_call, mic) = crate::voice::Call::start(HOST_PERSON).unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(handle.in_call(), vec![guest.me.clone()]);
        handle.call_join(host_call.clone(), mic);
        // The host's voice reaches the guest.
        let heard = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                match guest.updates.recv().await {
                    Some(GuestUpdate::Audio { from, packet }) => break (from, packet),
                    Some(_) => continue,
                    None => panic!("closed"),
                }
            }
        })
        .await
        .expect("audio from the host");
        assert_eq!(heard.0, HOST_PERSON);
        assert_eq!(heard.1.len(), 3 + crate::voice::FRAME / 2);
        // The guest's voice reaches the host.
        for _ in 0..3 {
            send.audio(&heard.1);
        }
        tokio::time::timeout(Duration::from_secs(5), async {
            while !host_call.speaking(&guest.me) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the host hears the guest");
        // Leaving the call stops it.
        send.call_leave();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(handle.in_call().is_empty());
        handle.call_leave();
        drop(handle);
    }

    #[tokio::test]
    async fn a_guest_joins_through_a_relay() {
        let relay = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay_port = relay.local_addr().unwrap().port();
        tokio::spawn(crate::relay::serve_on(relay));
        let (socket, mut commands, _events, dir) = fake_session().await;
        let (knock_tx, mut knock_rx) = mpsc::channel::<Knock>(4);
        tokio::spawn(async move {
            while let Some(k) = knock_rx.recv().await {
                let _ = k.reply.send(Admit::Drive);
            }
        });
        let handle = host_relay(
            HostOptions {
                port: 0,
                web: false,
                let_approve: false,
                socket,
                log_path: dir.path().join("none.events.jsonl"),
                title: "demo".into(),
                knocks: knock_tx,
                presence: None,
                host_name: "Dhanush".into(),
                project: "d-note".into(),
                session_id: "s-1".into(),
                model: "m".into(),
                auto_admit: false,
                hub: None,
            },
            &format!("127.0.0.1:{relay_port}"),
        )
        .await
        .unwrap();
        // The line names the relay, not this machine.
        let line = handle.invite.as_string();
        assert!(
            line.contains(&format!("@127.0.0.1:{relay_port}/")),
            "{line}"
        );
        // Give the host a moment to register with the relay.
        tokio::time::sleep(Duration::from_millis(300)).await;
        let invite = Invite::parse(&line).unwrap();
        let guest = Guest::connect(&invite, "Priya", || {}).await.unwrap();
        assert_eq!(guest.project, "d-note");
        let next = async |rx: &mut mpsc::UnboundedReceiver<AgentCommand>| {
            tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .unwrap()
                .unwrap()
        };
        assert!(matches!(
            next(&mut commands).await,
            AgentCommand::Hello { .. }
        ));
        guest.sender().say("via the relay");
        assert!(matches!(next(&mut commands).await,
            AgentCommand::Say { ref content, .. } if content == "via the relay"));
        // A room the relay doesn't know is refused, with a reason.
        let wrong = Invite {
            host: format!("{}@127.0.0.1", new_key()),
            ..invite.clone()
        };
        let err = Guest::connect(&wrong, "Eve", || {})
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("relay said"), "{err}");
        drop(handle);
    }
}
