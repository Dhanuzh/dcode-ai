use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};

use super::ToolExecutor;
use super::freshness::FileFreshness;

pub struct ReadFileTool {
    workspace_root: std::path::PathBuf,
    freshness: FileFreshness,
}

impl ReadFileTool {
    pub fn new(workspace_root: std::path::PathBuf) -> Self {
        Self::with_freshness(workspace_root, FileFreshness::new())
    }

    pub fn with_freshness(workspace_root: std::path::PathBuf, freshness: FileFreshness) -> Self {
        Self {
            workspace_root,
            freshness,
        }
    }
}

/// A PDF's text, page by page. For PDFs `offset` / `limit` count pages.
async fn read_pdf(
    path: &std::path::Path,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<String, String> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|e| format!("Failed to read file: {e}"))?;
    // The extractor can panic on unusual PDFs; keep that off the agent.
    let pages = tokio::task::spawn_blocking(move || {
        std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem_by_pages(&bytes))
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|_| "could not read this PDF".to_string())?
    .map_err(|e| format!("could not read this PDF: {e}"))?;
    Ok(pdf_window(&pages, offset, limit))
}

/// The requested pages (1-based), or as many as fit from `offset`.
fn pdf_window(pages: &[String], offset: Option<usize>, limit: Option<usize>) -> String {
    let total = pages.len();
    let start = offset.unwrap_or(1).max(1) - 1;
    if start >= total {
        return format!(
            "[the PDF has {total} pages; page {} is past the end]",
            start + 1
        );
    }
    let mut out = format!("[PDF · {total} pages]\n");
    let mut end = start;
    while end < total && limit.is_none_or(|l| end - start < l) {
        let text = pages[end].trim();
        if limit.is_none() && end > start && out.len() + text.len() > DEFAULT_READ_CHARS {
            break;
        }
        out.push_str(&format!("\n--- page {} ---\n{text}\n", end + 1));
        end += 1;
    }
    if out.trim_end().ends_with("---") || pages[start..end].iter().all(|p| p.trim().is_empty()) {
        out.push_str("\n[no text found: the pages may be scanned images]");
    }
    if end < total {
        out.push_str(&format!(
            "\n[pages {}-{end} of {total}; read on with offset={} (pages)]",
            start + 1,
            end + 1
        ));
    }
    out
}

/// Characters returned when no range is given (the agent caps any result
/// near 24k by eliding its middle; ending cleanly lets the model read on).
const DEFAULT_READ_CHARS: usize = 20_000;

/// The requested lines of `content`, or as much as fits from the top. A
/// partial read ends with a note giving the total and the next offset.
fn read_window(content: &str, offset: Option<usize>, limit: Option<usize>) -> String {
    if offset.is_none() && limit.is_none() && content.len() <= DEFAULT_READ_CHARS {
        return content.to_string();
    }
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let start = offset.unwrap_or(1).max(1) - 1;
    if start >= total {
        return format!(
            "[the file has {total} lines; offset {} is past the end]",
            start + 1
        );
    }
    let mut out = String::new();
    let mut end = start;
    while end < total && limit.is_none_or(|l| end - start < l) {
        let line = lines[end];
        if limit.is_none() && end > start && out.len() + line.len() > DEFAULT_READ_CHARS {
            break;
        }
        out.push_str(line);
        out.push('\n');
        end += 1;
    }
    if start > 0 || end < total {
        out.push_str(&format!("\n[lines {}-{end} of {total}", start + 1));
        if end < total {
            out.push_str(&format!(
                "; read on with offset={} or jump to a place found with search_code",
                end + 1
            ));
        }
        out.push(']');
    }
    out
}

#[async_trait::async_trait]
impl ToolExecutor for ReadFileTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_file".into(),
            description: "Read a file. For big files read only the part you need: find the \
                place with search_code, then pass `offset` (first line, 1-based) and `limit` \
                (number of lines). Without a range a long file returns its beginning and says \
                how to read on. Don't re-read a file you just read. PDFs come back as text page \
                by page (offset/limit count pages); Jupyter notebooks as numbered cells with \
                outputs (change them with notebook_edit)."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the file, relative to workspace root"
                    },
                    "offset": {
                        "type": "integer",
                        "description": "First line to read, 1-based (default 1)"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Number of lines to read (default: as much as fits, about 20k characters)"
                    }
                },
                "required": ["path"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let path = call.input["path"].as_str().unwrap_or("");
        let full_path = self.workspace_root.join(path);

        // Verify the path stays inside the workspace
        match dcode_ai_common::config::canonicalize_simplified(&full_path) {
            Ok(canonical)
                if dcode_ai_common::workspace_scope::allows(&self.workspace_root, &canonical) =>
            {
                let ext = canonical
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(str::to_ascii_lowercase);
                let offset = call.input["offset"].as_u64().map(|n| n as usize);
                let limit = call.input["limit"].as_u64().map(|n| n as usize);
                if ext.as_deref() == Some("pdf") {
                    self.freshness.note(&canonical);
                    let result = read_pdf(&canonical, offset, limit).await;
                    return ToolResult {
                        call_id: call.id.clone(),
                        success: result.is_ok(),
                        output: result.clone().unwrap_or_default(),
                        error: result.err(),
                    };
                }
                match tokio::fs::read_to_string(&canonical).await {
                    Ok(content) if ext.as_deref() == Some("ipynb") => {
                        self.freshness.note(&canonical);
                        match super::notebook::render(&content) {
                            Ok(text) => ToolResult {
                                call_id: call.id.clone(),
                                success: true,
                                output: read_window(&text, offset, limit),
                                error: None,
                            },
                            // Not a notebook after all: show it as text.
                            Err(_) => ToolResult {
                                call_id: call.id.clone(),
                                success: true,
                                output: read_window(&content, offset, limit),
                                error: None,
                            },
                        }
                    }
                    Ok(content) => {
                        self.freshness.note(&canonical);
                        ToolResult {
                            call_id: call.id.clone(),
                            success: true,
                            output: read_window(&content, offset, limit),
                            error: None,
                        }
                    }
                    Err(e) => ToolResult {
                        call_id: call.id.clone(),
                        success: false,
                        output: String::new(),
                        error: Some(format!("Failed to read file: {e}")),
                    },
                }
            }
            Err(_) if !full_path.exists() => ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(format!("File not found: {path}")),
            },
            _ => ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some("Path is outside the workspace".into()),
            },
        }
    }
}

#[cfg(test)]
mod read_window_tests {
    use super::*;

    #[test]
    fn pdf_pages_are_windowed() {
        let pages: Vec<String> = (1..=4).map(|i| format!("text of page {i}")).collect();
        let out = pdf_window(&pages, Some(2), Some(2));
        assert!(out.starts_with("[PDF · 4 pages]"), "{out}");
        assert!(out.contains("--- page 2 ---\ntext of page 2"));
        assert!(out.contains("--- page 3 ---"));
        assert!(!out.contains("page 4 ---"));
        assert!(out.contains("read on with offset=4"));
        let blank = pdf_window(&[String::new()], None, None);
        assert!(blank.contains("scanned images"), "{blank}");
    }

    #[test]
    fn small_files_come_back_whole() {
        assert_eq!(read_window("a\nb\n", None, None), "a\nb\n");
    }

    #[test]
    fn a_range_returns_those_lines_with_a_footer() {
        let text: String = (1..=10).map(|i| format!("line {i}\n")).collect();
        let out = read_window(&text, Some(3), Some(2));
        assert!(out.starts_with("line 3\nline 4\n"), "{out}");
        assert!(
            out.contains("[lines 3-4 of 10; read on with offset=5"),
            "{out}"
        );
        assert!(read_window(&text, Some(20), None).contains("past the end"));
    }

    #[test]
    fn big_files_stop_near_the_budget_and_say_how_to_go_on() {
        let text: String = (1..=5000)
            .map(|i| format!("this is line number {i}\n"))
            .collect();
        let out = read_window(&text, None, None);
        assert!(out.len() < DEFAULT_READ_CHARS + 200);
        assert!(
            out.contains("of 5000; read on with offset="),
            "{}",
            &out[out.len() - 120..]
        );
    }
}
