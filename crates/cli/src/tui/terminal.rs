//! Terminal setup/teardown for the full-screen TUI: raw mode, alternate
//! screen, bracketed paste, selective mouse capture, and a panic hook that
//! best-effort restores all of the above.

use std::io::{Stdout, stdout};

use crossterm::{
    cursor::{Hide, Show},
    event::{DisableBracketedPaste, EnableBracketedPaste},
    execute,
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode,
    },
};
use ratatui::{Terminal, backend::CrosstermBackend};

/// Install a panic hook (once) that best-effort restores the terminal before
/// the process unwinds/aborts. Without this, `panic = "abort"` skips every
/// `Drop`, so a panic inside the TUI leaves the user's terminal stuck in raw
/// mode and the alternate screen.
fn install_terminal_panic_hook() {
    use std::sync::Once;
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let default = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal(true);
            default(info);
        }));
    });
}

pub fn setup_terminal(mouse_capture: bool) -> anyhow::Result<Terminal<CrosstermBackend<Stdout>>> {
    install_terminal_panic_hook();
    enable_raw_mode().map_err(|e| anyhow::anyhow!("enable_raw_mode: {e}"))?;
    let res: anyhow::Result<Terminal<CrosstermBackend<Stdout>>> = (|| {
        let mut out = stdout();
        execute!(out, EnterAlternateScreen)?;
        let _ = execute!(out, EnableBracketedPaste);
        use std::io::Write;
        if mouse_capture {
            // Koda-style selective mouse capture:
            // - ?1002h button-event tracking (includes drag with button held)
            // - ?1006h SGR extended coordinates
            // This enables click-drag range selection in the in-app transcript.
            out.write_all(b"\x1b[?1002h\x1b[?1006h")
                .map_err(|e| anyhow::anyhow!("mouse enable: {e}"))?;
        }
        let _ = out.flush();
        execute!(out, Hide)?;
        execute!(out, Clear(ClearType::All))?;
        Ok(Terminal::new(CrosstermBackend::new(out))?)
    })();
    if res.is_err() {
        let _ = disable_raw_mode();
    }
    res
}

pub fn restore_terminal(_mouse_capture: bool) {
    let mut out = stdout();
    let _ = execute!(out, Show);
    let _ = execute!(out, DisableBracketedPaste);
    use std::io::Write;
    let _ = out.write_all(b"\x1b[?1002l\x1b[?1006l");
    let _ = out.flush();
    let _ = execute!(out, LeaveAlternateScreen);
    let _ = disable_raw_mode();
}
