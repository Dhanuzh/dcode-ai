//! Jupyter notebooks (`.ipynb`): `read_file` shows them as numbered cells
//! with their outputs, and `notebook_edit` replaces, inserts or deletes one
//! cell (Claude Code's NotebookEdit).

use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};
use serde_json::{Value, json};

use super::ToolExecutor;

/// Longest output shown per cell.
const MAX_OUTPUT_CHARS: usize = 2_000;

/// A cell's `source` (a string or a list of lines) as one string.
fn joined(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(Value::as_str).collect(),
        _ => String::new(),
    }
}

fn clip(text: &str) -> String {
    if text.chars().count() <= MAX_OUTPUT_CHARS {
        return text.to_string();
    }
    let head: String = text.chars().take(MAX_OUTPUT_CHARS).collect();
    format!("{head}\n… [output truncated]")
}

/// One output of a code cell, as text.
fn output_text(out: &Value) -> String {
    match out["output_type"].as_str().unwrap_or("") {
        "stream" => joined(&out["text"]),
        "error" => format!(
            "{}: {}",
            out["ename"].as_str().unwrap_or("Error"),
            out["evalue"].as_str().unwrap_or("")
        ),
        _ => {
            let data = &out["data"];
            if let Some(t) = data.get("text/plain") {
                joined(t)
            } else if data
                .as_object()
                .is_some_and(|d| d.keys().any(|k| k.starts_with("image/")))
            {
                "[image output]".into()
            } else {
                String::new()
            }
        }
    }
}

/// The notebook as numbered cells, for the model.
pub fn render(text: &str) -> Result<String, String> {
    let nb: Value = serde_json::from_str(text).map_err(|e| format!("not a valid notebook: {e}"))?;
    let cells = nb["cells"]
        .as_array()
        .ok_or("not a valid notebook: no cells")?;
    let lang = nb["metadata"]["kernelspec"]["language"]
        .as_str()
        .or_else(|| nb["metadata"]["language_info"]["name"].as_str())
        .unwrap_or("python");
    let mut out = format!(
        "[Jupyter notebook · {} cells · {lang}; edit cells with notebook_edit using the cell number]\n",
        cells.len()
    );
    for (i, cell) in cells.iter().enumerate() {
        let kind = cell["cell_type"].as_str().unwrap_or("code");
        out.push_str(&format!("\n[cell {i} · {kind}]\n"));
        out.push_str(joined(&cell["source"]).trim_end());
        out.push('\n');
        let outputs: Vec<String> = cell["outputs"]
            .as_array()
            .map(|o| {
                o.iter()
                    .map(output_text)
                    .filter(|t| !t.trim().is_empty())
                    .collect()
            })
            .unwrap_or_default();
        if !outputs.is_empty() {
            out.push_str("[output]\n");
            out.push_str(clip(outputs.join("\n").trim_end()).as_str());
            out.push('\n');
        }
    }
    Ok(out)
}

/// Source text in Jupyter's on-disk form: lines keeping their `\n`.
fn source_lines(text: &str) -> Value {
    Value::Array(
        text.split_inclusive('\n')
            .map(|l| Value::String(l.to_string()))
            .collect(),
    )
}

fn new_cell(kind: &str, source: &str) -> Value {
    if kind == "markdown" {
        json!({"cell_type": "markdown", "metadata": {}, "source": source_lines(source)})
    } else {
        json!({
            "cell_type": "code",
            "execution_count": null,
            "metadata": {},
            "outputs": [],
            "source": source_lines(source)
        })
    }
}

/// Apply one edit to the notebook text; returns the new text and a summary.
pub fn edit(
    text: &str,
    cell: usize,
    mode: &str,
    source: Option<&str>,
    kind: Option<&str>,
) -> Result<(String, String), String> {
    let mut nb: Value =
        serde_json::from_str(text).map_err(|e| format!("not a valid notebook: {e}"))?;
    let cells = nb["cells"]
        .as_array_mut()
        .ok_or("not a valid notebook: no cells")?;
    let summary = match mode {
        "insert" => {
            if cell > cells.len() {
                return Err(format!(
                    "cell {cell} is past the end ({} cells)",
                    cells.len()
                ));
            }
            let source = source.ok_or("insert needs new_source")?;
            cells.insert(cell, new_cell(kind.unwrap_or("code"), source));
            format!("Inserted a {} cell at {cell}", kind.unwrap_or("code"))
        }
        "delete" => {
            if cell >= cells.len() {
                return Err(format!("no cell {cell} ({} cells)", cells.len()));
            }
            cells.remove(cell);
            format!("Deleted cell {cell}")
        }
        "replace" | "" => {
            let target = cells
                .get_mut(cell)
                .ok_or_else(|| format!("no cell {cell}; use edit_mode insert to add one"))?;
            let source = source.ok_or("replace needs new_source")?;
            let current = target["cell_type"].as_str().unwrap_or("code").to_string();
            let kind = kind.unwrap_or(&current);
            if kind != current {
                *target = new_cell(kind, source);
            } else {
                target["source"] = source_lines(source);
                if kind == "code" {
                    // Old results no longer match the code.
                    target["outputs"] = json!([]);
                    target["execution_count"] = Value::Null;
                }
            }
            format!("Replaced cell {cell}")
        }
        other => {
            return Err(format!(
                "unknown edit_mode `{other}` (replace, insert, delete)"
            ));
        }
    };
    // Jupyter writes one-space indentation and a final newline.
    let mut buf = Vec::new();
    let fmt = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut ser = serde_json::Serializer::with_formatter(&mut buf, fmt);
    serde::Serialize::serialize(&nb, &mut ser).map_err(|e| e.to_string())?;
    let mut out = String::from_utf8(buf).map_err(|e| e.to_string())?;
    out.push('\n');
    Ok((out, summary))
}

pub struct NotebookEditTool {
    workspace_root: std::path::PathBuf,
}

impl NotebookEditTool {
    pub fn new(workspace_root: std::path::PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for NotebookEditTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "notebook_edit".into(),
            description: "Edit one cell of a Jupyter notebook (.ipynb). Read the notebook with \
                read_file first to see the cell numbers. edit_mode: replace (default) the cell's \
                source, insert a new cell at that position, or delete it."
                .into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Notebook path, relative to workspace root"},
                    "cell": {"type": "integer", "description": "Cell number (0-based) as read_file shows it"},
                    "new_source": {"type": "string", "description": "New cell source (replace, insert)"},
                    "cell_type": {"type": "string", "enum": ["code", "markdown"], "description": "Cell type (insert; replace to change it)"},
                    "edit_mode": {"type": "string", "enum": ["replace", "insert", "delete"]}
                },
                "required": ["path", "cell"]
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let fail = |e: String| ToolResult {
            call_id: call.id.clone(),
            success: false,
            output: String::new(),
            error: Some(e),
        };
        let path = call.input["path"].as_str().unwrap_or("");
        if !path.ends_with(".ipynb") {
            return fail("notebook_edit only edits .ipynb files; use edit_file".into());
        }
        let Some(cell) = call.input["cell"].as_u64() else {
            return fail("cell (a cell number) is required".into());
        };
        let full = self.workspace_root.join(path);
        let canonical = match dcode_ai_common::config::canonicalize_simplified(&full) {
            Ok(c) if dcode_ai_common::workspace_scope::allows(&self.workspace_root, &c) => c,
            Ok(_) => return fail("Path is outside the workspace".into()),
            Err(_) => return fail(format!("Notebook not found: {path}")),
        };
        let text = match tokio::fs::read_to_string(&canonical).await {
            Ok(t) => t,
            Err(e) => return fail(format!("Failed to read notebook: {e}")),
        };
        let result = edit(
            &text,
            cell as usize,
            call.input["edit_mode"].as_str().unwrap_or("replace"),
            call.input["new_source"].as_str(),
            call.input["cell_type"].as_str(),
        );
        match result {
            Ok((new_text, summary)) => match tokio::fs::write(&canonical, new_text).await {
                Ok(()) => ToolResult {
                    call_id: call.id.clone(),
                    success: true,
                    output: format!("{summary} in {path}"),
                    error: None,
                },
                Err(e) => fail(format!("Failed to write notebook: {e}")),
            },
            Err(e) => fail(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NB: &str = r##"{
 "cells": [
  {"cell_type": "markdown", "metadata": {}, "source": ["# Title\n", "intro"]},
  {"cell_type": "code", "execution_count": 1, "metadata": {}, "outputs": [
    {"output_type": "stream", "name": "stdout", "text": ["hi\n"]},
    {"output_type": "display_data", "data": {"image/png": "AAA"}, "metadata": {}}
  ], "source": "print('hi')"}
 ],
 "metadata": {"kernelspec": {"language": "python"}},
 "nbformat": 4,
 "nbformat_minor": 5
}"##;

    #[test]
    fn renders_numbered_cells_with_outputs() {
        let t = render(NB).unwrap();
        assert!(t.contains("2 cells · python"), "{t}");
        assert!(t.contains("[cell 0 · markdown]\n# Title\nintro"), "{t}");
        assert!(
            t.contains("[cell 1 · code]\nprint('hi')\n[output]\nhi\n\n[image output]"),
            "{t}"
        );
    }

    #[test]
    fn replace_insert_and_delete_cells() {
        let (t, s) = edit(NB, 1, "replace", Some("print('bye')\nx = 1"), None).unwrap();
        assert_eq!(s, "Replaced cell 1");
        let v: Value = serde_json::from_str(&t).unwrap();
        assert_eq!(v["cells"][1]["source"], json!(["print('bye')\n", "x = 1"]));
        assert_eq!(v["cells"][1]["outputs"], json!([]));
        assert!(v["cells"][1]["execution_count"].is_null());
        assert_eq!(v["nbformat"], 4);
        assert!(t.starts_with("{\n \"cells\""), "one-space indent");

        let (t, _) = edit(&t, 1, "insert", Some("## Notes"), Some("markdown")).unwrap();
        let v: Value = serde_json::from_str(&t).unwrap();
        assert_eq!(v["cells"].as_array().unwrap().len(), 3);
        assert_eq!(v["cells"][1]["cell_type"], "markdown");

        let (t, _) = edit(&t, 0, "delete", None, None).unwrap();
        let v: Value = serde_json::from_str(&t).unwrap();
        assert_eq!(v["cells"][0]["cell_type"], "markdown");
        assert_eq!(v["cells"].as_array().unwrap().len(), 2);

        assert!(edit(&t, 9, "replace", Some("x"), None).is_err());
    }
}
