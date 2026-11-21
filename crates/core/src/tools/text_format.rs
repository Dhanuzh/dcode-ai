//! A text file's own format — byte-order mark and line endings — kept across
//! edits. The model reads and writes `\n` text; a Windows file (`\r\n`) or
//! one starting with a BOM is matched on its normalized text and written back
//! the way it was, so edits don't fail to match or silently convert the file.

use std::borrow::Cow;

const BOM: char = '\u{feff}';

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextFormat {
    pub bom: bool,
    /// Every line ending is `\r\n` (mixed files are left exactly as they are).
    pub crlf: bool,
}

impl TextFormat {
    /// The format of `raw` and its text with a leading BOM removed and, for
    /// a CRLF file, `\r\n` turned into `\n`.
    pub fn split(raw: &str) -> (Self, String) {
        let (bom, body) = match raw.strip_prefix(BOM) {
            Some(rest) => (true, rest),
            None => (false, raw),
        };
        let crlf = body.contains("\r\n") && !body.replace("\r\n", "").contains('\n');
        let text = if crlf {
            body.replace("\r\n", "\n")
        } else {
            body.to_string()
        };
        (Self { bom, crlf }, text)
    }

    /// Model-written text in the same normalized form as [`split`]'s text.
    pub fn normalize<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let text = text.strip_prefix(BOM).unwrap_or(text);
        if self.crlf && text.contains("\r\n") {
            Cow::Owned(text.replace("\r\n", "\n"))
        } else {
            Cow::Borrowed(text)
        }
    }

    /// Normalized text back in this format.
    pub fn join(&self, text: &str) -> String {
        let body = if self.crlf {
            text.replace("\r\n", "\n").replace('\n', "\r\n")
        } else {
            text.to_string()
        };
        if self.bom {
            format!("{BOM}{body}")
        } else {
            body
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_bom_and_crlf() {
        let raw = "\u{feff}a\r\nb\r\n";
        let (fmt, text) = TextFormat::split(raw);
        assert_eq!(
            fmt,
            TextFormat {
                bom: true,
                crlf: true
            }
        );
        assert_eq!(text, "a\nb\n");
        assert_eq!(fmt.join(&text), raw);
        assert_eq!(fmt.join("a\nc\n"), "\u{feff}a\r\nc\r\n");
        assert_eq!(fmt.normalize("\u{feff}a\r\nb"), "a\nb");
        // Mixed endings are left alone.
        let (fmt, text) = TextFormat::split("a\r\nb\nc");
        assert!(!fmt.crlf);
        assert_eq!(fmt.join(&text), "a\r\nb\nc");
        // Plain files are untouched, including a missing last newline.
        let (fmt, text) = TextFormat::split("x\ny");
        assert_eq!(fmt, TextFormat::default());
        assert_eq!(fmt.join(&text), "x\ny");
    }
}
