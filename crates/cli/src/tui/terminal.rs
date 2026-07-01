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
use ratatui::{Terminal, TerminalOptions, Viewport, backend::CrosstermBackend};

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

pub fn setup_terminal(_mouse_capture: bool) -> anyhow::Result<Terminal<CrosstermBackend<Stdout>>> {
    install_terminal_panic_hook();
    enable_raw_mode().map_err(|e| anyhow::anyhow!("enable_raw_mode: {e}"))?;
    let res: anyhow::Result<Terminal<CrosstermBackend<Stdout>>> = (|| {
        let mut out = stdout();
        let _ = execute!(out, EnableBracketedPaste);
        // Focus reporting lets us suppress completion notifications while the
        // terminal is focused (Codex parity).
        let _ = execute!(out, EnableFocusChange);
        use std::io::Write;
        let _ = out.flush();
        let height = inline_viewport_height();
        let backend = CrosstermBackend::new(out);
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
    let mut out = stdout();
    let _ = execute!(out, Show);
    let _ = execute!(out, DisableBracketedPaste);
    let _ = execute!(out, DisableFocusChange);
    let _ = disable_raw_mode();
    use std::io::Write;
    // Move below the viewport so the shell prompt starts on a clean line.
    let _ = writeln!(out);
    let _ = out.flush();
}
