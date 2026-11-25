//! A real shell in a dashboard pane: a pseudo-terminal (`portable-pty`,
//! ConPTY on Windows) whose output `vt100` keeps as a screen, drawn cell by
//! cell. Keys go to it while its pane has focus.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};

use crate::tui::theme;

/// Lines of history kept above the screen.
const SCROLLBACK: usize = 2_000;

pub struct TermSession {
    parser: Arc<Mutex<vt100::Parser>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    master: Mutex<Box<dyn MasterPty + Send>>,
    child: Mutex<Box<dyn Child + Send + Sync>>,
    exited: Arc<AtomicBool>,
    /// Bumped on every chunk of output, so the screen knows to redraw.
    generation: Arc<AtomicU64>,
    size: Mutex<(u16, u16)>,
    /// Rows scrolled back into history (0 = live).
    scroll: Mutex<usize>,
    pub shell: String,
}

impl std::fmt::Debug for TermSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TermSession")
            .field("shell", &self.shell)
            .finish()
    }
}

/// The user's shell: `DCODE_AI_SHELL` if set; else `$SHELL`, bash or sh;
/// on Windows PowerShell 7 (`pwsh`), Windows PowerShell, then `%COMSPEC%`.
pub fn default_shell() -> String {
    if let Ok(s) = std::env::var("DCODE_AI_SHELL")
        && !s.trim().is_empty()
    {
        return s;
    }
    #[cfg(windows)]
    {
        ["pwsh.exe", "powershell.exe"]
            .into_iter()
            .find(|exe| on_path(exe))
            .map(str::to_string)
            .or_else(|| std::env::var("COMSPEC").ok())
            .unwrap_or_else(|| "cmd.exe".into())
    }
    #[cfg(not(windows))]
    {
        std::env::var("SHELL")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| {
                if Path::new("/bin/bash").exists() {
                    "/bin/bash".into()
                } else {
                    "/bin/sh".into()
                }
            })
    }
}

#[cfg(windows)]
fn on_path(exe: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(exe).is_file()))
}

/// Cursor-position requests (`ESC [ 6 n`) in `chunk`, with `tail` the end
/// of the previous chunk (a request can be split across reads). Windows'
/// ConPTY sends one at start and shows nothing until it is answered.
fn cursor_requests(tail: &mut Vec<u8>, chunk: &[u8]) -> usize {
    const Q: &[u8] = b"\x1b[6n";
    let mut joined = std::mem::take(tail);
    let skip = joined.len();
    joined.extend_from_slice(chunk);
    let n = joined
        .windows(Q.len())
        .enumerate()
        // Count a request once: it must end inside this chunk.
        .filter(|(i, w)| *w == Q && i + Q.len() > skip)
        .count();
    let keep = joined.len().saturating_sub(Q.len() - 1);
    *tail = joined[keep..].to_vec();
    n
}

impl TermSession {
    /// Starts `shell` in `cwd` on a `cols`×`rows` screen.
    pub fn spawn(shell: &str, cwd: &Path, cols: u16, rows: u16) -> anyhow::Result<Arc<Self>> {
        let (cols, rows) = (cols.max(10), rows.max(3));
        let pty = native_pty_system().openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        let mut cmd = CommandBuilder::new(shell);
        cmd.cwd(cwd);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        let child = pty.slave.spawn_command(cmd)?;
        drop(pty.slave);
        let writer: Arc<Mutex<Box<dyn Write + Send>>> =
            Arc::new(Mutex::new(pty.master.take_writer()?));
        let mut reader = pty.master.try_clone_reader()?;
        let parser = Arc::new(Mutex::new(vt100::Parser::new(rows, cols, SCROLLBACK)));
        let exited = Arc::new(AtomicBool::new(false));
        let generation = Arc::new(AtomicU64::new(0));
        {
            let (parser, exited, generation, writer) = (
                parser.clone(),
                exited.clone(),
                generation.clone(),
                writer.clone(),
            );
            std::thread::Builder::new()
                .name("term-pane".into())
                .spawn(move || {
                    let mut buf = [0u8; 16 * 1024];
                    let mut tail = Vec::new();
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                let asks = cursor_requests(&mut tail, &buf[..n]);
                                let mut at = (0, 0);
                                if let Ok(mut p) = parser.lock() {
                                    p.process(&buf[..n]);
                                    at = p.screen().cursor_position();
                                }
                                // Answer as a terminal would: where the
                                // cursor is, 1-based.
                                if asks > 0
                                    && let Ok(mut w) = writer.lock()
                                {
                                    let reply = format!("\x1b[{};{}R", at.0 + 1, at.1 + 1);
                                    for _ in 0..asks {
                                        let _ = w.write_all(reply.as_bytes());
                                    }
                                    let _ = w.flush();
                                }
                                generation.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                    }
                    exited.store(true, Ordering::Relaxed);
                    generation.fetch_add(1, Ordering::Relaxed);
                })?;
        }
        Ok(Arc::new(Self {
            parser,
            writer,
            master: Mutex::new(pty.master),
            child: Mutex::new(child),
            exited,
            generation,
            size: Mutex::new((cols, rows)),
            scroll: Mutex::new(0),
            shell: shell.to_string(),
        }))
    }

    pub fn exited(&self) -> bool {
        self.exited.load(Ordering::Relaxed)
            || self
                .child
                .lock()
                .ok()
                .and_then(|mut c| c.try_wait().ok().flatten())
                .is_some()
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// Sends bytes to the shell (typing returns to the live screen).
    pub fn write(&self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.scroll_to(0);
        if let Ok(mut w) = self.writer.lock() {
            let _ = w.write_all(bytes);
            let _ = w.flush();
        }
    }

    /// A key, as the shell expects it.
    pub fn send_key(&self, key: &KeyEvent) {
        let app_cursor = self
            .parser
            .lock()
            .map(|p| p.screen().application_cursor())
            .unwrap_or(false);
        self.write(&key_bytes(key, app_cursor));
    }

    /// Pasted text, bracketed when the program asked for it.
    pub fn paste(&self, text: &str) {
        let bracketed = self
            .parser
            .lock()
            .map(|p| p.screen().bracketed_paste())
            .unwrap_or(false);
        let text = text.replace("\r\n", "\r").replace('\n', "\r");
        if bracketed {
            self.write(format!("\x1b[200~{text}\x1b[201~").as_bytes());
        } else {
            self.write(text.as_bytes());
        }
    }

    /// Shift+PgUp/PgDn: move through the history (`rows` > 0 goes back).
    pub fn scroll_by(&self, rows: i64) {
        let cur = self.scroll.lock().map(|s| *s).unwrap_or(0) as i64;
        self.scroll_to((cur + rows).clamp(0, SCROLLBACK as i64) as usize);
    }

    fn scroll_to(&self, rows: usize) {
        if let Ok(mut s) = self.scroll.lock() {
            *s = rows;
        }
    }

    /// Fits the shell's screen to `cols`×`rows`.
    pub fn resize(&self, cols: u16, rows: u16) {
        let (cols, rows) = (cols.max(10), rows.max(3));
        let Ok(mut size) = self.size.lock() else {
            return;
        };
        if *size == (cols, rows) {
            return;
        }
        *size = (cols, rows);
        if let Ok(mut p) = self.parser.lock() {
            p.set_size(rows, cols);
        }
        if let Ok(m) = self.master.lock() {
            let _ = m.resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            });
        }
    }

    /// Draws the screen into `area` of `buf`; the cursor shows when
    /// `focused`. Returns whether the view is scrolled back.
    pub fn render(&self, area: Rect, buf: &mut Buffer, focused: bool) -> bool {
        self.resize(area.width, area.height);
        let want = self.scroll.lock().map(|s| *s).unwrap_or(0);
        let Ok(mut p) = self.parser.lock() else {
            return false;
        };
        p.set_scrollback(want);
        let scrolled = p.screen().scrollback();
        if scrolled != want {
            self.scroll_to(scrolled);
        }
        draw_screen(p.screen(), area, buf, focused && scrolled == 0);
        p.set_scrollback(0);
        scrolled > 0
    }
}

impl Drop for TermSession {
    fn drop(&mut self) {
        if let Ok(mut c) = self.child.lock() {
            let _ = c.kill();
        }
    }
}

fn color(c: vt100::Color, default: Color) -> Color {
    match c {
        vt100::Color::Default => default,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// The vt100 screen into ratatui cells.
pub fn draw_screen(screen: &vt100::Screen, area: Rect, buf: &mut Buffer, cursor: bool) {
    let (fg0, bg0) = (theme::text(), theme::bg());
    let (crow, ccol) = screen.cursor_position();
    let show_cursor = cursor && !screen.hide_cursor();
    for row in 0..area.height {
        for col in 0..area.width {
            let Some(cell) = buf.cell_mut((area.x + col, area.y + row)) else {
                continue;
            };
            let Some(vc) = screen.cell(row, col) else {
                cell.reset();
                cell.set_style(Style::default().bg(bg0));
                continue;
            };
            if vc.is_wide_continuation() {
                cell.set_symbol("");
                continue;
            }
            let mut fg = color(vc.fgcolor(), fg0);
            let mut bg = color(vc.bgcolor(), bg0);
            if vc.inverse() {
                std::mem::swap(&mut fg, &mut bg);
            }
            let mut m = Modifier::empty();
            if vc.bold() {
                m |= Modifier::BOLD;
            }
            if vc.italic() {
                m |= Modifier::ITALIC;
            }
            if vc.underline() {
                m |= Modifier::UNDERLINED;
            }
            if show_cursor && (row, col) == (crow, ccol) {
                m |= Modifier::REVERSED;
            }
            let text = vc.contents();
            cell.set_symbol(if text.is_empty() { " " } else { &text });
            cell.set_style(Style::default().fg(fg).bg(bg).add_modifier(m));
        }
    }
}

/// The bytes a terminal sends for `key`. `app_cursor`: the program asked
/// for application cursor keys (`ESC O A` instead of `ESC [ A`).
pub fn key_bytes(key: &KeyEvent, app_cursor: bool) -> Vec<u8> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    // xterm modifier parameter: 1 + shift + 2·alt + 4·ctrl.
    let mods = 1 + shift as u8 + 2 * alt as u8 + 4 * ctrl as u8;
    let csi = |end: char| -> Vec<u8> {
        if mods > 1 {
            format!("\x1b[1;{mods}{end}").into_bytes()
        } else if app_cursor {
            format!("\x1bO{end}").into_bytes()
        } else {
            format!("\x1b[{end}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if mods > 1 {
            format!("\x1b[{n};{mods}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };
    let mut out = match key.code {
        KeyCode::Char(c) if ctrl => {
            let b = match c.to_ascii_lowercase() {
                c @ 'a'..='z' => c as u8 - b'a' + 1,
                '@' | ' ' | '2' => 0,
                '[' | '3' => 0x1b,
                '\\' | '4' => 0x1c,
                ']' | '5' => 0x1d,
                '^' | '6' => 0x1e,
                '_' | '-' | '7' | '/' => 0x1f,
                '8' | '?' => 0x7f,
                _ => return Vec::new(),
            };
            vec![b]
        }
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace if ctrl => vec![0x08],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => return csi('A'),
        KeyCode::Down => return csi('B'),
        KeyCode::Right => return csi('C'),
        KeyCode::Left => return csi('D'),
        KeyCode::Home => return csi('H'),
        KeyCode::End => return csi('F'),
        KeyCode::Insert => return tilde(2),
        KeyCode::Delete => return tilde(3),
        KeyCode::PageUp => return tilde(5),
        KeyCode::PageDown => return tilde(6),
        KeyCode::F(n @ 1..=4) => {
            let end = b"PQRS"[n as usize - 1] as char;
            return if mods > 1 {
                format!("\x1b[1;{mods}{end}").into_bytes()
            } else {
                format!("\x1bO{end}").into_bytes()
            };
        }
        KeyCode::F(n @ 5..=12) => {
            let code = [15, 17, 18, 19, 20, 21, 23, 24][n as usize - 5];
            return tilde(code);
        }
        _ => return Vec::new(),
    };
    if alt {
        out.insert(0, 0x1b);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn keys_become_terminal_bytes() {
        let none = KeyModifiers::NONE;
        assert_eq!(
            key_bytes(&k(KeyCode::Char('c'), KeyModifiers::CONTROL), false),
            [3]
        );
        assert_eq!(key_bytes(&k(KeyCode::Char('x'), none), false), b"x");
        assert_eq!(
            key_bytes(&k(KeyCode::Char('b'), KeyModifiers::ALT), false),
            b"\x1bb"
        );
        assert_eq!(key_bytes(&k(KeyCode::Enter, none), false), b"\r");
        assert_eq!(key_bytes(&k(KeyCode::Backspace, none), false), [0x7f]);
        assert_eq!(key_bytes(&k(KeyCode::Up, none), false), b"\x1b[A");
        assert_eq!(key_bytes(&k(KeyCode::Up, none), true), b"\x1bOA");
        assert_eq!(
            key_bytes(&k(KeyCode::Right, KeyModifiers::CONTROL), true),
            b"\x1b[1;5C"
        );
        assert_eq!(key_bytes(&k(KeyCode::Delete, none), false), b"\x1b[3~");
        assert_eq!(key_bytes(&k(KeyCode::F(1), none), false), b"\x1bOP");
        assert_eq!(key_bytes(&k(KeyCode::F(5), none), false), b"\x1b[15~");
        assert_eq!(
            key_bytes(&k(KeyCode::Char('é'), none), false),
            "é".as_bytes()
        );
    }

    #[test]
    fn cursor_requests_are_found_even_split_across_reads() {
        let mut tail = Vec::new();
        assert_eq!(cursor_requests(&mut tail, b"hi\x1b[6n there \x1b[6n"), 2);
        assert_eq!(cursor_requests(&mut tail, b"\x1b["), 0);
        assert_eq!(cursor_requests(&mut tail, b"6n"), 1, "split request");
        assert_eq!(cursor_requests(&mut tail, b"plain"), 0, "not counted twice");
        assert_eq!(cursor_requests(&mut tail, b"\x1b[6"), 0);
        assert_eq!(cursor_requests(&mut tail, b"m"), 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_program_asking_for_the_cursor_gets_an_answer() {
        // `read` waits for the reply, as ConPTY does at start.
        if !Path::new("/bin/bash").exists() {
            return;
        }
        let t = TermSession::spawn("/bin/bash", &std::env::temp_dir(), 40, 5).expect("spawn");
        t.write(b"printf '\\033[6n'; IFS= read -rsd R pos; echo got${pos#*[}\r");
        let area = Rect::new(0, 0, 40, 5);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let mut buf = Buffer::empty(area);
            t.render(area, &mut buf, false);
            let text: String = (0..5)
                .flat_map(|y| (0..40).map(move |x| (x, y)))
                .map(|p| buf[p].symbol().to_string())
                .collect();
            // "got<row>;<col>" — digits, not the echoed command line.
            let answered = text.match_indices("got").any(|(i, _)| {
                let rest = &text[i + 3..];
                let row: String = rest.chars().take_while(char::is_ascii_digit).collect();
                !row.is_empty()
                    && rest[row.len()..].starts_with(';')
                    && rest[row.len() + 1..].starts_with(|c: char| c.is_ascii_digit())
            });
            if answered {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "no reply: {text}");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    #[test]
    fn the_screen_draws_with_colours_and_cursor() {
        let mut p = vt100::Parser::new(3, 10, 0);
        p.process(b"\x1b[31mred\x1b[0m \x1b[7mrev\x1b[0m");
        let area = Rect::new(0, 0, 10, 3);
        let mut buf = Buffer::empty(area);
        draw_screen(p.screen(), area, &mut buf, true);
        assert_eq!(buf[(0, 0)].symbol(), "r");
        assert_eq!(buf[(0, 0)].fg, Color::Indexed(1));
        assert_eq!(buf[(4, 0)].symbol(), "r");
        assert_eq!(buf[(4, 0)].bg, theme::text(), "inverse swaps colours");
        // The cursor sits after "rev".
        assert!(buf[(7, 0)].modifier.contains(Modifier::REVERSED));
    }

    #[cfg(unix)]
    #[test]
    fn a_shell_runs_and_its_output_shows() {
        let dir = std::env::temp_dir();
        let t = TermSession::spawn("/bin/sh", &dir, 40, 5).expect("spawn");
        t.write(b"echo hi-$((40+2))\r");
        let area = Rect::new(0, 0, 40, 5);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            let mut buf = Buffer::empty(area);
            t.render(area, &mut buf, false);
            let text: String = (0..5)
                .flat_map(|y| (0..40).map(move |x| (x, y)))
                .map(|p| buf[p].symbol().to_string())
                .collect();
            if text.contains("hi-42") {
                break;
            }
            assert!(std::time::Instant::now() < deadline, "no output: {text}");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        t.write(b"exit\r");
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !t.exited() {
            assert!(std::time::Instant::now() < deadline, "shell didn't exit");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}
