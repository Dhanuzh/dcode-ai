/// Renders markdown content to the terminal with lightweight terminal-friendly formatting.
pub struct MarkdownRenderer;

impl MarkdownRenderer {
    pub fn new() -> Self {
        Self
    }

    /// Render a markdown string to styled terminal output.
    pub fn render(&self, markdown: &str) -> String {
        let mut out = String::new();
        let mut in_code_block = false;

        for line in markdown.lines() {
            let trimmed = line.trim_end();
            let compact = trimmed.trim_start();

            if compact.starts_with("```") {
                in_code_block = !in_code_block;
                if !out.ends_with('\n') && !out.is_empty() {
                    out.push('\n');
                }
                continue;
            }

            if in_code_block {
                out.push_str("    ");
                out.push_str(trimmed);
                out.push('\n');
                continue;
            }

            if compact.is_empty() {
                if !out.ends_with("\n\n") {
                    out.push('\n');
                }
                continue;
            }

            if let Some(rest) = compact.strip_prefix("### ") {
                out.push_str(&format!("{rest}\n{}\n\n", "-".repeat(rest.chars().count())));
                continue;
            }
            if let Some(rest) = compact.strip_prefix("## ") {
                out.push_str(&format!("{rest}\n{}\n\n", "=".repeat(rest.chars().count())));
                continue;
            }
            if let Some(rest) = compact.strip_prefix("# ") {
                out.push_str(&format!("{rest}\n{}\n\n", "=".repeat(rest.chars().count())));
                continue;
            }
            if let Some(rest) = compact.strip_prefix("> ") {
                out.push_str("│ ");
                out.push_str(rest);
                out.push('\n');
                continue;
            }
            if compact.starts_with("- ") || compact.starts_with("* ") {
                out.push_str("• ");
                out.push_str(&compact[2..]);
                out.push('\n');
                continue;
            }

            out.push_str(trimmed);
            out.push('\n');
        }

        out.trim_end().to_string()
    }
}
