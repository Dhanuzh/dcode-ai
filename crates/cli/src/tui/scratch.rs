//! The dashboard's scratch tab: what the agent made along the way (test
//! logs, screenshots, API responses) in `.dcode-ai/scratch/<session>/`,
//! newest first, live, with the selected file previewed.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};

use crate::tui::theme;

/// Files listed at most.
const MAX_FILES: usize = 200;
/// Bytes read for a text preview (the end of the file).
const PREVIEW_BYTES: u64 = 64 * 1024;
/// Images previewed at most this big.
const MAX_IMAGE_BYTES: u64 = 20 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Json,
    Image,
    Other,
}

impl Kind {
    fn of(path: &Path) -> Self {
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        match ext.as_str() {
            "png" | "jpg" | "jpeg" => Kind::Image,
            "json" | "jsonl" | "ndjson" => Kind::Json,
            "txt" | "log" | "md" | "out" | "err" | "csv" | "tsv" | "html" | "xml" | "yaml"
            | "yml" | "toml" | "diff" | "patch" | "sh" | "sql" | "" => Kind::Text,
            _ => Kind::Other,
        }
    }

    fn icon(self) -> &'static str {
        match self {
            Kind::Text => "≡",
            Kind::Json => "{}",
            Kind::Image => "▣",
            Kind::Other => "·",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScratchFile {
    pub path: PathBuf,
    /// Relative to the scratch folder (a sub-agent's prefixed with its id).
    pub name: String,
    pub size: u64,
    pub modified: SystemTime,
    pub kind: Kind,
}

#[derive(Debug, Default)]
pub struct Scratch {
    pub files: Vec<ScratchFile>,
    pub selected: usize,
    scanned: Option<Instant>,
    /// (path, modified, width, height) → its preview lines.
    preview: Option<(PathBuf, SystemTime, u16, u16, Vec<Line<'static>>)>,
}

fn human(n: u64) -> String {
    match n {
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.0} KB", n as f64 / 1024.0),
        n => format!("{n} B"),
    }
}

fn age(t: SystemTime) -> String {
    let s = t.elapsed().unwrap_or_default().as_secs();
    match s {
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

impl Scratch {
    /// Re-reads the folders (at most once a second).
    pub fn refresh(&mut self, dirs: &[(String, PathBuf)]) {
        if self
            .scanned
            .is_some_and(|t| t.elapsed() < Duration::from_secs(1))
        {
            return;
        }
        self.scanned = Some(Instant::now());
        let selected_path = self.files.get(self.selected).map(|f| f.path.clone());
        let mut files = Vec::new();
        for (label, dir) in dirs {
            collect(dir, dir, label, &mut files, 0);
        }
        files.sort_by_key(|f| std::cmp::Reverse(f.modified));
        files.truncate(MAX_FILES);
        // Keep the same file selected as new ones arrive on top.
        self.selected = selected_path
            .and_then(|p| files.iter().position(|f| f.path == p))
            .unwrap_or(0);
        self.files = files;
    }

    pub fn move_selection(&mut self, step: i32) {
        let n = self.files.len();
        if n == 0 {
            return;
        }
        self.selected = (self.selected as i32 + step).clamp(0, n as i32 - 1) as usize;
    }

    pub fn selected_file(&self) -> Option<&ScratchFile> {
        self.files.get(self.selected)
    }

    /// The list, `rows` high.
    pub fn list_lines(&self, width: usize, rows: usize, focused: bool) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        if self.files.is_empty() {
            out.push(Line::from(Span::styled(
                "nothing yet — the agent's logs, screenshots and API responses appear here",
                Style::default().fg(theme::muted()),
            )));
            return out;
        }
        let start = self.selected.saturating_sub(rows.saturating_sub(1));
        for (i, f) in self.files.iter().enumerate().skip(start).take(rows) {
            let sel = i == self.selected;
            let meta = format!("{} · {}", human(f.size), age(f.modified));
            let room = width.saturating_sub(meta.chars().count() + 5);
            let name: String = if f.name.chars().count() > room {
                let tail: String = f
                    .name
                    .chars()
                    .rev()
                    .take(room.saturating_sub(1))
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                format!("…{tail}")
            } else {
                f.name.clone()
            };
            let pad = width.saturating_sub(name.chars().count() + meta.chars().count() + 5);
            let style = if sel && focused {
                Style::default()
                    .fg(theme::accent())
                    .add_modifier(Modifier::BOLD)
            } else if sel {
                Style::default()
                    .fg(theme::text())
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::text())
            };
            out.push(Line::from(vec![
                Span::styled(
                    if sel { "▸ " } else { "  " },
                    Style::default().fg(theme::accent()),
                ),
                Span::styled(
                    format!("{} ", f.kind.icon()),
                    Style::default().fg(theme::muted()),
                ),
                Span::styled(name, style),
                Span::raw(" ".repeat(pad + 1)),
                Span::styled(meta, Style::default().fg(theme::muted())),
            ]));
        }
        out
    }

    /// The selected file's preview, `width × height` (cached until it
    /// changes).
    pub fn preview_lines(&mut self, width: u16, height: u16) -> Vec<Line<'static>> {
        let Some(f) = self.files.get(self.selected).cloned() else {
            return Vec::new();
        };
        if let Some((p, m, w, h, lines)) = &self.preview
            && *p == f.path
            && *m == f.modified
            && *w == width
            && *h == height
        {
            return lines.clone();
        }
        let lines = preview(&f, width, height);
        self.preview = Some((f.path.clone(), f.modified, width, height, lines.clone()));
        lines
    }
}

fn collect(root: &Path, dir: &Path, label: &str, out: &mut Vec<ScratchFile>, depth: usize) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let path = e.path();
        let Ok(meta) = e.metadata() else { continue };
        let fname = e.file_name().to_string_lossy().into_owned();
        if fname == ".gitignore" {
            continue;
        }
        if meta.is_dir() {
            if depth < 3 {
                collect(root, &path, label, out, depth + 1);
            }
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or(fname);
        let name = if label.is_empty() {
            rel
        } else {
            format!("{label}/{rel}")
        };
        out.push(ScratchFile {
            kind: Kind::of(&path),
            path,
            name,
            size: meta.len(),
            modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
        });
    }
}

fn muted(s: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(s.into(), Style::default().fg(theme::muted())))
}

/// What a file looks like in `width × height`: an image as a thumbnail,
/// JSON pretty-printed, text as its last lines.
pub fn preview(f: &ScratchFile, width: u16, height: u16) -> Vec<Line<'static>> {
    let h = height as usize;
    match f.kind {
        Kind::Image if f.size <= MAX_IMAGE_BYTES => match std::fs::read(&f.path)
            .ok()
            .and_then(|b| crate::tui::thumbnail::from_bytes(&b, width, height))
        {
            Some(lines) => lines,
            None => vec![muted("(can't read this picture)")],
        },
        Kind::Image => vec![muted("(too big to preview — o opens it)")],
        Kind::Other => vec![muted("(no preview — o opens it, Enter adds @file)")],
        Kind::Json | Kind::Text => {
            let text = read_tail(&f.path, PREVIEW_BYTES);
            let pretty = if f.kind == Kind::Json {
                serde_json::from_str::<serde_json::Value>(&text)
                    .ok()
                    .and_then(|v| serde_json::to_string_pretty(&v).ok())
            } else {
                None
            };
            let body = pretty.unwrap_or(text);
            let all: Vec<&str> = body.lines().collect();
            // JSON from the top; logs from the end (the latest output).
            let shown: Vec<&str> = if f.kind == Kind::Json {
                all.iter().take(h).copied().collect()
            } else {
                all.iter()
                    .skip(all.len().saturating_sub(h))
                    .copied()
                    .collect()
            };
            shown
                .into_iter()
                .map(|l| {
                    let l: String = l.chars().take(width as usize).collect();
                    let lower = l.to_ascii_lowercase();
                    let color = if lower.contains("error")
                        || lower.contains("failed")
                        || lower.contains("panicked")
                    {
                        theme::error()
                    } else if lower.contains("warn") {
                        theme::warn()
                    } else if lower.contains(" ok") || lower.contains("passed") {
                        theme::success()
                    } else {
                        theme::text()
                    };
                    Line::from(Span::styled(l, Style::default().fg(color)))
                })
                .collect()
        }
    }
}

fn read_tail(path: &Path, max: u64) -> String {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut f) = std::fs::File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    if len > max {
        let _ = f.seek(SeekFrom::Start(len - max));
    }
    let mut buf = Vec::new();
    let _ = f.take(max).read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf).into_owned();
    if len > max {
        // Start at a whole line.
        text.split_once('\n')
            .map(|(_, r)| r.to_string())
            .unwrap_or(text)
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_newest_first_and_previews_by_kind() {
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path().to_path_buf();
        std::fs::write(d.join(".gitignore"), "*").unwrap();
        std::fs::write(d.join("tests.log"), "line 1\nline 2\ntest result: FAILED\n").unwrap();
        std::thread::sleep(Duration::from_millis(20));
        std::fs::write(d.join("resp.json"), r#"{"ok":true,"items":[1,2]}"#).unwrap();
        std::thread::sleep(Duration::from_millis(20));
        let img = image::RgbImage::from_pixel(8, 8, image::Rgb([10, 200, 30]));
        img.save(d.join("shot.png")).unwrap();

        let mut s = Scratch::default();
        s.refresh(&[(String::new(), d.clone())]);
        let names: Vec<&str> = s.files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["shot.png", "resp.json", "tests.log"]);
        assert_eq!(s.files[0].kind, Kind::Image);
        let pic = s.preview_lines(10, 5);
        assert!(!pic.is_empty() && pic[0].spans[0].content == "▀");
        s.move_selection(1);
        let json: String = s
            .preview_lines(40, 10)
            .iter()
            .map(|l| l.spans[0].content.to_string() + "\n")
            .collect();
        assert!(json.contains("\"ok\": true"), "{json}");
        s.move_selection(1);
        let log = s.preview_lines(40, 2);
        assert_eq!(log.len(), 2);
        assert_eq!(log[1].spans[0].content, "test result: FAILED");
    }
}
