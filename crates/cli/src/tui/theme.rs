//! TUI color themes.
//!
//! The active theme palette is stored in a global `RwLock<Theme>` and read via small
//! accessor functions (e.g. `theme::user()`, `theme::bg()`). Set the theme once at
//! startup from config (`[ui].theme = "tokyonight"`) and it applies to the whole TUI.
//!
//! Supported themes:
//! - `default` (dcode — balanced dark teal/cyan, the original palette refined)
//! - `tokyonight` (Tokyo Night dark, blue/purple)
//! - `catppuccin` (Catppuccin Mocha, warm pastel dark)
//! - `gruvbox` (Gruvbox Dark, earthy retro)
//! - `dracula` (Dracula, high-contrast purple/pink)
//! - `nord` (Nord, cool blue-steel)
//! - `light` (soft light theme for bright terminals)
//! - `transparent` (no background fill — inherits terminal background color)

use ratatui::style::Color;
use std::sync::RwLock;

/// Palette of semantic colors used throughout the TUI.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// Canonical name used in config (lowercase, dash-separated).
    pub name: &'static str,
    /// Background fill for the main viewport.
    pub bg: Color,
    /// Surface color for popovers, modals, and sidebars.
    pub surface: Color,
    /// Border color for panels.
    pub border: Color,
    /// Highlight background for `@mention` tokens in the composer.
    pub mention_bg: Color,
    /// Accent color for user-authored elements (prompt caret, user chip).
    pub user: Color,
    /// Accent color for assistant replies.
    pub assistant: Color,
    /// Accent color for tool calls.
    pub tool: Color,
    /// Muted/secondary text (hints, timestamps, inactive items).
    pub muted: Color,
    /// Primary foreground text color.
    pub text: Color,
    /// Success indicator.
    pub success: Color,
    /// Error indicator.
    pub error: Color,
    /// Warning / caution indicator.
    pub warn: Color,
}

/// Built-in dcode-ai dark theme (refined).
pub const DEFAULT_DARK: Theme = Theme {
    name: "default",
    bg: Color::Rgb(18, 20, 26),
    surface: Color::Rgb(28, 30, 40),
    border: Color::Rgb(62, 66, 86),
    mention_bg: Color::Rgb(55, 72, 110),
    user: Color::Rgb(86, 207, 250),
    assistant: Color::Rgb(194, 174, 255),
    tool: Color::Rgb(110, 240, 215),
    muted: Color::Rgb(114, 121, 148),
    text: Color::Rgb(246, 249, 255),
    success: Color::Rgb(96, 230, 145),
    error: Color::Rgb(250, 130, 130),
    warn: Color::Rgb(255, 205, 85),
};

/// Tokyo Night (storm variant).
pub const TOKYONIGHT: Theme = Theme {
    name: "tokyonight",
    bg: Color::Rgb(26, 27, 38),
    surface: Color::Rgb(36, 40, 59),
    border: Color::Rgb(65, 72, 104),
    mention_bg: Color::Rgb(54, 63, 101),
    user: Color::Rgb(125, 207, 255),
    assistant: Color::Rgb(187, 154, 247),
    tool: Color::Rgb(115, 218, 202),
    muted: Color::Rgb(86, 95, 137),
    text: Color::Rgb(192, 202, 245),
    success: Color::Rgb(158, 206, 106),
    error: Color::Rgb(247, 118, 142),
    warn: Color::Rgb(224, 175, 104),
};

/// Catppuccin Mocha.
pub const CATPPUCCIN: Theme = Theme {
    name: "catppuccin",
    bg: Color::Rgb(30, 30, 46),
    surface: Color::Rgb(49, 50, 68),
    border: Color::Rgb(69, 71, 90),
    mention_bg: Color::Rgb(88, 91, 112),
    user: Color::Rgb(137, 180, 250),
    assistant: Color::Rgb(203, 166, 247),
    tool: Color::Rgb(148, 226, 213),
    muted: Color::Rgb(147, 153, 178),
    text: Color::Rgb(205, 214, 244),
    success: Color::Rgb(166, 227, 161),
    error: Color::Rgb(243, 139, 168),
    warn: Color::Rgb(249, 226, 175),
};

/// Gruvbox Dark (medium contrast).
pub const GRUVBOX: Theme = Theme {
    name: "gruvbox",
    bg: Color::Rgb(40, 40, 40),
    surface: Color::Rgb(60, 56, 54),
    border: Color::Rgb(80, 73, 69),
    mention_bg: Color::Rgb(102, 92, 84),
    user: Color::Rgb(131, 165, 152),
    assistant: Color::Rgb(211, 134, 155),
    tool: Color::Rgb(142, 192, 124),
    muted: Color::Rgb(168, 153, 132),
    text: Color::Rgb(235, 219, 178),
    success: Color::Rgb(184, 187, 38),
    error: Color::Rgb(251, 73, 52),
    warn: Color::Rgb(250, 189, 47),
};

/// Dracula.
pub const DRACULA: Theme = Theme {
    name: "dracula",
    bg: Color::Rgb(40, 42, 54),
    surface: Color::Rgb(68, 71, 90),
    border: Color::Rgb(98, 114, 164),
    mention_bg: Color::Rgb(98, 80, 129),
    user: Color::Rgb(139, 233, 253),
    assistant: Color::Rgb(189, 147, 249),
    tool: Color::Rgb(80, 250, 123),
    muted: Color::Rgb(98, 114, 164),
    text: Color::Rgb(248, 248, 242),
    success: Color::Rgb(80, 250, 123),
    error: Color::Rgb(255, 85, 85),
    warn: Color::Rgb(241, 250, 140),
};

/// Nord.
pub const NORD: Theme = Theme {
    name: "nord",
    bg: Color::Rgb(46, 52, 64),
    surface: Color::Rgb(59, 66, 82),
    border: Color::Rgb(76, 86, 106),
    mention_bg: Color::Rgb(67, 76, 94),
    user: Color::Rgb(136, 192, 208),
    assistant: Color::Rgb(180, 142, 173),
    tool: Color::Rgb(143, 188, 187),
    muted: Color::Rgb(129, 140, 158),
    text: Color::Rgb(236, 239, 244),
    success: Color::Rgb(163, 190, 140),
    error: Color::Rgb(191, 97, 106),
    warn: Color::Rgb(235, 203, 139),
};

/// Soft light theme.
pub const LIGHT: Theme = Theme {
    name: "light",
    bg: Color::Rgb(250, 250, 252),
    surface: Color::Rgb(238, 240, 246),
    border: Color::Rgb(196, 200, 214),
    mention_bg: Color::Rgb(208, 220, 245),
    user: Color::Rgb(14, 116, 178),
    assistant: Color::Rgb(124, 58, 205),
    tool: Color::Rgb(17, 153, 138),
    muted: Color::Rgb(110, 115, 135),
    text: Color::Rgb(30, 34, 46),
    success: Color::Rgb(22, 150, 80),
    error: Color::Rgb(200, 60, 60),
    warn: Color::Rgb(190, 130, 20),
};

/// Transparent — no background fill, inherits terminal/window background.
/// Use this when your terminal has a custom background color or wallpaper.
pub const TRANSPARENT: Theme = Theme {
    name: "transparent",
    bg: Color::Reset,
    surface: Color::Reset,
    border: Color::Rgb(86, 95, 137),
    mention_bg: Color::Rgb(54, 63, 101),
    user: Color::Rgb(125, 207, 255),
    assistant: Color::Rgb(187, 154, 247),
    tool: Color::Rgb(115, 218, 202),
    muted: Color::Rgb(102, 112, 158),
    text: Color::Rgb(192, 202, 245),
    success: Color::Rgb(158, 206, 106),
    error: Color::Rgb(247, 118, 142),
    warn: Color::Rgb(224, 175, 104),
};

/// All built-in themes, in display order.
pub const ALL_THEMES: &[Theme] = &[
    DEFAULT_DARK,
    TOKYONIGHT,
    CATPPUCCIN,
    GRUVBOX,
    DRACULA,
    NORD,
    LIGHT,
    TRANSPARENT,
];

/// Look up a theme by config name. Falls back to `DEFAULT_DARK` when the name is unknown.
pub fn resolve(name: Option<&str>) -> Theme {
    let Some(raw) = name else {
        return DEFAULT_DARK;
    };
    let key = raw.trim().to_ascii_lowercase().replace('_', "-");
    match key.as_str() {
        "default" | "dcode" | "dark" => DEFAULT_DARK,
        "tokyonight" | "tokyo-night" | "tokyo" => TOKYONIGHT,
        "catppuccin" | "mocha" => CATPPUCCIN,
        "gruvbox" | "gruv" => GRUVBOX,
        "dracula" => DRACULA,
        "nord" => NORD,
        "light" => LIGHT,
        "transparent" | "clear" => TRANSPARENT,
        _ => DEFAULT_DARK,
    }
}

static ACTIVE: RwLock<Theme> = RwLock::new(DEFAULT_DARK);

/// Replace the active theme. Safe to call at startup or when the user switches.
pub fn set_active(theme: Theme) {
    if let Ok(mut w) = ACTIVE.write() {
        *w = theme;
    }
}

/// Resolve and activate a theme by name. Returns the theme that ended up active.
pub fn set_by_name(name: Option<&str>) -> Theme {
    let t = resolve(name);
    set_active(t);
    t
}

/// Current theme palette snapshot.
pub fn current() -> Theme {
    ACTIVE.read().map(|g| *g).unwrap_or(DEFAULT_DARK)
}

// Accessor functions preserve the "looks like a const" call pattern at use sites
// (`theme::user()`, `theme::bg()`, …) while reading from the active theme at runtime.

#[inline]
pub fn bg() -> Color {
    current().bg
}

#[inline]
pub fn surface() -> Color {
    current().surface
}

#[inline]
pub fn border() -> Color {
    current().border
}

#[inline]
pub fn mention_bg() -> Color {
    current().mention_bg
}

#[inline]
pub fn user() -> Color {
    current().user
}

#[inline]
pub fn assistant() -> Color {
    current().assistant
}

#[inline]
pub fn tool() -> Color {
    current().tool
}

#[inline]
pub fn muted() -> Color {
    current().muted
}

#[inline]
pub fn text() -> Color {
    current().text
}

#[inline]
pub fn success() -> Color {
    current().success
}

#[inline]
pub fn error() -> Color {
    current().error
}

#[inline]
pub fn warn() -> Color {
    current().warn
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_known_and_unknown_names() {
        assert_eq!(resolve(Some("tokyonight")).name, "tokyonight");
        assert_eq!(resolve(Some("TOKYO-NIGHT")).name, "tokyonight");
        assert_eq!(resolve(Some("catppuccin")).name, "catppuccin");
        assert_eq!(resolve(Some("does-not-exist")).name, "default");
        assert_eq!(resolve(None).name, "default");
    }

    #[test]
    fn set_by_name_updates_active_palette() {
        let before = current();
        let switched = set_by_name(Some("gruvbox"));
        assert_eq!(switched.name, "gruvbox");
        assert_eq!(current().name, "gruvbox");
        // restore to avoid leaking state to other tests
        set_active(before);
    }
}
