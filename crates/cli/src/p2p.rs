//! Direct peer-to-peer invites over [iroh]: QUIC dialed by the host's public
//! key, with UDP hole-punching, so a guest on another network joins without
//! a VPN or a relay you run. When the two networks can't be punched through,
//! iroh carries the (encrypted) packets over its public relays instead.
//!
//! Security: the QUIC connection is authenticated with the host's key (the
//! id in the invite), and the usual `DCAI1` + Noise `NNpsk0` handshake with
//! the invite's key runs inside it, so iroh's relays and address lookup
//! only ever see ciphertext — and never the invite key.

use std::time::Duration;

use iroh::endpoint::{Connection, presets};
use iroh::{Endpoint, EndpointId};

use crate::invite::{LinkRead, LinkWrite};

/// The protocol name both sides agree on in the QUIC handshake.
pub const ALPN: &[u8] = b"dcode-ai/invite/1";
/// How an invite marks the host's peer-to-peer id among its addresses.
pub const PREFIX: &str = "p2p-";
/// How long a guest waits for the direct link (lookup + hole-punching).
const DIAL_TIMEOUT: Duration = Duration::from_secs(25);

/// Whether an invite address is a peer-to-peer id (`p2p-<id>`).
pub fn is_id(host: &str) -> bool {
    host.strip_prefix(PREFIX)
        .is_some_and(|id| parse_id(id).is_some())
}

/// Off with `DCODE_AI_P2P=0` (or `/invite lan`).
pub fn enabled() -> bool {
    !std::env::var("DCODE_AI_P2P").is_ok_and(|v| matches!(v.as_str(), "0" | "false" | "off"))
}

/// The id as it goes in an invite: base32, lowercase (52 characters).
pub fn encode_id(id: &EndpointId) -> String {
    format!("{PREFIX}{}", base32_lower(id.as_bytes()))
}

fn parse_id(s: &str) -> Option<EndpointId> {
    (s.len() == 52 && s.chars().all(|c| c.is_ascii_alphanumeric()))
        .then(|| s.parse().ok())
        .flatten()
}

fn base32_lower(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let (mut buf, mut bits) = (0u32, 0u32);
    for &b in bytes {
        buf = (buf << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buf >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

fn io_err(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

/// Tests point both sides at an in-memory address book on localhost, so
/// nothing reaches the public network.
#[cfg(test)]
pub(crate) static TEST_LOOKUP: std::sync::OnceLock<iroh::address_lookup::MemoryLookup> =
    std::sync::OnceLock::new();

async fn bind(alpns: Vec<Vec<u8>>) -> std::io::Result<Endpoint> {
    #[cfg(test)]
    if let Some(lookup) = TEST_LOOKUP.get() {
        return Endpoint::builder(presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .address_lookup(lookup.clone())
            .bind_addr("127.0.0.1:0")
            .map_err(io_err)?
            .alpns(alpns)
            .bind()
            .await
            .map_err(io_err);
    }
    Endpoint::builder(presets::N0)
        .alpns(alpns)
        .bind()
        .await
        .map_err(io_err)
}

/// The host's endpoint for one invite: a fresh key each time, so an old
/// invite's id leads nowhere.
pub async fn listen() -> std::io::Result<Endpoint> {
    let ep = bind(vec![ALPN.to_vec()]).await?;
    #[cfg(test)]
    if let Some(lookup) = TEST_LOOKUP.get() {
        lookup.add_endpoint_info(ep.addr());
    }
    Ok(ep)
}

/// Waits for the next guest's stream: `(read, write, connection)`. The
/// connection must be kept as long as the streams are used.
pub async fn accept(ep: &Endpoint) -> Option<std::io::Result<(LinkRead, LinkWrite, Connection)>> {
    let incoming = ep.accept().await?;
    Some(
        async {
            let conn = incoming.await.map_err(io_err)?;
            let (send, recv) = tokio::time::timeout(Duration::from_secs(10), conn.accept_bi())
                .await
                .map_err(io_err)?
                .map_err(io_err)?;
            Ok((
                Box::new(recv) as LinkRead,
                Box::new(send) as LinkWrite,
                conn,
            ))
        }
        .await,
    )
}

/// One endpoint for every dial from this process (reconnects reuse it).
static CLIENT: tokio::sync::OnceCell<Endpoint> = tokio::sync::OnceCell::const_new();

/// Dials `p2p-<id>` and opens the stream the invite runs on.
pub async fn dial(host: &str) -> std::io::Result<(LinkRead, LinkWrite, Connection)> {
    let id = host
        .strip_prefix(PREFIX)
        .and_then(parse_id)
        .ok_or_else(|| io_err("not a peer-to-peer id"))?;
    let ep = CLIENT.get_or_try_init(|| bind(Vec::new())).await?;
    let conn = tokio::time::timeout(DIAL_TIMEOUT, ep.connect(id, ALPN))
        .await
        .map_err(|_| io_err("the direct link timed out"))?
        .map_err(io_err)?;
    let (send, recv) = conn.open_bi().await.map_err(io_err)?;
    Ok((Box::new(recv), Box::new(send), conn))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_through_an_invite() {
        let id = iroh::SecretKey::generate().public();
        let s = encode_id(&id);
        assert!(s.starts_with(PREFIX) && s.len() == PREFIX.len() + 52, "{s}");
        assert!(is_id(&s));
        assert_eq!(parse_id(&s[PREFIX.len()..]), Some(id));
        assert!(!is_id("192.168.0.9"));
        assert!(!is_id("p2p-short"));
    }
}

/// Over the real network (n0's address lookup and relays): run by hand with
/// `cargo test -p dcode-ai-cli --lib p2p -- --ignored`.
#[cfg(test)]
mod public_tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    #[ignore = "uses the public network"]
    async fn dials_by_id_over_the_public_network() {
        let ep = listen().await.unwrap();
        tokio::time::timeout(Duration::from_secs(10), ep.online())
            .await
            .unwrap();
        let id = encode_id(&ep.id());
        let server = tokio::spawn(async move {
            let (mut r, mut w, conn) = accept(&ep).await.unwrap().unwrap();
            let mut b = [0u8; 5];
            r.read_exact(&mut b).await.unwrap();
            w.write_all(&b).await.unwrap();
            w.flush().await.unwrap();
            conn.closed().await;
        });
        let started = std::time::Instant::now();
        let (mut r, mut w, conn) = dial(&id).await.unwrap();
        w.write_all(b"DCAI1").await.unwrap();
        w.flush().await.unwrap();
        let mut b = [0u8; 5];
        r.read_exact(&mut b).await.unwrap();
        assert_eq!(&b, b"DCAI1");
        eprintln!("round trip after {:?}", started.elapsed());
        conn.close(0u32.into(), b"bye");
        let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
    }
}
