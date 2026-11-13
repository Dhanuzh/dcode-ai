//! Sessions that outlive the terminal.
//!
//! An interactive `dcode-ai` runs inside a small background *host* that owns
//! a pseudo-terminal; your terminal only shows it (a thin client relaying
//! keys and screen). Close the terminal and the host — the session, a turn in
//! progress, background agents — keeps going; run `dcode-ai` in the folder
//! again (or `dcode-ai bg attach`) and you're back where you were. `/detach`
//! leaves on purpose.
//!
//! - The host listens on a socket in this user's private runtime folder and
//!   only accepts this user's connections.
//! - One terminal at a time: attaching from another one moves the session
//!   there.
//! - While nobody is attached the session sees the terminal as unfocused, so
//!   approvals notify you (desktop / `[ui] push_url`).
//! - Unix (Linux, macOS, WSL) for now.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// What the session prints to leave (`/detach`): the host drops the
/// terminal instead of showing it.
pub const DETACH_MARKER: &[u8] = b"\x1b]7799;dcode-ai-detach\x07";

/// Set inside a hosted session.
pub const HOSTED_ENV: &str = "DCODE_AI_HOSTED";

// Frame kinds.
const INPUT: u8 = 0;
const RESIZE: u8 = 1;
const OUTPUT: u8 = 0;
const DETACHED: u8 = 2;
const EXITED: u8 = 3;
const MOVED: u8 = 4;

/// A frame: kind, then a 4-byte big-endian length, then the bytes.
pub fn frame(kind: u8, data: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(5 + data.len());
    f.push(kind);
    f.extend_from_slice(&(data.len() as u32).to_be_bytes());
    f.extend_from_slice(data);
    f
}

/// Reads one frame (`None` at the end of the stream).
pub fn read_frame(r: &mut impl std::io::Read) -> std::io::Result<Option<(u8, Vec<u8>)>> {
    let mut head = [0u8; 5];
    match r.read_exact(&mut head) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > 8 * 1024 * 1024 {
        return Err(std::io::Error::other("frame too large"));
    }
    let mut data = vec![0u8; len];
    r.read_exact(&mut data)?;
    Ok(Some((head[0], data)))
}

/// A running host, as recorded next to its socket.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HostInfo {
    pub id: String,
    pub pid: u32,
    pub cwd: PathBuf,
    pub started: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    pub args: Vec<String>,
}

pub fn hosts_dir() -> PathBuf {
    dcode_ai_runtime::ipc::runtime_ipc_dir().join("hosts")
}

fn socket_of(id: &str) -> PathBuf {
    hosts_dir().join(format!("{id}.sock"))
}

fn info_of(id: &str) -> PathBuf {
    hosts_dir().join(format!("{id}.json"))
}

#[cfg(unix)]
fn alive(pid: u32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn alive(_pid: u32) -> bool {
    false
}

/// Hosts still running (stale records are removed), newest first.
pub fn list() -> Vec<HostInfo> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(hosts_dir()) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Ok(info) = std::fs::read_to_string(&p)
            .map_err(|_| ())
            .and_then(|s| serde_json::from_str::<HostInfo>(&s).map_err(|_| ()))
        else {
            continue;
        };
        if alive(info.pid) && socket_of(&info.id).exists() {
            out.push(info);
        } else {
            let _ = std::fs::remove_file(&p);
            let _ = std::fs::remove_file(socket_of(&info.id));
        }
    }
    out.sort_by_key(|h| std::cmp::Reverse(h.started));
    out
}

/// Hosts running in `cwd`.
pub fn here(cwd: &Path) -> Vec<HostInfo> {
    let cwd = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    list().into_iter().filter(|h| h.cwd == cwd).collect()
}

/// Stops a host's session.
pub fn kill(id: &str) -> bool {
    #[cfg(unix)]
    if let Some(h) = list().into_iter().find(|h| h.id == id) {
        // SAFETY: plain signal to a process we recorded.
        unsafe {
            libc::kill(h.pid as libc::pid_t, libc::SIGTERM);
        }
        return true;
    }
    let _ = id;
    false
}

// ── Host ───────────────────────────────────────────────────────────────

#[cfg(unix)]
mod unix {
    use super::*;
    use portable_pty::{CommandBuilder, PtySize, native_pty_system};
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::sync::{Arc, Mutex};

    type Client = Arc<Mutex<Option<UnixStream>>>;

    fn send(client: &Client, kind: u8, data: &[u8]) {
        let mut g = client.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(s) = g.as_mut()
            && s.write_all(&frame(kind, data)).is_err()
        {
            *g = None;
        }
    }

    /// Runs `program args…` in a pseudo-terminal and serves it on the host
    /// socket until it exits. Returns its exit code.
    pub fn run(
        id: &str,
        program: &Path,
        args: &[String],
        cwd: &Path,
        size: (u16, u16),
    ) -> anyhow::Result<i32> {
        let dir = hosts_dir();
        dcode_ai_runtime::ipc::ensure_private_dir(&dir)?;
        let sock = socket_of(id);
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock)?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600))?;
        }
        let pty = native_pty_system().openpty(PtySize {
            rows: size.1.max(5),
            cols: size.0.max(20),
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut cmd = CommandBuilder::new(program);
        cmd.args(args);
        cmd.cwd(cwd);
        cmd.env(HOSTED_ENV, id);
        let mut child = pty.slave.spawn_command(cmd)?;
        drop(pty.slave);
        let info = HostInfo {
            id: id.to_string(),
            pid: std::process::id(),
            cwd: cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf()),
            started: chrono::Utc::now(),
            args: args.to_vec(),
        };
        std::fs::write(info_of(id), serde_json::to_string(&info)?)?;

        let master = Arc::new(Mutex::new(pty.master));
        let writer = Arc::new(Mutex::new(
            master
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take_writer()?,
        ));
        let mut reader = master
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .try_clone_reader()?;
        let client: Client = Arc::new(Mutex::new(None));
        let size_now = Arc::new(Mutex::new(size));
        // Whether the program asked for focus reports (ESC[?1004h).
        let wants_focus = Arc::new(std::sync::atomic::AtomicBool::new(false));
        // The screen as it is now, to paint a terminal that attaches; and the
        // keyboard mode the program pushed (kitty `ESC[>…u`), to restore.
        let screen = Arc::new(Mutex::new(vt100::Parser::new(
            size.1.max(5),
            size.0.max(20),
            0,
        )));
        let key_mode: Arc<Mutex<Option<Vec<u8>>>> = Arc::new(Mutex::new(None));

        // Screen → the attached terminal (dropped while nobody is attached;
        // a new terminal gets a full repaint).
        {
            let (client, writer, wants_focus, screen, key_mode) = (
                client.clone(),
                writer.clone(),
                wants_focus.clone(),
                screen.clone(),
                key_mode.clone(),
            );
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 16 * 1024];
                loop {
                    let n = match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    let chunk = &buf[..n];
                    if find(chunk, b"\x1b[?1004h").is_some() {
                        wants_focus.store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                    if find(chunk, b"\x1b[?1004l").is_some() {
                        wants_focus.store(false, std::sync::atomic::Ordering::SeqCst);
                    }
                    track_key_mode(chunk, &key_mode);
                    screen
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .process(chunk);
                    if let Some(at) = find(chunk, DETACH_MARKER) {
                        let mut rest = chunk[..at].to_vec();
                        rest.extend_from_slice(&chunk[at + DETACH_MARKER.len()..]);
                        send(&client, OUTPUT, &rest);
                        send(&client, DETACHED, b"");
                        *client.lock().unwrap_or_else(|e| e.into_inner()) = None;
                        focus(&writer, &wants_focus, false);
                    } else {
                        send(&client, OUTPUT, chunk);
                    }
                }
            });
        }

        // Terminals attaching.
        {
            let (client, writer, master, size_now, wants_focus) = (
                client.clone(),
                writer.clone(),
                master.clone(),
                size_now.clone(),
                wants_focus.clone(),
            );
            let (screen, key_mode) = (screen.clone(), key_mode.clone());
            let me = dcode_ai_runtime::ipc::current_uid();
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    if !peer_is(&stream, me) {
                        continue;
                    }
                    // One terminal at a time: the newest wins.
                    if let Some(mut old) = client.lock().unwrap_or_else(|e| e.into_inner()).take() {
                        let _ = old.write_all(&frame(MOVED, b""));
                    }
                    let Ok(mut reading) = stream.try_clone() else {
                        continue;
                    };
                    // Paint the screen as it is, then follow it.
                    {
                        let mut stream = stream;
                        let paint = repaint(
                            &screen.lock().unwrap_or_else(|e| e.into_inner()),
                            wants_focus.load(std::sync::atomic::Ordering::SeqCst),
                            key_mode
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .as_deref(),
                        );
                        let mut g = client.lock().unwrap_or_else(|e| e.into_inner());
                        if stream.write_all(&frame(OUTPUT, &paint)).is_ok() {
                            *g = Some(stream);
                        }
                    }
                    focus(&writer, &wants_focus, true);
                    let (client, writer, master, size_now, wants_focus, screen) = (
                        client.clone(),
                        writer.clone(),
                        master.clone(),
                        size_now.clone(),
                        wants_focus.clone(),
                        screen.clone(),
                    );
                    std::thread::spawn(move || {
                        let mut first_resize = true;
                        while let Ok(Some((kind, data))) = read_frame(&mut reading) {
                            match kind {
                                INPUT => {
                                    let _ = writer
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .write_all(&data);
                                }
                                RESIZE if data.len() == 4 => {
                                    let cols = u16::from_be_bytes([data[0], data[1]]).max(20);
                                    let rows = u16::from_be_bytes([data[2], data[3]]).max(5);
                                    let resize = |c: u16, r: u16| {
                                        let _ = master
                                            .lock()
                                            .unwrap_or_else(|e| e.into_inner())
                                            .resize(PtySize {
                                                rows: r,
                                                cols: c,
                                                pixel_width: 0,
                                                pixel_height: 0,
                                            });
                                    };
                                    // A newly attached terminal of another width: the
                                    // session redraws for it.
                                    let changed =
                                        *size_now.lock().unwrap_or_else(|e| e.into_inner())
                                            != (cols, rows);
                                    if first_resize || changed {
                                        first_resize = false;
                                        screen
                                            .lock()
                                            .unwrap_or_else(|e| e.into_inner())
                                            .set_size(rows, cols);
                                    }
                                    resize(cols, rows);
                                    *size_now.lock().unwrap_or_else(|e| e.into_inner()) =
                                        (cols, rows);
                                }
                                _ => {}
                            }
                        }
                        // That terminal went away (closed, or moved on):
                        // forget it if it's still the attached one.
                        let mut g = client.lock().unwrap_or_else(|e| e.into_inner());
                        if streams_equal(g.as_ref(), &reading) {
                            *g = None;
                            drop(g);
                            focus(&writer, &wants_focus, false);
                        }
                    });
                }
            });
        }

        let status = child.wait()?;
        let code = status.exit_code() as i32;
        send(&client, EXITED, &code.to_be_bytes());
        let _ = std::fs::remove_file(&sock);
        let _ = std::fs::remove_file(info_of(id));
        Ok(code)
    }

    /// Escape codes that paint `screen` on a fresh terminal: the right
    /// buffer, its contents and cursor, input modes, focus reports, keys.
    pub(super) fn repaint(parser: &vt100::Parser, focus: bool, key_mode: Option<&[u8]>) -> Vec<u8> {
        let s = parser.screen();
        let mut out = Vec::new();
        if s.alternate_screen() {
            out.extend_from_slice(b"\x1b[?1049h");
        }
        out.extend_from_slice(b"\x1b[H\x1b[2J");
        out.extend_from_slice(&s.state_formatted());
        if focus {
            out.extend_from_slice(b"\x1b[?1004h");
        }
        if let Some(k) = key_mode {
            out.extend_from_slice(k);
        }
        out
    }

    /// Remembers the last kitty keyboard mode pushed (`ESC[>Nu`) or popped.
    fn track_key_mode(chunk: &[u8], mode: &Mutex<Option<Vec<u8>>>) {
        let mut i = 0;
        while let Some(at) = find(&chunk[i..], b"\x1b[") {
            let start = i + at;
            let rest = &chunk[start + 2..];
            if rest.first() == Some(&b'>') {
                let digits = rest[1..].iter().take_while(|b| b.is_ascii_digit()).count();
                if rest.get(1 + digits) == Some(&b'u') {
                    *mode.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(chunk[start..start + 2 + 1 + digits + 1].to_vec());
                }
            } else if rest.first() == Some(&b'<') && rest.get(1) == Some(&b'u') {
                *mode.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
            i = start + 2;
        }
    }

    fn streams_equal(a: Option<&UnixStream>, b: &UnixStream) -> bool {
        use std::os::fd::AsRawFd;
        let Some(a) = a else { return false };
        // Same socket: same inode behind both descriptors.
        let ino = |fd: i32| -> Option<(u64, u64)> {
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            // SAFETY: fstat on a descriptor we own, into a zeroed struct.
            (unsafe { libc::fstat(fd, &mut st) } == 0)
                .then_some((st.st_dev as u64, st.st_ino as u64))
        };
        ino(a.as_raw_fd()).is_some() && ino(a.as_raw_fd()) == ino(b.as_raw_fd())
    }

    /// Tells the session the terminal gained or lost focus (it reports
    /// focus changes, and only notifies you when you're away).
    fn focus(
        writer: &Arc<Mutex<Box<dyn Write + Send>>>,
        wants: &std::sync::atomic::AtomicBool,
        on: bool,
    ) {
        if !wants.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let _ = writer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .write_all(if on { b"\x1b[I" } else { b"\x1b[O" });
    }

    fn peer_is(stream: &UnixStream, uid: u32) -> bool {
        use std::os::fd::AsRawFd;
        #[cfg(target_os = "linux")]
        {
            let mut cred: libc::ucred = unsafe { std::mem::zeroed() };
            let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
            // SAFETY: getsockopt into a correctly sized ucred.
            let ok = unsafe {
                libc::getsockopt(
                    stream.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_PEERCRED,
                    &mut cred as *mut _ as *mut libc::c_void,
                    &mut len,
                )
            } == 0;
            ok && cred.uid == uid
        }
        #[cfg(not(target_os = "linux"))]
        {
            let mut euid: libc::uid_t = 0;
            let mut egid: libc::gid_t = 0;
            // SAFETY: getpeereid writes two ids.
            let ok = unsafe { libc::getpeereid(stream.as_raw_fd(), &mut euid, &mut egid) } == 0;
            ok && euid == uid
        }
    }

    pub(super) fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
        hay.windows(needle.len()).position(|w| w == needle)
    }

    /// What ended an attachment.
    #[derive(Debug, PartialEq)]
    pub enum Ended {
        Detached,
        Exited(i32),
        Moved,
        Lost,
    }

    /// Shows the host's session in this terminal until it ends or detaches.
    pub fn attach(id: &str) -> anyhow::Result<Ended> {
        use crossterm::terminal;
        let sock = socket_of(id);
        let stream = UnixStream::connect(&sock)?;
        let mut out = stream.try_clone()?;
        let mut from = stream.try_clone()?;
        terminal::enable_raw_mode()?;
        let (c, r) = terminal::size().unwrap_or((100, 30));
        let resize = |s: &mut UnixStream, c: u16, r: u16| {
            let mut d = c.to_be_bytes().to_vec();
            d.extend_from_slice(&r.to_be_bytes());
            let _ = s.write_all(&frame(RESIZE, &d));
        };
        resize(&mut out, c, r);
        // Keys → session.
        {
            let mut out = out.try_clone()?;
            std::thread::spawn(move || {
                let mut stdin = std::io::stdin();
                let mut buf = [0u8; 4096];
                loop {
                    match stdin.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if out.write_all(&frame(INPUT, &buf[..n])).is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
        // Size changes.
        {
            let mut out = out.try_clone()?;
            std::thread::spawn(move || {
                let mut last = (c, r);
                loop {
                    std::thread::sleep(Duration::from_millis(250));
                    let now = terminal::size().unwrap_or(last);
                    if now != last {
                        last = now;
                        let mut d = now.0.to_be_bytes().to_vec();
                        d.extend_from_slice(&now.1.to_be_bytes());
                        if out.write_all(&frame(RESIZE, &d)).is_err() {
                            break;
                        }
                    }
                }
            });
        }
        let mut stdout = std::io::stdout();
        let ended = loop {
            match read_frame(&mut from) {
                Ok(Some((OUTPUT, data))) => {
                    let _ = stdout.write_all(&data);
                    let _ = stdout.flush();
                }
                Ok(Some((DETACHED, _))) => break Ended::Detached,
                Ok(Some((MOVED, _))) => break Ended::Moved,
                Ok(Some((EXITED, d))) if d.len() == 4 => {
                    break Ended::Exited(i32::from_be_bytes([d[0], d[1], d[2], d[3]]));
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break Ended::Lost,
            }
        };
        let _ = terminal::disable_raw_mode();
        if ended != Ended::Exited(0) {
            // The session's screen modes stay with it; give the terminal back.
            let _ = stdout.write_all(
                b"\x1b[<u\x1b[?1049l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1004l\x1b[?2004l\x1b[?25h\r\n",
            );
            let _ = stdout.flush();
        }
        Ok(ended)
    }

    /// Starts a host for `args` in `cwd`, detached from this terminal.
    pub fn spawn(cwd: &Path, args: &[String]) -> anyhow::Result<String> {
        use std::os::unix::process::CommandExt;
        let id = format!("h{}", chrono::Utc::now().timestamp_millis());
        let (c, r) = crossterm::terminal::size().unwrap_or((100, 30));
        let exe = std::env::current_exe()?;
        let mut cmd = std::process::Command::new(&exe);
        cmd.arg("__host")
            .args([
                "--id",
                &id,
                "--cols",
                &c.to_string(),
                "--rows",
                &r.to_string(),
                "--",
            ])
            .args(args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // SAFETY: setsid only detaches the child from this terminal.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        cmd.spawn()?;
        let sock = socket_of(&id);
        for _ in 0..100 {
            if sock.exists() {
                return Ok(id);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        anyhow::bail!("the background session didn't start")
    }
}

#[cfg(unix)]
pub use unix::{Ended, attach, run, spawn};

/// Whether this platform can host sessions.
pub fn supported() -> bool {
    cfg!(unix)
}

/// The words for how an attachment ended.
#[cfg(unix)]
pub fn farewell(ended: &Ended, id: &str) -> Option<String> {
    match ended {
        Ended::Detached => Some(format!(
            "Detached — the session keeps running. `dcode-ai` here (or `dcode-ai bg attach {id}`) brings it back."
        )),
        Ended::Moved => Some("The session moved to another terminal.".into()),
        Ended::Lost => Some("The background session ended.".into()),
        Ended::Exited(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let mut buf = frame(INPUT, b"hello");
        buf.extend(frame(RESIZE, &[0, 80, 0, 24]));
        let mut r = std::io::Cursor::new(buf);
        assert_eq!(
            read_frame(&mut r).unwrap(),
            Some((INPUT, b"hello".to_vec()))
        );
        assert_eq!(
            read_frame(&mut r).unwrap(),
            Some((RESIZE, vec![0, 80, 0, 24]))
        );
        assert_eq!(read_frame(&mut r).unwrap(), None);
    }

    /// A real program in a host: keys go in, its screen comes out, and the
    /// session survives the terminal going away.
    #[cfg(unix)]
    #[test]
    fn a_hosted_program_outlives_its_terminal() {
        use std::io::{Read, Write};
        let id = format!("test{}", std::process::id());
        let cwd = std::env::temp_dir();
        let host = {
            let id = id.clone();
            std::thread::spawn(move || {
                run(
                    &id,
                    Path::new("/bin/sh"),
                    &[
                        "-c".into(),
                        "read a; echo got:$a; read b; echo then:$b".into(),
                    ],
                    &cwd,
                    (80, 24),
                )
            })
        };
        let sock = socket_of(&id);
        for _ in 0..100 {
            if sock.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let read_until = |s: &mut std::os::unix::net::UnixStream, want: &str| {
            let mut seen = String::new();
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            while !seen.contains(want) {
                match read_frame(s) {
                    Ok(Some((OUTPUT, d))) => seen.push_str(&String::from_utf8_lossy(&d)),
                    Ok(Some(_)) => {}
                    _ => break,
                }
            }
            seen
        };
        // First terminal types a line, then goes away.
        let mut t1 = std::os::unix::net::UnixStream::connect(&sock).unwrap();
        t1.write_all(&frame(INPUT, b"one\r")).unwrap();
        let seen = read_until(&mut t1, "got:one");
        assert!(seen.contains("got:one"), "{seen:?}");
        drop(t1);
        assert!(
            here(&std::env::temp_dir()).iter().any(|h| h.id == id),
            "still running"
        );
        // A second terminal picks it up.
        let mut t2 = std::os::unix::net::UnixStream::connect(&sock).unwrap();
        std::thread::sleep(Duration::from_millis(100));
        t2.write_all(&frame(INPUT, b"two\r")).unwrap();
        assert!(read_until(&mut t2, "then:two").contains("then:two"));
        // It exits; the terminal is told the code.
        let mut tail = Vec::new();
        t2.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let _ = t2.read_to_end(&mut tail);
        assert_eq!(host.join().unwrap().unwrap(), 0);
        assert!(!sock.exists(), "cleaned up");
        assert!(unix::find(b"ab\x1b]7799;dcode-ai-detach\x07cd", DETACH_MARKER) == Some(2));
    }
}
