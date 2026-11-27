//! Terminal setup/teardown for the inline-viewport TUI (Codex-style):
//! raw mode + bracketed paste, a small inline viewport at the bottom of the
//! terminal, and NO alternate screen. Completed output is flushed into the
//! terminal's native scrollback; only the live input/streaming pane is drawn
//! by ratatui. Native terminal scroll shows history.

use std::io::{Stdout, stdout};

use crossterm::{
    cursor::Show,
    event::{DisableBracketedPaste, DisableFocusChange, EnableBracketedPaste, EnableFocusChange},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode},
};
use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use ratatui::{Terminal, TerminalOptions, Viewport};

/// The inline view's backend: crossterm, except that a terminal which
/// doesn't answer the cursor-position query (ESC[6n) in time no longer stops
/// dcode-ai with "The cursor position could not be read within a normal
/// duration". The inline viewport is placed from that answer at start and
/// on every resize; without one, ask again once, then assume the cursor is
/// on the last row — the viewport opens at the bottom of the screen, which
/// is where it usually is anyway. Once a terminal hasn't answered, it isn't
/// asked again (each try waits 2 s — a freeze on every resize).
pub struct InlineBackend(CrosstermBackend<Stdout>, bool);

impl InlineBackend {
    fn fallback_position(&self) -> Position {
        let rows = self.0.size().map(|s| s.height).unwrap_or(24);
        Position::new(0, rows.saturating_sub(1))
    }
}

impl Backend for InlineBackend {
    type Error = std::io::Error;

    fn draw<'a, I>(&mut self, content: I) -> std::io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.0.draw(content)
    }
    fn append_lines(&mut self, n: u16) -> std::io::Result<()> {
        self.0.append_lines(n)
    }
    fn hide_cursor(&mut self) -> std::io::Result<()> {
        self.0.hide_cursor()
    }
    fn show_cursor(&mut self) -> std::io::Result<()> {
        self.0.show_cursor()
    }
    fn get_cursor_position(&mut self) -> std::io::Result<Position> {
        if self.1 {
            return Ok(self.fallback_position());
        }
        match self.0.get_cursor_position() {
            Ok(p) => Ok(p),
            Err(first) => match self.0.get_cursor_position() {
                Ok(p) => Ok(p),
                Err(_) => {
                    self.1 = true;
                    tracing::debug!(
                        "terminal didn't report the cursor position ({first}); assuming the last row"
                    );
                    Ok(self.fallback_position())
                }
            },
        }
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> std::io::Result<()> {
        self.0.set_cursor_position(position)
    }
    fn clear(&mut self) -> std::io::Result<()> {
        self.0.clear()
    }
    fn clear_region(&mut self, clear_type: ClearType) -> std::io::Result<()> {
        self.0.clear_region(clear_type)
    }
    fn size(&self) -> std::io::Result<Size> {
        self.0.size()
    }
    fn window_size(&mut self) -> std::io::Result<WindowSize> {
        self.0.window_size()
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Backend::flush(&mut self.0)
    }
}

fn install_terminal_panic_hook() {
    use std::sync::Once;
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal(false);
            default(info);
        }));
    });
}

/// Height of the inline viewport (the live bottom pane). Completed content
/// scrolls above it in the terminal's native scrollback. Sized to give popups
/// (command palette, pickers) room to render centered, while keeping the live
/// input pane close to the latest output.
pub fn inline_viewport_height() -> u16 {
    let rows = crossterm::terminal::size().map(|(_, h)| h).unwrap_or(24);
    (rows / 2).clamp(12, 18)
}

/// Set when the terminal speaks the kitty keyboard protocol, so Shift+Enter
/// (and Esc) arrive unambiguously.
static KEY_FLAGS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Terminals known to support the kitty keyboard protocol. Detected from
/// the environment: querying the terminal would delay startup.
pub fn supports_kitty_keyboard() -> bool {
    let var = |k: &str| std::env::var(k).unwrap_or_default().to_ascii_lowercase();
    if std::env::var_os("KITTY_WINDOW_ID").is_some()
        || std::env::var_os("GHOSTTY_RESOURCES_DIR").is_some()
    {
        return true;
    }
    let program = var("TERM_PROGRAM");
    let term = var("TERM");
    matches!(program.as_str(), "ghostty" | "kitty" | "wezterm")
        || term.contains("kitty")
        || term.starts_with("foot")
}

/// Ask the terminal to report Shift+Enter and friends distinctly. Call after
/// entering a screen (main and alternate screens keep separate settings).
pub fn push_key_flags() {
    if KEY_FLAGS.load(std::sync::atomic::Ordering::Relaxed) {
        use crossterm::event::{KeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
        let _ = execute!(
            stdout(),
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        );
    }
}

/// Undo [`push_key_flags`] before leaving a screen.
pub fn pop_key_flags() {
    if KEY_FLAGS.load(std::sync::atomic::Ordering::Relaxed) {
        let _ = execute!(stdout(), crossterm::event::PopKeyboardEnhancementFlags);
    }
}

pub fn setup_terminal(_mouse_capture: bool) -> anyhow::Result<Terminal<InlineBackend>> {
    install_terminal_panic_hook();
    enable_raw_mode().map_err(|e| anyhow::anyhow!("enable_raw_mode: {e}"))?;
    let res: anyhow::Result<Terminal<InlineBackend>> = (|| {
        let mut out = stdout();
        let _ = execute!(out, EnableBracketedPaste);
        // Focus reporting lets us suppress completion notifications while the
        // terminal is focused (Codex parity).
        let _ = execute!(out, EnableFocusChange);
        if supports_kitty_keyboard() {
            KEY_FLAGS.store(true, std::sync::atomic::Ordering::Relaxed);
            push_key_flags();
        }
        use std::io::Write;
        let _ = out.flush();
        let height = inline_viewport_height();
        let backend = InlineBackend(CrosstermBackend::new(out), false);
        let terminal = Terminal::with_options(
            backend,
            TerminalOptions {
                viewport: Viewport::Inline(height),
            },
        )?;
        Ok(terminal)
    })();
    if res.is_err() {
        let _ = disable_raw_mode();
    }
    res
}

pub fn restore_terminal(_mouse_capture: bool) {
    pop_key_flags();
    let mut out = stdout();
    // Harmless when not on the alternate screen; required when the dashboard
    // (or a panic inside it) exits while it is active.
    let _ = execute!(out, crossterm::event::DisableMouseCapture);
    let _ = execute!(out, crossterm::terminal::LeaveAlternateScreen);
    let _ = execute!(out, Show);
    let _ = execute!(out, DisableBracketedPaste);
    let _ = execute!(out, DisableFocusChange);
    let _ = disable_raw_mode();
    use std::io::Write;
    // Move below the viewport so the shell prompt starts on a clean line.
    let _ = writeln!(out);
    let _ = out.flush();
}
