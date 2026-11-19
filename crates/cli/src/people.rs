//! Who you are to the others on a session: the name shown in the office,
//! on `dcode-ai join` and on the `/watch` page.

use std::path::Path;

/// Your name: git `user.name` in `workspace`, else `$USER` / `$USERNAME`,
/// else "a teammate".
pub fn my_name(workspace: &Path) -> String {
    std::process::Command::new("git")
        .args(["config", "user.name"])
        .current_dir(workspace)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|n| !n.is_empty())
        .or_else(|| std::env::var("USER").ok().filter(|n| !n.is_empty()))
        .or_else(|| std::env::var("USERNAME").ok().filter(|n| !n.is_empty()))
        .unwrap_or_else(|| "a teammate".into())
}

/// A fresh id for one person on one session (`<prefix>-<random>`).
pub fn new_person_id(prefix: &str) -> String {
    use rand::Rng;
    let n: u64 = rand::thread_rng().r#gen();
    format!("{prefix}-{n:016x}")
}

/// A link as a QR code in half-block characters, one string per terminal
/// row, drawn so it reads as dark-on-light whatever the terminal's colours.
pub fn qr_lines(text: &str) -> Vec<String> {
    qr_lines_for(text, crate::tui::theme::is_light())
}

/// `light_terminal`: dark text on a light background.
pub fn qr_lines_for(text: &str, light_terminal: bool) -> Vec<String> {
    use qrcode::render::unicode::Dense1x2;
    let Ok(code) = qrcode::QrCode::new(text.as_bytes()) else {
        return Vec::new();
    };
    // On a dark terminal, text is light: draw the code's dark modules as
    // the background and light ones as text. A light terminal is the other
    // way round.
    let (dark, light) = if light_terminal {
        (Dense1x2::Dark, Dense1x2::Light)
    } else {
        (Dense1x2::Light, Dense1x2::Dark)
    };
    code.render::<Dense1x2>()
        .dark_color(dark)
        .light_color(light)
        .quiet_zone(true)
        .build()
        .lines()
        // A no-break space: wrapping and trimming leave it alone.
        .map(|l| l.replace(' ', "\u{a0}"))
        .collect()
}

/// A QR pixel pair: (top half, bottom half) of one terminal cell.
pub type CellColors = ([u8; 3], [u8; 3]);

/// A link as a QR code to draw with half blocks in exact colours: pure black
/// on white with a quiet border, whatever the terminal's theme — phones read
/// it reliably. One row per two module rows.
pub fn qr_picture(text: &str) -> Vec<Vec<CellColors>> {
    const QUIET: usize = 2;
    const BLACK: [u8; 3] = [0, 0, 0];
    const WHITE: [u8; 3] = [255, 255, 255];
    let Ok(code) = qrcode::QrCode::new(text.as_bytes()) else {
        return Vec::new();
    };
    let w = code.width();
    let colors = code.to_colors();
    let size = w + 2 * QUIET;
    let dark = |x: usize, y: usize| -> bool {
        x >= QUIET
            && y >= QUIET
            && x < QUIET + w
            && y < QUIET + w
            && colors[(y - QUIET) * w + (x - QUIET)] == qrcode::Color::Dark
    };
    let px = |x: usize, y: usize| if y < size && dark(x, y) { BLACK } else { WHITE };
    (0..size.div_ceil(2))
        .map(|r| {
            (0..size)
                .map(|x| (px(x, 2 * r), px(x, 2 * r + 1)))
                .collect()
        })
        .collect()
}

/// The QR as terminal text with explicit black/white colours (24-bit), for
/// output that isn't the full-screen view.
pub fn qr_ansi_lines(text: &str) -> Vec<String> {
    qr_picture(text)
        .into_iter()
        .map(|row| {
            let mut s = String::new();
            for (t, b) in row {
                s.push_str(&format!(
                    "\x1b[38;2;{};{};{};48;2;{};{};{}m\u{2580}",
                    t[0], t[1], t[2], b[0], b[1], b[2]
                ));
            }
            s.push_str("\x1b[0m");
            s
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn qr_pictures_are_black_on_white_and_square() {
        let rows = super::qr_picture("http://192.168.0.119:8788/?t=Qk3f9xq2abcdefghijkmnpqr");
        let w = rows[0].len();
        assert!(rows.iter().all(|r| r.len() == w));
        assert_eq!(rows.len(), w.div_ceil(2));
        // The quiet border is white; the finder pattern's corner is black.
        assert_eq!(rows[0][0], ([255, 255, 255], [255, 255, 255]));
        assert_eq!(rows[1][2].0, [0, 0, 0]);
        assert!(super::qr_ansi_lines("x")[0].ends_with("\x1b[0m"));
    }

    #[test]
    fn links_become_qr_codes() {
        let lines = super::qr_lines("http://192.168.0.119:8788/?t=Qk3f9xq2abcdefghijkmnpqr");
        assert!(lines.len() > 10);
        assert!(
            lines
                .iter()
                .all(|l| l.chars().count() == lines[0].chars().count())
        );
        assert!(
            lines
                .iter()
                .all(|l| l.chars().all(|c| "▀▄█\u{a0}".contains(c)))
        );
    }
}
