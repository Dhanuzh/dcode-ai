use crate::code_intel::CodeIntel;
use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};

use super::ToolExecutor;

pub struct CodeIntelTool<T: CodeIntel> {
    intel: T,
}

impl<T: CodeIntel> CodeIntelTool<T> {
    pub fn new(intel: T) -> Self {
        Self { intel }
    }
}

#[async_trait::async_trait]
impl<T: CodeIntel> ToolExecutor for CodeIntelTool<T> {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "query_symbols".into(),
            description: "Code intelligence lookup. Defaults to Rust symbol definitions; can also return definition, reference, or diagnostic results.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "operation": {
                        "type": "string",
                        "enum": ["symbols", "definition", "references", "diagnostics"],
                        "description": "Lookup operation. Defaults to symbols."
                    },
                    "query": {
                        "type": "string",
                        "description": "Literal symbol name to look up, not a regex. Not required for diagnostics."
                    },
                    "glob": { "type": "string" }
                },
                "required": []
            }),
        }
    }

    async fn execute(&self, call: &ToolCall) -> ToolResult {
        let operation = call.input["operation"].as_str().unwrap_or("symbols");
        let query = call.input["query"].as_str().unwrap_or("");
        let glob = call.input["glob"].as_str();
        let result = match operation {
            "symbols" => self.intel.query_symbols(query, glob).await.map(|matches| {
                matches
                    .into_iter()
                    .map(|m| format!("{}:{}:{}", m.file.display(), m.line, m.text))
                    .collect::<Vec<_>>()
                    .join("\n")
            }),
            "definition" => self
                .intel
                .goto_definition(query, glob)
                .await
                .map(|matches| {
                    matches
                        .into_iter()
                        .map(|m| {
                            let col = m.column.map(|c| format!(":{c}")).unwrap_or_default();
                            format!("{}:{}{}:{}", m.file.display(), m.line, col, m.text)
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }),
            "references" => self
                .intel
                .find_references(query, glob)
                .await
                .map(|matches| {
                    matches
                        .into_iter()
                        .map(|m| {
                            let col = m.column.map(|c| format!(":{c}")).unwrap_or_default();
                            format!("{}:{}{}:{}", m.file.display(), m.line, col, m.text)
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }),
            "diagnostics" => self.intel.diagnostics(glob).await.map(|diagnostics| {
                diagnostics
                    .into_iter()
                    .map(|d| {
                        let col = d.column.map(|c| format!(":{c}")).unwrap_or_default();
                        format!(
                            "{}:{}{}:{:?}:{}",
                            d.file.display(),
                            d.line,
                            col,
                            d.severity,
                            d.message
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }),
            other => Err(crate::code_intel::CodeIntelError::Execution(format!(
                "unsupported query_symbols operation `{other}`"
            ))),
        };

        match result {
            Ok(output) => ToolResult {
                call_id: call.id.clone(),
                success: true,
                output,
                error: None,
            },
            Err(err) => ToolResult {
                call_id: call.id.clone(),
                success: false,
                output: String::new(),
                error: Some(err.to_string()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_intel::FastLocalCodeIntel;

    fn make_call(input: serde_json::Value) -> ToolCall {
        ToolCall {
            id: "call-1".into(),
            name: "query_symbols".into(),
            input,
        }
    }

    fn rg_available() -> bool {
        std::process::Command::new("rg")
            .arg("--version")
            .output()
            .is_ok()
    }

    #[tokio::test]
    async fn references_operation_returns_literal_locations() {
        if !rg_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "struct Widget;\nfn main() { let _ = Widget; }\n",
        )
        .unwrap();

        let tool = CodeIntelTool::new(FastLocalCodeIntel::new(dir.path()));
        let result = tool
            .execute(&make_call(serde_json::json!({
                "operation": "references",
                "query": "Widget",
                "glob": "*.rs"
            })))
            .await;

        assert!(result.success, "{result:?}");
        assert!(result.output.contains("lib.rs:1:8:struct Widget;"));
        assert!(result.output.contains("lib.rs:2:21:fn main()"));
    }

    #[tokio::test]
    async fn diagnostics_operation_does_not_require_query() {
        if !rg_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("build.log"),
            "error[E0425]: cannot find value\nwarning: unused variable\n",
        )
        .unwrap();

        let tool = CodeIntelTool::new(FastLocalCodeIntel::new(dir.path()));
        let result = tool
            .execute(&make_call(serde_json::json!({
                "operation": "diagnostics",
                "glob": "*.log"
            })))
            .await;

        assert!(result.success, "{result:?}");
        assert!(result.output.contains("Error:error[E0425]"));
        assert!(result.output.contains("Warning:warning: unused variable"));
    }

    #[tokio::test]
    async fn unsupported_operation_fails_explicitly() {
        let dir = tempfile::tempdir().unwrap();
        let tool = CodeIntelTool::new(FastLocalCodeIntel::new(dir.path()));
        let result = tool
            .execute(&make_call(serde_json::json!({
                "operation": "hover",
                "query": "Widget"
            })))
            .await;

        assert!(!result.success);
        assert!(
            result
                .error
                .unwrap()
                .contains("unsupported query_symbols operation `hover`")
        );
    }
}
