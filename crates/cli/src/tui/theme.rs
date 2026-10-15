//! TUI color themes.
//!
//! The active theme palette is stored in a global `RwLock<Theme>` and read via small
//! accessor functions (e.g. `theme::user()`, `theme::bg()`). Set the theme once at
//! startup from config (`[ui].theme = "tokyonight"`) and it applies to the whole TUI.
//!
//! Supported themes:
//! - `default` / `claude` (Claude Code-like: terminal background, warm orange
//!   accent, neutral greys)
//! - `dcode` (the original dark teal/cyan palette)
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
    /// Human label shown in the theme picker.
    pub label: &'static str,
    /// Designed for light terminal backgrounds.
    pub light: bool,
    /// Background of added / removed diff lines (`Color::Reset` = none).
    pub diff_add: Color,
    pub diff_del: Color,
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
    /// Primary accent color for branding, header highlights, and emphasis.
    pub accent: Color,
    /// Header/banner background color.
    #[allow(dead_code)]
    pub header_bg: Color,
}

/// Default theme — Claude Code-like: inherits the terminal background, warm
/// orange accent, neutral grey secondary text.
pub const CLAUDE: Theme = Theme {
    name: "claude",
    label: "Dark mode",
    light: false,
    diff_add: Color::Rgb(34, 92, 43),
    diff_del: Color::Rgb(122, 41, 54),
    bg: Color::Reset,
    surface: Color::Reset,
    border: Color::Rgb(136, 136, 136),
    mention_bg: Color::Rgb(60, 60, 78),
    user: Color::Rgb(153, 153, 153),
    assistant: Color::Rgb(215, 119, 87),
    tool: Color::Rgb(177, 185, 249),
    muted: Color::Rgb(153, 153, 153),
    text: Color::Rgb(235, 235, 235),
    success: Color::Rgb(78, 186, 101),
    error: Color::Rgb(255, 107, 128),
    warn: Color::Rgb(255, 193, 7),
    accent: Color::Rgb(215, 119, 87),
    header_bg: Color::Reset,
};

/// Claude-style light theme for light terminal backgrounds.
pub const CLAUDE_LIGHT: Theme = Theme {
    name: "claude-light",
    label: "Light mode",
    light: true,
    diff_add: Color::Rgb(198, 240, 205),
    diff_del: Color::Rgb(255, 214, 220),
    bg: Color::Reset,
    surface: Color::Reset,
    border: Color::Rgb(153, 153, 153),
    mention_bg: Color::Rgb(220, 226, 248),
    user: Color::Rgb(102, 102, 102),
    assistant: Color::Rgb(201, 100, 66),
    tool: Color::Rgb(87, 105, 247),
    muted: Color::Rgb(102, 102, 102),
    text: Color::Rgb(0, 0, 0),
    success: Color::Rgb(44, 122, 57),
    error: Color::Rgb(171, 43, 63),
    warn: Color::Rgb(150, 108, 30),
    accent: Color::Rgb(201, 100, 66),
    header_bg: Color::Reset,
};

/// Dark theme with a blue/orange palette in place of green/red.
pub const CLAUDE_DARK_DALTONIZED: Theme = Theme {
    name: "claude-dark-daltonized",
    label: "Dark mode (colorblind-friendly)",
    light: false,
    diff_add: Color::Rgb(0, 68, 102),
    diff_del: Color::Rgb(102, 51, 0),
    success: Color::Rgb(51, 166, 255),
    error: Color::Rgb(255, 153, 51),
    ..CLAUDE
};

/// Light theme with a blue/orange palette in place of green/red.
pub const CLAUDE_LIGHT_DALTONIZED: Theme = Theme {
    name: "claude-light-daltonized",
    label: "Light mode (colorblind-friendly)",
    light: true,
    diff_add: Color::Rgb(204, 229, 255),
    diff_del: Color::Rgb(255, 229, 204),
    success: Color::Rgb(0, 102, 153),
    error: Color::Rgb(204, 102, 0),
    ..CLAUDE_LIGHT
};

/// Dark theme using only the terminal's 16 ANSI colours.
pub const CLAUDE_DARK_ANSI: Theme = Theme {
    name: "claude-dark-ansi",
    label: "Dark mode (ANSI colors only)",
    light: false,
    diff_add: Color::Reset,
    diff_del: Color::Reset,
    bg: Color::Reset,
    surface: Color::Reset,
    border: Color::DarkGray,
    mention_bg: Color::Blue,
    user: Color::Gray,
    assistant: Color::LightRed,
    tool: Color::LightBlue,
    muted: Color::Gray,
    text: Color::White,
    success: Color::Green,
    error: Color::Red,
    warn: Color::Yellow,
    accent: Color::LightRed,
    header_bg: Color::Reset,
};

/// Light theme using only the terminal's 16 ANSI colours.
pub const CLAUDE_LIGHT_ANSI: Theme = Theme {
    name: "claude-light-ansi",
    label: "Light mode (ANSI colors only)",
    light: true,
    border: Color::Gray,
    mention_bg: Color::LightBlue,
    user: Color::DarkGray,
    assistant: Color::Red,
    tool: Color::Blue,
    muted: Color::DarkGray,
    text: Color::Black,
    accent: Color::Red,
    ..CLAUDE_DARK_ANSI
};

/// The original dcode-ai dark theme — sleek dark with cyan/violet accents.
pub const DEFAULT_DARK: Theme = Theme {
    name: "dcode",
    label: "dcode (teal)",
    light: false,
    diff_add: Color::Rgb(33, 58, 43),
    diff_del: Color::Rgb(74, 34, 29),
    bg: Color::Rgb(15, 17, 23),
    surface: Color::Rgb(22, 25, 35),
    border: Color::Rgb(48, 54, 74),
    mention_bg: Color::Rgb(45, 60, 100),
    user: Color::Rgb(72, 209, 204),
    assistant: Color::Rgb(180, 160, 255),
    tool: Color::Rgb(100, 230, 200),
    muted: Color::Rgb(100, 108, 138),
    text: Color::Rgb(240, 244, 255),
    success: Color::Rgb(80, 220, 140),
    error: Color::Rgb(245, 110, 120),
    warn: Color::Rgb(250, 200, 80),
    accent: Color::Rgb(72, 209, 204),
    header_bg: Color::Rgb(18, 20, 28),
};

/// Tokyo Night (storm variant).
pub const TOKYONIGHT: Theme = Theme {
    name: "tokyonight",
    label: "Tokyo Night",
    light: false,
    diff_add: Color::Rgb(33, 58, 43),
    diff_del: Color::Rgb(74, 34, 29),
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
    accent: Color::Rgb(125, 207, 255),
    header_bg: Color::Rgb(26, 27, 38),
};

/// Catppuccin Mocha.
pub const CATPPUCCIN: Theme = Theme {
    name: "catppuccin",
    label: "Catppuccin Mocha",
    light: false,
    diff_add: Color::Rgb(33, 58, 43),
    diff_del: Color::Rgb(74, 34, 29),
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
    accent: Color::Rgb(137, 180, 250),
    header_bg: Color::Rgb(30, 30, 46),
};

/// Gruvbox Dark (medium contrast).
pub const GRUVBOX: Theme = Theme {
    name: "gruvbox",
    label: "Gruvbox",
    light: false,
    diff_add: Color::Rgb(33, 58, 43),
    diff_del: Color::Rgb(74, 34, 29),
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
    accent: Color::Rgb(131, 165, 152),
    header_bg: Color::Rgb(40, 40, 40),
};

/// Dracula.
pub const DRACULA: Theme = Theme {
    name: "dracula",
    label: "Dracula",
    light: false,
    diff_add: Color::Rgb(33, 58, 43),
    diff_del: Color::Rgb(74, 34, 29),
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
    accent: Color::Rgb(139, 233, 253),
    header_bg: Color::Rgb(40, 42, 54),
};

/// Nord.
pub const NORD: Theme = Theme {
    name: "nord",
    label: "Nord",
    light: false,
    diff_add: Color::Rgb(33, 58, 43),
    diff_del: Color::Rgb(74, 34, 29),
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
    accent: Color::Rgb(136, 192, 208),
    header_bg: Color::Rgb(46, 52, 64),
};

/// Soft light theme.
pub const LIGHT: Theme = Theme {
    name: "soft-light",
    label: "Soft light",
    light: true,
    diff_add: Color::Rgb(218, 251, 225),
    diff_del: Color::Rgb(255, 235, 233),
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
    accent: Color::Rgb(14, 116, 178),
    header_bg: Color::Rgb(245, 245, 250),
};

/// Transparent — no background fill, inherits terminal/window background.
/// Use this when your terminal has a custom background color or wallpaper.
pub const TRANSPARENT: Theme = Theme {
    name: "transparent",
    label: "Transparent",
    light: false,
    diff_add: Color::Rgb(33, 58, 43),
    diff_del: Color::Rgb(74, 34, 29),
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
    accent: Color::Rgb(125, 207, 255),
    header_bg: Color::Reset,
};

/// The themes the picker offers (Claude Code's set).
pub const CLAUDE_THEMES: &[Theme] = &[
    CLAUDE,
    CLAUDE_LIGHT,
    CLAUDE_DARK_DALTONIZED,
    CLAUDE_LIGHT_DALTONIZED,
    CLAUDE_DARK_ANSI,
    CLAUDE_LIGHT_ANSI,
];

/// All built-in themes, in display order.
pub const ALL_THEMES: &[Theme] = &[
    CLAUDE,
    CLAUDE_LIGHT,
    CLAUDE_DARK_DALTONIZED,
    CLAUDE_LIGHT_DALTONIZED,
    CLAUDE_DARK_ANSI,
    CLAUDE_LIGHT_ANSI,
    DEFAULT_DARK,
    TOKYONIGHT,
    CATPPUCCIN,
    GRUVBOX,
    DRACULA,
    NORD,
    LIGHT,
    TRANSPARENT,
];

/// Look up a theme by config name. Falls back to `CLAUDE` when the name is unknown.
pub fn resolve(name: Option<&str>) -> Theme {
    let Some(raw) = name else {
        return CLAUDE;
    };
    let key = raw.trim().to_ascii_lowercase().replace('_', "-");
    match key.as_str() {
        "default" | "claude" | "dark" | "claude-dark" => CLAUDE,
        "light" | "claude-light" => CLAUDE_LIGHT,
        "claude-dark-daltonized" | "dark-daltonized" | "dark-colorblind" => CLAUDE_DARK_DALTONIZED,
        "claude-light-daltonized" | "light-daltonized" | "light-colorblind" => {
            CLAUDE_LIGHT_DALTONIZED
        }
        "claude-dark-ansi" | "dark-ansi" | "ansi" => CLAUDE_DARK_ANSI,
        "claude-light-ansi" | "light-ansi" => CLAUDE_LIGHT_ANSI,
        "dcode" => DEFAULT_DARK,
        "tokyonight" | "tokyo-night" | "tokyo" => TOKYONIGHT,
        "catppuccin" | "mocha" => CATPPUCCIN,
        "gruvbox" | "gruv" => GRUVBOX,
        "dracula" => DRACULA,
        "nord" => NORD,
        "soft-light" => LIGHT,
        "transparent" | "clear" => TRANSPARENT,
        _ => CLAUDE,
    }
}

static ACTIVE: RwLock<Theme> = RwLock::new(CLAUDE);

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
    ACTIVE.read().map(|g| *g).unwrap_or(CLAUDE)
}

// Accessor functions preserve the "looks like a const" call pattern at use sites
// (`theme::user()`, `theme::bg()`, …) while reading from the active theme at runtime.

/// The active theme uses only the 16 ANSI colours.
#[inline]
pub fn ansi_only() -> bool {
    current().name.ends_with("-ansi")
}

/// The active theme is meant for a light terminal background.
#[inline]
pub fn is_light() -> bool {
    current().light
}

/// Background of added diff lines (`Color::Reset` = none).
#[inline]
pub fn diff_add() -> Color {
    current().diff_add
}

/// Background of removed diff lines (`Color::Reset` = none).
#[inline]
pub fn diff_del() -> Color {
    current().diff_del
}

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

/// Rule colour of the composer in `!` shell mode (Claude's `bashBorder`).
pub fn bash_border() -> Color {
    let t = current();
    if t.name.contains("ansi") {
        Color::Magenta
    } else if t.light {
        Color::Rgb(255, 0, 135)
    } else {
        Color::Rgb(253, 93, 177)
    }
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

#[inline]
pub fn accent() -> Color {
    current().accent
}

/// Readable foreground for text drawn on top of the accent color (selected
/// rows / highlights). Picks a dark or light foreground from the accent's
/// perceived luminance, so highlights stay legible on any theme instead of
/// hardcoding black (which is low-contrast on a dark accent).
pub fn on_accent() -> Color {
    match accent() {
        Color::Rgb(r, g, b) => {
            let lum = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
            if lum > 140.0 {
                Color::Rgb(15, 17, 23)
            } else {
                Color::Rgb(240, 244, 255)
            }
        }
        _ => Color::Black,
    }
}

#[inline]
#[allow(dead_code)]
pub fn header_bg() -> Color {
    current().header_bg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_known_and_unknown_names() {
        assert_eq!(resolve(Some("tokyonight")).name, "tokyonight");
        assert_eq!(resolve(Some("TOKYO-NIGHT")).name, "tokyonight");
        assert_eq!(resolve(Some("catppuccin")).name, "catppuccin");
        assert_eq!(resolve(Some("does-not-exist")).name, "claude");
        assert_eq!(resolve(None).name, "claude");
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
