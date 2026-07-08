//! Pure parsing helpers for turning pasted/typed text into candidate file
//! paths (notably image attachments). Extracted from `tui::app` — these are
//! self-contained string/path functions with no TUI state, so they live and
//! test on their own.

use std::path::{Path, PathBuf};

pub(crate) fn strip_outer_quotes(s: &str) -> &str {
    let t = s.trim();
    if t.len() >= 2 {
        let first = t.as_bytes()[0] as char;
        let last = t.as_bytes()[t.len() - 1] as char;
        if matches!((first, last), ('\'', '\'') | ('"', '"') | ('`', '`')) {
            return &t[1..t.len() - 1];
        }
    }
    t
}

pub(crate) fn normalize_file_url_path(raw: &str) -> Option<PathBuf> {
    if !raw.starts_with("file://") {
        return None;
    }

    if let Ok(url) = url::Url::parse(raw)
        && url.scheme() == "file"
        && let Ok(path) = url.to_file_path()
    {
        return Some(path);
    }

    let decoded = urlencoding::decode(raw.strip_prefix("file://")?)
        .ok()?
        .into_owned();
    Some(PathBuf::from(decoded))
}

pub(crate) fn looks_like_windows_drive_path(raw: &str) -> bool {
    let bytes = raw.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
}

pub(crate) fn windows_drive_path_to_wsl(raw: &str) -> Option<PathBuf> {
    if !looks_like_windows_drive_path(raw) {
        return None;
    }
    let drive = raw.chars().next()?.to_ascii_lowercase();
    let rest = raw[3..].replace('\\', "/");
    Some(PathBuf::from(format!("/mnt/{drive}/{rest}")))
}

pub(crate) fn unescape_shell_path(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\'
            && let Some(next) = chars.peek().copied()
            && matches!(
                next,
                ' ' | '\'' | '"' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | '\\'
            )
        {
            out.push(next);
            chars.next();
            continue;
        }
        out.push(ch);
    }
    out
}

pub(crate) fn looks_like_image_path(path: &Path) -> bool {
    path.extension()
        .and_then(|s| s.to_str())
        .map(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "webp" | "gif"
            )
        })
        .unwrap_or(false)
}

pub(crate) fn path_looks_explicit(raw: &str, path: &Path) -> bool {
    path.is_absolute()
        || raw.contains('/')
        || raw.contains('\\')
        || raw.starts_with("./")
        || raw.starts_with("../")
}

pub(crate) fn parse_candidate_image_path(raw_line: &str) -> Option<PathBuf> {
    let raw = strip_outer_quotes(raw_line);
    if raw.is_empty() {
        return None;
    }

    // `D:\...` is already a usable absolute path on native Windows; the
    // /mnt/<drive> mapping only applies under WSL.
    let wsl_mapped = if cfg!(windows) {
        None
    } else {
        windows_drive_path_to_wsl(raw)
    };
    let mut candidate = normalize_file_url_path(raw)
        .or(wsl_mapped)
        .unwrap_or_else(|| PathBuf::from(unescape_shell_path(raw)));
    if !looks_like_image_path(&candidate) {
        return None;
    }

    let candidate_text = candidate.to_string_lossy().into_owned();
    if !path_looks_explicit(raw, &candidate) && !path_looks_explicit(&candidate_text, &candidate) {
        return None;
    }

    // Handle file:///C:/... URLs on Unix-like systems by mapping to /mnt/<drive>/...
    if cfg!(not(windows))
        && let Some(s) = candidate.to_str()
        && s.len() >= 4
        && s.starts_with('/')
        && s.as_bytes()[1].is_ascii_alphabetic()
        && s.as_bytes()[2] == b':'
        && s.as_bytes()[3] == b'/'
        && let Some(mapped) = windows_drive_path_to_wsl(&s[1..])
    {
        candidate = mapped;
    }

    Some(candidate)
}

pub(crate) fn extract_quoted_fragments(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    for quote in ['"', '\'', '`'] {
        let mut start: Option<usize> = None;
        for (idx, ch) in line.char_indices() {
            if ch == quote {
                if let Some(s) = start.take() {
                    if idx > s + 1 {
                        out.push(line[s..=idx].to_string());
                    }
                } else {
                    start = Some(idx);
                }
            }
        }
    }
    out
}

pub(crate) fn extract_embedded_path_fragments(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let image_exts = [".png", ".jpg", ".jpeg", ".webp", ".gif"];
    let trimmed = line.trim();
    if !trimmed.is_empty() {
        out.push(trimmed.to_string());
    }
    out.extend(extract_quoted_fragments(trimmed));

    let bytes = trimmed.as_bytes();
    for (idx, b) in bytes.iter().enumerate() {
        let looks_unix = *b == b'/';
        let looks_windows = if idx + 2 < bytes.len() {
            bytes[idx].is_ascii_alphabetic()
                && bytes[idx + 1] == b':'
                && matches!(bytes[idx + 2], b'\\' | b'/')
        } else {
            false
        };
        if !(looks_unix || looks_windows) {
            continue;
        }

        for ext in image_exts {
            let mut search_from = idx;
            while let Some(found) = trimmed[search_from..].find(ext) {
                let end = search_from + found + ext.len();
                if end <= idx {
                    search_from += found + ext.len();
                    continue;
                }
                let candidate = trimmed[idx..end].trim().trim_end_matches(|c: char| {
                    matches!(c, ')' | ']' | '}' | '"' | '\'' | '`' | ',' | ';' | ':')
                });
                if !candidate.is_empty() {
                    out.push(candidate.to_string());
                }
                search_from += found + ext.len();
            }
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::{extract_embedded_path_fragments, parse_candidate_image_path};

    #[test]
    fn parse_candidate_image_path_supports_quoted_and_escaped_spaces() {
        let parsed = parse_candidate_image_path("\"./assets/Screenshot\\ 2026-05-05.png\"")
            .expect("image path should parse");
        assert_eq!(
            parsed.to_string_lossy(),
            "./assets/Screenshot 2026-05-05.png"
        );
    }

    #[test]
    fn parse_candidate_image_path_supports_file_url_and_percent_decoding() {
        let parsed = parse_candidate_image_path("file:///tmp/Screenshot%202026-05-05.png")
            .expect("file URL should parse");
        assert_eq!(parsed.to_string_lossy(), "/tmp/Screenshot 2026-05-05.png");
    }

    #[test]
    fn drive_paths_stay_native_on_windows_and_map_under_wsl() {
        let parsed =
            parse_candidate_image_path(r"D:\shots\a.png").expect("drive path should parse");
        #[cfg(windows)]
        assert_eq!(parsed.to_string_lossy(), r"D:\shots\a.png");
        #[cfg(not(windows))]
        assert_eq!(parsed.to_string_lossy(), "/mnt/d/shots/a.png");
    }

    #[test]
    fn parse_candidate_image_path_ignores_non_image_text() {
        assert!(parse_candidate_image_path("just some notes").is_none());
        assert!(parse_candidate_image_path("README.md").is_none());
    }

    #[test]
    fn extract_embedded_path_fragments_finds_path_inside_sentence() {
        let line = "please inspect this image: /tmp/Screenshot 2026-05-05 125529.png thanks";
        let fragments = extract_embedded_path_fragments(line);
        assert!(fragments.iter().any(|f| {
            f == "/tmp/Screenshot 2026-05-05 125529.png"
                || f == "/tmp/Screenshot 2026-05-05 125529.png thanks"
        }));
    }
}
