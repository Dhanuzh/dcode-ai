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

#[cfg(test)]
mod tests {
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
