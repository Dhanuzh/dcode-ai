//! Customizable key bindings: parse `ctrl+b`-style combos, defaults per
//! action, and translation of incoming keys through user aliases.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A key combination: a key code plus active modifiers. Used by the
/// customizable-keybinding layer to match incoming keys against user aliases.
pub type KeyCombo = (KeyCode, KeyModifiers);

/// Parse a key string like `"ctrl+b"`, `"alt+enter"`, or `"f5"` into a
/// [`KeyCombo`]. Returns `None` for unrecognized syntax.
pub fn parse_key_combo(s: &str) -> Option<KeyCombo> {
    let parts: Vec<&str> = s
        .split('+')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        return None;
    }
    let (mod_parts, key_part) = parts.split_at(parts.len() - 1);
    let mut mods = KeyModifiers::NONE;
    for m in mod_parts {
        match m.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= KeyModifiers::CONTROL,
            "alt" | "option" | "meta" => mods |= KeyModifiers::ALT,
            "shift" => mods |= KeyModifiers::SHIFT,
            _ => return None,
        }
    }
    let k = key_part[0].to_ascii_lowercase();
    let code = match k.as_str() {
        "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "esc" | "escape" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        s if s.chars().count() == 1 => KeyCode::Char(s.chars().next()?),
        s if s.len() >= 2 && s.starts_with('f') => KeyCode::F(s[1..].parse::<u8>().ok()?),
        _ => return None,
    };
    Some((code, mods))
}

/// Default key for a remappable global action, or `None` for unknown actions.
pub fn default_action_key(action: &str) -> Option<KeyCombo> {
    use KeyCode::Char;
    Some(match action.to_ascii_lowercase().as_str() {
        "palette" => (Char('p'), KeyModifiers::CONTROL),
        "search" | "transcript_search" => (Char('f'), KeyModifiers::CONTROL),
        "history" => (Char('r'), KeyModifiers::CONTROL),
        "pin" => (Char('k'), KeyModifiers::CONTROL),
        "expand" => (Char('o'), KeyModifiers::CONTROL),
        "subagents" | "subagent" => (Char('g'), KeyModifiers::CONTROL),
        "thinking" => (Char('t'), KeyModifiers::CONTROL),
        "clear" => (Char('l'), KeyModifiers::CONTROL),
        _ => return None,
    })
}

/// Global actions whose keys can be remapped via `/keymap`.
const REMAPPABLE_ACTIONS: &[&str] = &[
    "palette",
    "search",
    "history",
    "pin",
    "expand",
    "subagents",
    "thinking",
    "clear",
];

/// A remappable action's built-in default key and its effective (possibly
/// user-overridden) key.
#[derive(Clone, Copy)]
pub struct KeyBinding {
    pub default: KeyCombo,
    pub effective: KeyCombo,
}

/// Resolve config key overrides into per-action bindings. Returns empty when no
/// overrides are configured, so key handling is completely untouched in the
/// common case.
pub fn build_key_bindings(keymap: &std::collections::BTreeMap<String, String>) -> Vec<KeyBinding> {
    if keymap.is_empty() {
        return Vec::new();
    }
    REMAPPABLE_ACTIONS
        .iter()
        .filter_map(|action| {
            let default = default_action_key(action)?;
            let effective = keymap
                .get(*action)
                .and_then(|s| parse_key_combo(s))
                .unwrap_or(default);
            Some(KeyBinding { default, effective })
        })
        .collect()
}

/// Translate an incoming key through the user's remap:
/// - if it's an action's *effective* key, rewrite it to that action's default
///   key so the (unchanged) key handler fires the action;
/// - if it's the default key of an action that was remapped *away*, suppress it
///   (`None`) so the old default no longer triggers the action;
/// - otherwise pass it through unchanged.
pub(crate) fn translate_key(key: KeyEvent, bindings: &[KeyBinding]) -> Option<KeyEvent> {
    let pressed: KeyCombo = (key.code, key.modifiers);
    for b in bindings {
        if b.effective == pressed {
            return Some(KeyEvent::new(b.default.0, b.default.1));
        }
    }
    for b in bindings {
        if b.default == pressed && b.effective != pressed {
            return None;
        }
    }
    Some(key)
}
