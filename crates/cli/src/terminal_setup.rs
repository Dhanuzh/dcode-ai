//! `/terminal-setup`: make Shift+Enter insert a newline in the composer.
//!
//! Terminals that speak the kitty keyboard protocol (kitty, Ghostty, foot,
//! WezTerm with it enabled) already report Shift+Enter, and dcode-ai turns
//! the protocol on. Elsewhere the terminal sends a plain Enter, so we bind
//! Shift+Enter to send `\` + Enter, which the composer reads as a newline.

use std::path::{Path, PathBuf};
use std::process::Command;

/// What Shift+Enter should send: `\` then Enter.
const SEQUENCE_JSON: &str = r#""\\\r""#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Terminal {
    /// VS Code or a fork; the name is its settings folder.
    VsCode(&'static str),
    WinTerm,
    Alacritty,
    WezTerm,
    /// Already reports Shift+Enter.
    Native(&'static str),
    Other,
}

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_default()
}

fn detect() -> Terminal {
    let program = env("TERM_PROGRAM").to_ascii_lowercase();
    if program == "vscode" {
        // Forks set TERM_PROGRAM=vscode too; their install paths give them away.
        let hints = format!(
            "{} {} {}",
            env("VSCODE_GIT_ASKPASS_NODE"),
            env("VSCODE_IPC_HOOK_CLI"),
            env("VSCODE_GIT_ASKPASS_MAIN")
        )
        .to_ascii_lowercase();
        let app = if std::env::var_os("CURSOR_TRACE_ID").is_some() || hints.contains("cursor") {
            "Cursor"
        } else if hints.contains("windsurf") {
            "Windsurf"
        } else if hints.contains("insiders") {
            "Code - Insiders"
        } else if hints.contains("vscodium") {
            "VSCodium"
        } else {
            "Code"
        };
        return Terminal::VsCode(app);
    }
    if crate::tui::terminal::supports_kitty_keyboard() {
        return match program.as_str() {
            "wezterm" => Terminal::WezTerm,
            "ghostty" => Terminal::Native("Ghostty"),
            _ if env("TERM").starts_with("foot") => Terminal::Native("foot"),
            _ => Terminal::Native("kitty"),
        };
    }
    if std::env::var_os("ALACRITTY_SOCKET").is_some() || env("TERM") == "alacritty" {
        return Terminal::Alacritty;
    }
    if std::env::var_os("WT_SESSION").is_some() {
        return Terminal::WinTerm;
    }
    Terminal::Other
}

fn is_wsl() -> bool {
    std::env::var_os("WSL_DISTRO_NAME").is_some()
}

/// A Windows environment variable as a path this process can open (from
/// WSL via `cmd.exe` + `wslpath`).
fn windows_dir(var: &str) -> Option<PathBuf> {
    if cfg!(windows) {
        return std::env::var_os(var).map(PathBuf::from);
    }
    if !is_wsl() {
        return None;
    }
    let out = Command::new("cmd.exe")
        .args(["/c", &format!("echo %{var}%")])
        .current_dir("/mnt/c")
        .output()
        .ok()?;
    let win = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if win.is_empty() || win.contains('%') {
        return None;
    }
    let out = Command::new("wslpath").args(["-u", &win]).output().ok()?;
    let p = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!p.is_empty()).then(|| PathBuf::from(p))
}

fn vscode_keybindings(app: &str) -> Option<PathBuf> {
    // From WSL the editor runs on Windows, so its settings live there.
    if cfg!(windows) || is_wsl() {
        return windows_dir("APPDATA").map(|d| d.join(app).join("User").join("keybindings.json"));
    }
    let home = dcode_ai_common::config::home_dir()?;
    Some(if cfg!(target_os = "macos") {
        home.join("Library/Application Support")
            .join(app)
            .join("User/keybindings.json")
    } else {
        home.join(".config").join(app).join("User/keybindings.json")
    })
}

fn windows_terminal_settings() -> Option<PathBuf> {
    let local = windows_dir("LOCALAPPDATA")?;
    [
        "Packages/Microsoft.WindowsTerminal_8wekyb3d8bbwe/LocalState/settings.json",
        "Packages/Microsoft.WindowsTerminalPreview_8wekyb3d8bbwe/LocalState/settings.json",
        "Microsoft/Windows Terminal/settings.json",
    ]
    .iter()
    .map(|p| local.join(p))
    .find(|p| p.exists())
}

fn alacritty_config() -> Option<PathBuf> {
    if cfg!(windows) {
        return windows_dir("APPDATA").map(|d| d.join("alacritty/alacritty.toml"));
    }
    let home = dcode_ai_common::config::home_dir()?;
    let xdg = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    Some(xdg.join("alacritty/alacritty.toml"))
}

/// Remove `//` and `/* */` comments outside strings (JSONC → JSON-ish), to
/// look at a settings file's shape.
fn strip_json_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_str = false;
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match (c, chars.peek()) {
            ('"', _) => {
                in_str = true;
                out.push(c);
            }
            ('/', Some('/')) => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            ('/', Some('*')) => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    out
}

fn has_shift_enter(text: &str) -> bool {
    let t = strip_json_comments(text).to_ascii_lowercase();
    t.contains("\"shift+enter\"")
}

/// Add the VS Code binding to a `keybindings.json` (a JSON array, comments
/// allowed). `None` when it is already there.
fn add_vscode_binding(existing: Option<&str>) -> Result<Option<String>, String> {
    let entry = format!(
        "{{\n    \"key\": \"shift+enter\",\n    \"command\": \"workbench.action.terminal.sendSequence\",\n    \"args\": {{ \"text\": {SEQUENCE_JSON} }},\n    \"when\": \"terminalFocus\"\n  }}"
    );
    let Some(text) = existing.filter(|t| !t.trim().is_empty()) else {
        return Ok(Some(format!("[\n  {entry}\n]\n")));
    };
    if has_shift_enter(text) {
        return Ok(None);
    }
    let close = text
        .rfind(']')
        .ok_or("keybindings.json is not a JSON array")?;
    let body = strip_json_comments(&text[..close]);
    let inner = body
        .rfind('[')
        .map(|open| body[open + 1..].trim())
        .ok_or("keybindings.json is not a JSON array")?;
    // The comma goes on its own line: the last entry may end in a comment.
    let sep = if inner.is_empty() || inner.ends_with(',') {
        ""
    } else {
        "\n  ,"
    };
    let head = text[..close].trim_end();
    Ok(Some(format!("{head}{sep}\n  {entry}\n{}", &text[close..])))
}

/// Add a Shift+Enter action to Windows Terminal's `settings.json`.
fn add_windows_terminal_binding(text: &str) -> Result<Option<String>, String> {
    if has_shift_enter(text) {
        return Ok(None);
    }
    let key = text
        .find("\"actions\"")
        .ok_or("no \"actions\" list in settings.json")?;
    let open = text[key..]
        .find('[')
        .map(|i| key + i)
        .ok_or("no \"actions\" list in settings.json")?;
    let entry = format!(
        "\n        {{ \"command\": {{ \"action\": \"sendInput\", \"input\": {SEQUENCE_JSON} }}, \"keys\": \"shift+enter\" }},"
    );
    Ok(Some(format!(
        "{}{entry}{}",
        &text[..=open],
        &text[open + 1..]
    )))
}

/// Append a `[[keyboard.bindings]]` entry to `alacritty.toml`.
fn add_alacritty_binding(existing: Option<&str>) -> Result<Option<String>, String> {
    let text = existing.unwrap_or("");
    let lower = text.to_ascii_lowercase();
    if lower.contains("\"return\"") && lower.contains("\"shift\"") {
        return Ok(None);
    }
    if text.contains("bindings = [") || text.contains("bindings=[") {
        return Err("alacritty.toml lists bindings inline; add the entry by hand".into());
    }
    let mut out = text.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(
        "\n# Added by dcode-ai /terminal-setup: Shift+Enter inserts a newline.\n\
         [[keyboard.bindings]]\nkey = \"Return\"\nmods = \"Shift\"\nchars = \"\\\\\\r\"\n",
    );
    Ok(Some(out))
}

/// Write `content` to `path`, keeping a `.bak` copy of what was there.
fn write_with_backup(path: &Path, content: &str) -> Result<(), String> {
    if path.exists() {
        let bak = path.with_extension(format!(
            "{}.bak",
            path.extension().and_then(|e| e.to_str()).unwrap_or("")
        ));
        std::fs::copy(path, &bak).map_err(|e| format!("backup {}: {e}", bak.display()))?;
    } else if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::fs::write(path, content).map_err(|e| format!("{}: {e}", path.display()))
}

/// Apply an edit to a config file and say what happened.
fn edit_file(
    what: &str,
    path: Option<PathBuf>,
    edit: impl FnOnce(Option<&str>) -> Result<Option<String>, String>,
    manual: &str,
) -> Vec<String> {
    let Some(path) = path else {
        return vec![
            format!("Couldn't find the {what} settings file."),
            manual.to_string(),
        ];
    };
    let existing = std::fs::read_to_string(&path).ok();
    match edit(existing.as_deref()) {
        Ok(None) => vec![format!(
            "Shift+Enter is already set up in {what} ({}).",
            path.display()
        )],
        Ok(Some(content)) => match write_with_backup(&path, &content) {
            Ok(()) => vec![
                format!("Added a Shift+Enter binding to {}.", path.display()),
                format!(
                    "Shift+Enter now inserts a newline in {what}'s terminal (restart it if needed)."
                ),
            ],
            Err(e) => vec![
                format!("Couldn't write {what} settings: {e}"),
                manual.to_string(),
            ],
        },
        Err(e) => vec![
            format!("Couldn't update {}: {e}", path.display()),
            manual.to_string(),
        ],
    }
}

/// Run `/terminal-setup`; returns the lines to show.
pub fn run() -> Vec<String> {
    let fallback = "In any terminal, \\ then Enter (or Ctrl+J) also inserts a newline.";
    let mut lines = match detect() {
        Terminal::VsCode(app) => edit_file(
            app,
            vscode_keybindings(app),
            add_vscode_binding,
            "Add to keybindings.json: { \"key\": \"shift+enter\", \"command\": \
             \"workbench.action.terminal.sendSequence\", \"args\": { \"text\": \"\\\\\\r\" }, \
             \"when\": \"terminalFocus\" }",
        ),
        Terminal::WinTerm if !cfg!(windows) => {
            let path = windows_terminal_settings();
            let manual = "In Windows Terminal settings (JSON), add to \"actions\": { \"command\": \
                          { \"action\": \"sendInput\", \"input\": \"\\\\\\r\" }, \"keys\": \"shift+enter\" }";
            match path {
                Some(p) => {
                    let text = std::fs::read_to_string(&p).unwrap_or_default();
                    match add_windows_terminal_binding(&text) {
                        Ok(None) => vec![format!(
                            "Shift+Enter is already set up in Windows Terminal ({}).",
                            p.display()
                        )],
                        Ok(Some(content)) => match write_with_backup(&p, &content) {
                            Ok(()) => vec![
                                format!("Added a Shift+Enter action to {}.", p.display()),
                                "Windows Terminal reloads it at once.".into(),
                            ],
                            Err(e) => vec![e, manual.into()],
                        },
                        Err(e) => vec![
                            format!("Couldn't update {}: {e}", p.display()),
                            manual.into(),
                        ],
                    }
                }
                None => vec![
                    "Couldn't find Windows Terminal's settings.json.".into(),
                    manual.into(),
                ],
            }
        }
        // Native Windows console input reports Shift+Enter by itself.
        Terminal::WinTerm => vec!["Shift+Enter already inserts a newline here.".into()],
        Terminal::Alacritty => edit_file(
            "Alacritty",
            alacritty_config(),
            add_alacritty_binding,
            "Add to alacritty.toml: [[keyboard.bindings]] key = \"Return\" mods = \"Shift\" chars = \"\\\\\\r\"",
        ),
        Terminal::WezTerm => vec![
            "WezTerm reports Shift+Enter once its kitty keyboard support is on.".into(),
            "Add `config.enable_kitty_keyboard = true` to ~/.wezterm.lua, then restart WezTerm."
                .into(),
        ],
        Terminal::Native(name) => vec![format!(
            "{name} already reports Shift+Enter — nothing to set up."
        )],
        Terminal::Other => vec![
            "This terminal can't be set up automatically (supported: VS Code, Cursor, Windsurf, \
             Windows Terminal, Alacritty, WezTerm; kitty, Ghostty and foot work as is)."
                .into(),
        ],
    };
    lines.push(fallback.into());
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vscode_binding_goes_into_new_empty_and_commented_files() {
        let fresh = add_vscode_binding(None).unwrap().unwrap();
        assert!(fresh.trim_start().starts_with('['));
        assert!(fresh.contains(r#""text": "\\\r""#), "{fresh}");

        let empty = add_vscode_binding(Some("// Place your key bindings here\n[\n]\n"))
            .unwrap()
            .unwrap();
        assert!(!empty.contains("[,"), "{empty}");
        assert!(empty.contains("shift+enter"));

        let existing = "[\n  { \"key\": \"ctrl+k\", \"command\": \"x\" } // mine\n]\n";
        let out = add_vscode_binding(Some(existing)).unwrap().unwrap();
        let json = strip_json_comments(&out);
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid json");
        assert_eq!(parsed.as_array().map(Vec::len), Some(2));
        assert_eq!(parsed[1]["args"]["text"], "\\\r");

        assert_eq!(add_vscode_binding(Some(&out)).unwrap(), None);
    }

    #[test]
    fn windows_terminal_action_is_added_once() {
        let settings =
            "{\n  \"actions\": [\n    { \"command\": \"copy\", \"keys\": \"ctrl+c\" }\n  ]\n}";
        let out = add_windows_terminal_binding(settings).unwrap().unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&out).expect("valid json");
        assert_eq!(parsed["actions"][0]["keys"], "shift+enter");
        assert_eq!(parsed["actions"][0]["command"]["input"], "\\\r");
        assert_eq!(add_windows_terminal_binding(&out).unwrap(), None);
    }

    #[test]
    fn alacritty_binding_is_appended_as_toml() {
        let out = add_alacritty_binding(Some("[font]\nsize = 12"))
            .unwrap()
            .unwrap();
        let parsed: toml::Value = toml::from_str(&out).expect("valid toml");
        let b = &parsed["keyboard"]["bindings"][0];
        assert_eq!(b["key"].as_str(), Some("Return"));
        assert_eq!(b["chars"].as_str(), Some("\\\r"));
        assert_eq!(add_alacritty_binding(Some(&out)).unwrap(), None);
        assert!(add_alacritty_binding(Some("[keyboard]\nbindings = [\n]")).is_err());
    }
}
