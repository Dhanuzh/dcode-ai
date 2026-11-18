//! The dashboard's layout as you left it: the side panels' widths (dragged
//! or Alt+←/→), which panels show, kept in `~/.dcode-ai/tui.json`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DashPrefs {
    /// Left (sessions) panel width; 0 = the default.
    #[serde(default)]
    pub left_w: u16,
    /// Right (context, changes, agents…) panel width; 0 = the default.
    #[serde(default)]
    pub right_w: u16,
    #[serde(default)]
    pub hide_left: bool,
    #[serde(default)]
    pub hide_right: bool,
}

fn file() -> Option<std::path::PathBuf> {
    dcode_ai_common::config::dcode_ai_home_dir().map(|d| d.join("tui.json"))
}

pub fn load() -> DashPrefs {
    file()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(p: &DashPrefs) {
    if let Some(f) = file() {
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(f, serde_json::to_string_pretty(p).unwrap_or_default());
    }
}

/// A width moved by `delta` columns from `current` (0 = `default`), kept
/// between `min` and `max`.
pub fn nudge_width(current: u16, default: u16, delta: i32, min: u16, max: u16) -> u16 {
    let base = if current == 0 { default } else { current } as i32;
    (base + delta).clamp(min as i32, max.max(min) as i32) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn widths_move_within_bounds() {
        assert_eq!(nudge_width(0, 44, 4, 16, 80), 48);
        assert_eq!(nudge_width(48, 44, -40, 16, 80), 16);
        assert_eq!(nudge_width(78, 44, 10, 16, 80), 80);
    }

    #[test]
    fn prefs_round_trip() {
        let p = DashPrefs {
            left_w: 30,
            right_w: 50,
            hide_left: true,
            hide_right: false,
        };
        let s = serde_json::to_string(&p).unwrap();
        assert_eq!(serde_json::from_str::<DashPrefs>(&s).unwrap(), p);
        // An empty or older file still loads.
        assert_eq!(
            serde_json::from_str::<DashPrefs>("{}").unwrap(),
            DashPrefs::default()
        );
    }
}
