use regex::escape;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout};
use tokio::time::{Duration, timeout};

#[derive(Debug, Clone, Copy)]
pub enum CodeIntelMode {
    FastLocal,
    LanguageServer,
}

#[derive(Debug, Clone)]
pub struct SymbolMatch {
    pub file: PathBuf,
    pub line: u32,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeLocation {
    pub file: PathBuf,
    pub line: u32,
    pub column: Option<u32>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeDiagnostic {
    pub file: PathBuf,
    pub line: u32,
    pub column: Option<u32>,
    pub severity: DiagnosticSeverity,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Info,
}

#[async_trait::async_trait]
pub trait CodeIntel: Send + Sync {
    async fn query_symbols(
        &self,
        query: &str,
        glob: Option<&str>,
    ) -> Result<Vec<SymbolMatch>, CodeIntelError>;

    async fn goto_definition(
        &self,
        symbol: &str,
        glob: Option<&str>,
    ) -> Result<Vec<CodeLocation>, CodeIntelError>;

    async fn find_references(
        &self,
        symbol: &str,
        glob: Option<&str>,
    ) -> Result<Vec<CodeLocation>, CodeIntelError>;

    async fn diagnostics(&self, glob: Option<&str>) -> Result<Vec<CodeDiagnostic>, CodeIntelError>;
}

pub struct FastLocalCodeIntel {
    workspace_root: PathBuf,
}

impl FastLocalCodeIntel {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
        }
    }

    fn canonical_workspace(&self) -> PathBuf {
        dcode_ai_common::config::canonicalize_simplified(&self.workspace_root)
            .unwrap_or_else(|_| self.workspace_root.clone())
    }

    async fn ripgrep_literal(
        &self,
        pattern: &str,
        glob: Option<&str>,
    ) -> Result<Vec<CodeLocation>, CodeIntelError> {
        let current_dir = self.canonical_workspace();
        let mut cmd = tokio::process::Command::new("rg");
        cmd.arg("--line-number")
            .arg("--column")
            .arg("--color=never")
            .arg("--no-heading")
            .arg("--fixed-strings")
            .arg(pattern)
            .arg(".")
            .current_dir(&current_dir);

        if let Some(glob) = glob {
            cmd.arg("--glob").arg(glob);
        } else {
            cmd.arg("--glob").arg("*.rs");
        }

        let output = cmd
            .output()
            .await
            .map_err(|err| CodeIntelError::Execution(err.to_string()))?;
        parse_rg_locations(output)
    }
}

#[async_trait::async_trait]
impl CodeIntel for FastLocalCodeIntel {
    async fn query_symbols(
        &self,
        query: &str,
        glob: Option<&str>,
    ) -> Result<Vec<SymbolMatch>, CodeIntelError> {
        let query = query.trim();
        if query.is_empty() {
            return Err(CodeIntelError::Execution(
                "query_symbols requires a non-empty literal symbol name".into(),
            ));
        }
        let escaped_query = escape(query);
        let symbol_pattern = format!(r"(fn|struct|enum|trait|impl)\s+{escaped_query}\b");
        let current_dir = self.canonical_workspace();
        let mut cmd = tokio::process::Command::new("rg");
        cmd.arg("--line-number")
            .arg("--color=never")
            .arg("--no-heading")
            .arg(&symbol_pattern)
            .arg(".")
            .current_dir(&current_dir);

        if let Some(glob) = glob {
            cmd.arg("--glob").arg(glob);
        } else {
            cmd.arg("--glob").arg("*.rs");
        }

        let output = cmd
            .output()
            .await
            .map_err(|err| CodeIntelError::Execution(err.to_string()))?;

        parse_rg_symbol_matches(output)
    }

    async fn goto_definition(
        &self,
        symbol: &str,
        glob: Option<&str>,
    ) -> Result<Vec<CodeLocation>, CodeIntelError> {
        Ok(self
            .query_symbols(symbol, glob)
            .await?
            .into_iter()
            .map(|symbol| CodeLocation {
                file: symbol.file,
                line: symbol.line,
                column: None,
                text: symbol.text,
            })
            .collect())
    }

    async fn find_references(
        &self,
        symbol: &str,
        glob: Option<&str>,
    ) -> Result<Vec<CodeLocation>, CodeIntelError> {
        let symbol = symbol.trim();
        if symbol.is_empty() {
            return Err(CodeIntelError::Execution(
                "find_references requires a non-empty literal symbol name".into(),
            ));
        }
        self.ripgrep_literal(symbol, glob).await
    }

    async fn diagnostics(&self, glob: Option<&str>) -> Result<Vec<CodeDiagnostic>, CodeIntelError> {
        let candidates = self
            .ripgrep_literal("error[", glob.or(Some("*.txt")))
            .await?
            .into_iter()
            .chain(
                self.ripgrep_literal("warning:", glob.or(Some("*.txt")))
                    .await?,
            )
            .chain(
                self.ripgrep_literal("error:", glob.or(Some("*.txt")))
                    .await?,
            );

        Ok(candidates.filter_map(diagnostic_from_location).collect())
    }
}

pub struct LanguageServerCodeIntel {
    workspace_root: PathBuf,
    command: String,
}

impl LanguageServerCodeIntel {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
            command: "rust-analyzer".into(),
        }
    }

    pub fn with_command(workspace_root: impl AsRef<Path>, command: impl Into<String>) -> Self {
        Self {
            workspace_root: workspace_root.as_ref().to_path_buf(),
            command: command.into(),
        }
    }

    async fn start_session(&self) -> Result<LspSession, CodeIntelError> {
        LspSession::start(&self.command, &self.workspace_root).await
    }

    async fn resolve_seed(
        &self,
        session: &mut LspSession,
        symbol: &str,
        glob: Option<&str>,
    ) -> Result<CodeLocation, CodeIntelError> {
        let symbols = request_workspace_symbols(session, symbol).await?;
        if let Some(location) = symbols
            .iter()
            .find(|location| location.text == symbol)
            .or_else(|| symbols.first())
            .cloned()
        {
            return Ok(location);
        }

        FastLocalCodeIntel::new(&self.workspace_root)
            .goto_definition(symbol, glob)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| {
                CodeIntelError::Execution(format!("no definition seed found for symbol `{symbol}`"))
            })
    }
}

#[async_trait::async_trait]
impl CodeIntel for LanguageServerCodeIntel {
    async fn query_symbols(
        &self,
        query: &str,
        _glob: Option<&str>,
    ) -> Result<Vec<SymbolMatch>, CodeIntelError> {
        let query = query.trim();
        if query.is_empty() {
            return Err(CodeIntelError::Execution(
                "query_symbols requires a non-empty literal symbol name".into(),
            ));
        }

        let mut session = self.start_session().await?;
        let locations = request_workspace_symbols(&mut session, query).await?;
        session.shutdown().await.ok();
        Ok(locations
            .into_iter()
            .map(|location| SymbolMatch {
                file: location.file,
                line: location.line,
                text: location.text,
            })
            .collect())
    }

    async fn goto_definition(
        &self,
        symbol: &str,
        glob: Option<&str>,
    ) -> Result<Vec<CodeLocation>, CodeIntelError> {
        let symbol = symbol.trim();
        if symbol.is_empty() {
            return Err(CodeIntelError::Execution(
                "goto_definition requires a non-empty literal symbol name".into(),
            ));
        }

        let mut session = self.start_session().await?;
        let seed = self.resolve_seed(&mut session, symbol, glob).await?;
        let position = lsp_position_for_symbol(&seed, symbol)?;
        session.did_open(&seed.file).await?;
        let response = session
            .request(
                "textDocument/definition",
                serde_json::json!({
                    "textDocument": { "uri": file_uri(&seed.file) },
                    "position": position
                }),
            )
            .await?;
        session.shutdown().await.ok();
        parse_lsp_locations(&response)
    }

    async fn find_references(
        &self,
        symbol: &str,
        glob: Option<&str>,
    ) -> Result<Vec<CodeLocation>, CodeIntelError> {
        let symbol = symbol.trim();
        if symbol.is_empty() {
            return Err(CodeIntelError::Execution(
                "find_references requires a non-empty literal symbol name".into(),
            ));
        }

        let mut session = self.start_session().await?;
        let seed = self.resolve_seed(&mut session, symbol, glob).await?;
        let position = lsp_position_for_symbol(&seed, symbol)?;
        session.did_open(&seed.file).await?;
        let response = session
            .request(
                "textDocument/references",
                serde_json::json!({
                    "textDocument": { "uri": file_uri(&seed.file) },
                    "position": position,
                    "context": { "includeDeclaration": true }
                }),
            )
            .await?;
        session.shutdown().await.ok();
        parse_lsp_locations(&response)
    }

    async fn diagnostics(&self, glob: Option<&str>) -> Result<Vec<CodeDiagnostic>, CodeIntelError> {
        let mut session = self.start_session().await?;
        let files = collect_diagnostic_files(&self.workspace_root, glob, 32)?;
        for file in &files {
            session.did_open(file).await?;
        }
        let diagnostics = session
            .collect_publish_diagnostics(Duration::from_millis(900))
            .await?;
        session.shutdown().await.ok();
        Ok(diagnostics)
    }
}

pub struct WorkspaceCodeIntel {
    lsp: LanguageServerCodeIntel,
    fast: FastLocalCodeIntel,
}

impl WorkspaceCodeIntel {
    pub fn new(workspace_root: impl AsRef<Path>) -> Self {
        Self {
            lsp: LanguageServerCodeIntel::new(workspace_root.as_ref()),
            fast: FastLocalCodeIntel::new(workspace_root),
        }
    }

    async fn with_fallback<T, FutLsp, FutFast>(
        lsp: FutLsp,
        fast: FutFast,
    ) -> Result<Vec<T>, CodeIntelError>
    where
        FutLsp: std::future::Future<Output = Result<Vec<T>, CodeIntelError>>,
        FutFast: std::future::Future<Output = Result<Vec<T>, CodeIntelError>>,
    {
        match lsp.await {
            Ok(results) => Ok(results),
            Err(CodeIntelError::Unsupported(_)) => fast.await,
            Err(CodeIntelError::Execution(err))
                if err.contains("No such file") || err.contains("not found") =>
            {
                fast.await
            }
            Err(err) => Err(err),
        }
    }
}

#[async_trait::async_trait]
impl CodeIntel for WorkspaceCodeIntel {
    async fn query_symbols(
        &self,
        query: &str,
        glob: Option<&str>,
    ) -> Result<Vec<SymbolMatch>, CodeIntelError> {
        Self::with_fallback(
            self.lsp.query_symbols(query, glob),
            self.fast.query_symbols(query, glob),
        )
        .await
    }

    async fn goto_definition(
        &self,
        symbol: &str,
        glob: Option<&str>,
    ) -> Result<Vec<CodeLocation>, CodeIntelError> {
        Self::with_fallback(
            self.lsp.goto_definition(symbol, glob),
            self.fast.goto_definition(symbol, glob),
        )
        .await
    }

    async fn find_references(
        &self,
        symbol: &str,
        glob: Option<&str>,
    ) -> Result<Vec<CodeLocation>, CodeIntelError> {
        Self::with_fallback(
            self.lsp.find_references(symbol, glob),
            self.fast.find_references(symbol, glob),
        )
        .await
    }

    async fn diagnostics(&self, glob: Option<&str>) -> Result<Vec<CodeDiagnostic>, CodeIntelError> {
        Self::with_fallback(self.lsp.diagnostics(glob), self.fast.diagnostics(glob)).await
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CodeIntelError {
    #[error("code-intel execution failed: {0}")]
    Execution(String),
    #[error("code-intel mode unsupported: {0}")]
    Unsupported(String),
}

fn validate_rg_status(output: &std::process::Output) -> Result<(), CodeIntelError> {
    let exit_code = output.status.code().unwrap_or(-1);
    if matches!(exit_code, 0 | 1) {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(CodeIntelError::Execution(if stderr.is_empty() {
        format!("ripgrep failed with exit code {exit_code}")
    } else {
        format!("ripgrep failed with exit code {exit_code}: {stderr}")
    }))
}

fn parse_rg_symbol_matches(
    output: std::process::Output,
) -> Result<Vec<SymbolMatch>, CodeIntelError> {
    validate_rg_status(&output)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut matches = Vec::new();
    for line in stdout.lines() {
        let mut parts = line.splitn(3, ':');
        let Some(file) = parts.next() else { continue };
        let Some(line_no) = parts.next() else {
            continue;
        };
        let Some(text) = parts.next() else { continue };
        matches.push(SymbolMatch {
            file: PathBuf::from(file),
            line: line_no.parse().unwrap_or(0),
            text: text.to_string(),
        });
    }
    Ok(matches)
}

fn parse_rg_locations(output: std::process::Output) -> Result<Vec<CodeLocation>, CodeIntelError> {
    validate_rg_status(&output)?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut matches = Vec::new();
    for line in stdout.lines() {
        let mut parts = line.splitn(4, ':');
        let Some(file) = parts.next() else { continue };
        let Some(line_no) = parts.next() else {
            continue;
        };
        let Some(column) = parts.next() else { continue };
        let Some(text) = parts.next() else { continue };
        matches.push(CodeLocation {
            file: PathBuf::from(file),
            line: line_no.parse().unwrap_or(0),
            column: column.parse().ok(),
            text: text.to_string(),
        });
    }
    Ok(matches)
}

fn diagnostic_from_location(location: CodeLocation) -> Option<CodeDiagnostic> {
    let lower = location.text.to_ascii_lowercase();
    let severity = if lower.contains("error[") || lower.contains("error:") {
        DiagnosticSeverity::Error
    } else if lower.contains("warning:") {
        DiagnosticSeverity::Warning
    } else {
        return None;
    };

    Some(CodeDiagnostic {
        file: location.file,
        line: location.line,
        column: location.column,
        severity,
        message: location.text.trim().to_string(),
    })
}

struct LspSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    workspace_root: PathBuf,
}

impl LspSession {
    async fn start(command: &str, workspace_root: &Path) -> Result<Self, CodeIntelError> {
        let mut child = tokio::process::Command::new(command)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| {
                CodeIntelError::Execution(format!("failed to start {command}: {err}"))
            })?;
        let stdin = child.stdin.take().ok_or_else(|| {
            CodeIntelError::Execution(format!("failed to open stdin for {command}"))
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            CodeIntelError::Execution(format!("failed to open stdout for {command}"))
        })?;
        let mut session = Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
            workspace_root: dcode_ai_common::config::canonicalize_simplified(workspace_root)
                .unwrap_or_else(|_| workspace_root.to_path_buf()),
        };
        session.initialize().await?;
        Ok(session)
    }

    async fn initialize(&mut self) -> Result<(), CodeIntelError> {
        let root_uri = file_uri(&self.workspace_root);
        self.request(
            "initialize",
            serde_json::json!({
                "processId": null,
                "rootUri": root_uri,
                "capabilities": {},
                "workspaceFolders": [{
                    "uri": root_uri,
                    "name": self.workspace_root
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("workspace")
                }]
            }),
        )
        .await?;
        self.notify("initialized", serde_json::json!({})).await
    }

    async fn request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, CodeIntelError> {
        let id = self.next_id;
        self.next_id += 1;
        self.write_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))
        .await?;

        loop {
            let message = self.read_message(Duration::from_secs(8)).await?;
            if message.get("id").and_then(|value| value.as_u64()) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                return Err(CodeIntelError::Execution(format!(
                    "language-server request `{method}` failed: {error}"
                )));
            }
            return Ok(message
                .get("result")
                .cloned()
                .unwrap_or(serde_json::Value::Null));
        }
    }

    async fn notify(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<(), CodeIntelError> {
        self.write_message(&serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        }))
        .await
    }

    async fn did_open(&mut self, path: &Path) -> Result<(), CodeIntelError> {
        let text = tokio::fs::read_to_string(path).await.map_err(|err| {
            CodeIntelError::Execution(format!("failed to read {}: {err}", path.display()))
        })?;
        self.notify(
            "textDocument/didOpen",
            serde_json::json!({
                "textDocument": {
                    "uri": file_uri(path),
                    "languageId": "rust",
                    "version": 1,
                    "text": text
                }
            }),
        )
        .await
    }

    async fn collect_publish_diagnostics(
        &mut self,
        window: Duration,
    ) -> Result<Vec<CodeDiagnostic>, CodeIntelError> {
        let mut diagnostics = Vec::new();
        loop {
            match self.read_message(window).await {
                Ok(message) => {
                    if message.get("method").and_then(|value| value.as_str())
                        == Some("textDocument/publishDiagnostics")
                    {
                        diagnostics.extend(parse_publish_diagnostics(&message));
                    }
                }
                Err(CodeIntelError::Execution(err)) if err.contains("timed out") => break,
                Err(err) => return Err(err),
            }
        }
        Ok(diagnostics)
    }

    async fn shutdown(&mut self) -> Result<(), CodeIntelError> {
        self.request("shutdown", serde_json::json!(null)).await?;
        self.notify("exit", serde_json::json!(null)).await?;
        let _ = self.child.wait().await;
        Ok(())
    }

    async fn write_message(&mut self, message: &serde_json::Value) -> Result<(), CodeIntelError> {
        let body = serde_json::to_vec(message)
            .map_err(|err| CodeIntelError::Execution(err.to_string()))?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.stdin
            .write_all(header.as_bytes())
            .await
            .map_err(|err| CodeIntelError::Execution(err.to_string()))?;
        self.stdin
            .write_all(&body)
            .await
            .map_err(|err| CodeIntelError::Execution(err.to_string()))?;
        self.stdin
            .flush()
            .await
            .map_err(|err| CodeIntelError::Execution(err.to_string()))
    }

    async fn read_message(
        &mut self,
        deadline: Duration,
    ) -> Result<serde_json::Value, CodeIntelError> {
        timeout(deadline, async {
            let mut header = Vec::new();
            let mut byte = [0_u8; 1];
            loop {
                self.stdout.read_exact(&mut byte).await.map_err(|err| {
                    CodeIntelError::Execution(format!("language-server read failed: {err}"))
                })?;
                header.push(byte[0]);
                if header.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let header = String::from_utf8_lossy(&header);
            let content_length = header
                .lines()
                .find_map(|line| {
                    line.strip_prefix("Content-Length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .ok_or_else(|| {
                    CodeIntelError::Execution(
                        "language-server response missing Content-Length".into(),
                    )
                })?;
            let mut body = vec![0_u8; content_length];
            self.stdout.read_exact(&mut body).await.map_err(|err| {
                CodeIntelError::Execution(format!("language-server body read failed: {err}"))
            })?;
            serde_json::from_slice(&body).map_err(|err| {
                CodeIntelError::Execution(format!("invalid language-server JSON response: {err}"))
            })
        })
        .await
        .map_err(|_| CodeIntelError::Execution("language-server read timed out".into()))?
    }
}

impl Drop for LspSession {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

async fn request_workspace_symbols(
    session: &mut LspSession,
    query: &str,
) -> Result<Vec<CodeLocation>, CodeIntelError> {
    let response = session
        .request("workspace/symbol", serde_json::json!({ "query": query }))
        .await?;
    parse_lsp_symbols(&response)
}

fn parse_lsp_symbols(value: &serde_json::Value) -> Result<Vec<CodeLocation>, CodeIntelError> {
    Ok(value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|symbol| {
            let name = symbol.get("name")?.as_str()?.to_string();
            let location = symbol_location_value(symbol)?;
            lsp_location_from_value(location, Some(name))
        })
        .collect())
}

fn parse_lsp_locations(value: &serde_json::Value) -> Result<Vec<CodeLocation>, CodeIntelError> {
    if value.is_null() {
        return Ok(Vec::new());
    }
    if let Some(array) = value.as_array() {
        return Ok(array
            .iter()
            .filter_map(|location| lsp_location_from_value(location, None))
            .collect());
    }
    Ok(lsp_location_from_value(value, None).into_iter().collect())
}

fn symbol_location_value(symbol: &serde_json::Value) -> Option<&serde_json::Value> {
    let location = symbol.get("location")?;
    if location.get("uri").is_some() && location.get("range").is_some() {
        Some(location)
    } else {
        Some(symbol)
    }
}

fn lsp_location_from_value(
    value: &serde_json::Value,
    text_override: Option<String>,
) -> Option<CodeLocation> {
    let uri = value
        .get("uri")
        .or_else(|| value.get("targetUri"))
        .and_then(|value| value.as_str())?;
    let range = value
        .get("range")
        .or_else(|| value.get("targetSelectionRange"))
        .or_else(|| value.get("selectionRange"))?;
    let start = range.get("start")?;
    let line = start.get("line")?.as_u64()? as u32 + 1;
    let column = start
        .get("character")
        .and_then(|value| value.as_u64())
        .map(|column| column as u32 + 1);
    let file = path_from_file_uri(uri)?;
    let text = text_override.unwrap_or_else(|| read_line_text(&file, line).unwrap_or_default());
    Some(CodeLocation {
        file,
        line,
        column,
        text,
    })
}

fn lsp_position_for_symbol(
    location: &CodeLocation,
    symbol: &str,
) -> Result<serde_json::Value, CodeIntelError> {
    let text =
        read_line_text(&location.file, location.line).unwrap_or_else(|| location.text.clone());
    let character = text
        .find(symbol)
        .map(|index| text[..index].chars().count() as u32)
        .or_else(|| location.column.map(|column| column.saturating_sub(1)))
        .ok_or_else(|| {
            CodeIntelError::Execution(format!(
                "could not resolve LSP position for `{symbol}` in {}:{}",
                location.file.display(),
                location.line
            ))
        })?;
    Ok(serde_json::json!({
        "line": location.line.saturating_sub(1),
        "character": character
    }))
}

fn parse_publish_diagnostics(message: &serde_json::Value) -> Vec<CodeDiagnostic> {
    let Some(params) = message.get("params") else {
        return Vec::new();
    };
    let Some(uri) = params
        .get("uri")
        .or_else(|| params.get("textDocument").and_then(|doc| doc.get("uri")))
        .and_then(|value| value.as_str())
    else {
        return Vec::new();
    };
    let Some(file) = path_from_file_uri(uri) else {
        return Vec::new();
    };
    params
        .get("diagnostics")
        .and_then(|value| value.as_array())
        .into_iter()
        .flatten()
        .filter_map(|diagnostic| {
            let start = diagnostic.get("range")?.get("start")?;
            let line = start.get("line")?.as_u64()? as u32 + 1;
            let column = start
                .get("character")
                .and_then(|value| value.as_u64())
                .map(|column| column as u32 + 1);
            let severity = match diagnostic.get("severity").and_then(|value| value.as_u64()) {
                Some(1) => DiagnosticSeverity::Error,
                Some(2) => DiagnosticSeverity::Warning,
                _ => DiagnosticSeverity::Info,
            };
            let message = diagnostic.get("message")?.as_str()?.to_string();
            Some(CodeDiagnostic {
                file: file.clone(),
                line,
                column,
                severity,
                message,
            })
        })
        .collect()
}

fn collect_diagnostic_files(
    workspace_root: &Path,
    glob: Option<&str>,
    limit: usize,
) -> Result<Vec<PathBuf>, CodeIntelError> {
    let mut files = Vec::new();
    collect_rust_files(
        &dcode_ai_common::config::canonicalize_simplified(workspace_root)
            .unwrap_or_else(|_| workspace_root.to_path_buf()),
        glob,
        limit,
        &mut files,
    )?;
    Ok(files)
}

fn collect_rust_files(
    dir: &Path,
    glob: Option<&str>,
    limit: usize,
    files: &mut Vec<PathBuf>,
) -> Result<(), CodeIntelError> {
    if files.len() >= limit {
        return Ok(());
    }
    let entries = std::fs::read_dir(dir).map_err(|err| {
        CodeIntelError::Execution(format!("failed to list {}: {err}", dir.display()))
    })?;
    for entry in entries {
        if files.len() >= limit {
            break;
        }
        let path = entry
            .map_err(|err| CodeIntelError::Execution(err.to_string()))?
            .path();
        if path.is_dir() {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if matches!(name, ".git" | ".dcode-ai" | "target") {
                continue;
            }
            collect_rust_files(&path, glob, limit, files)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs")
            && glob_matches(&path, glob)
        {
            files.push(path);
        }
    }
    Ok(())
}

fn glob_matches(path: &Path, glob: Option<&str>) -> bool {
    let Some(glob) = glob else {
        return true;
    };
    if glob == "*.rs" {
        return path.extension().and_then(|ext| ext.to_str()) == Some("rs");
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if let Some(suffix) = glob.strip_prefix("*.") {
        return name.ends_with(suffix);
    }
    name == glob
}

fn file_uri(path: &Path) -> String {
    format!("file://{}", path.to_string_lossy())
}

fn path_from_file_uri(uri: &str) -> Option<PathBuf> {
    let raw = uri.strip_prefix("file://")?;
    urlencoding::decode(raw)
        .ok()
        .map(|path| PathBuf::from(path.into_owned()))
}

fn read_line_text(path: &Path, line: u32) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()?
        .lines()
        .nth(line.saturating_sub(1) as usize)
        .map(|line| line.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rg_available() -> bool {
        std::process::Command::new("rg")
            .arg("--version")
            .output()
            .is_ok()
    }

    #[tokio::test]
    async fn query_symbols_treats_query_as_literal() {
        if !rg_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "fn foo_bar() {}\nfn foo() {}\n").unwrap();

        let intel = FastLocalCodeIntel::new(dir.path());
        let matches = intel.query_symbols("foo.bar", Some("*.rs")).await.unwrap();
        assert!(matches.is_empty());
    }

    #[tokio::test]
    async fn query_symbols_rejects_empty_queries() {
        let dir = tempfile::tempdir().unwrap();
        let intel = FastLocalCodeIntel::new(dir.path());
        let err = intel.query_symbols("   ", Some("*.rs")).await.unwrap_err();
        assert!(err.to_string().contains("non-empty literal symbol name"));
    }

    #[tokio::test]
    async fn goto_definition_reuses_symbol_lookup() {
        if !rg_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "struct Widget {}\n").unwrap();

        let intel = FastLocalCodeIntel::new(dir.path());
        let matches = intel.goto_definition("Widget", Some("*.rs")).await.unwrap();

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].line, 1);
        assert!(matches[0].text.contains("struct Widget"));
    }

    #[tokio::test]
    async fn find_references_uses_literal_identifier_search() {
        if !rg_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("lib.rs"),
            "fn Widget() {}\nlet a = Widget();\n",
        )
        .unwrap();

        let intel = FastLocalCodeIntel::new(dir.path());
        let matches = intel.find_references("Widget", Some("*.rs")).await.unwrap();

        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0].column, Some(4));
        assert_eq!(matches[1].column, Some(9));
    }

    #[tokio::test]
    async fn diagnostics_extracts_common_error_and_warning_lines() {
        if !rg_available() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("build.log"),
            "error[E0425]: cannot find value\nwarning: unused variable\n",
        )
        .unwrap();

        let intel = FastLocalCodeIntel::new(dir.path());
        let diagnostics = intel.diagnostics(Some("*.log")).await.unwrap();

        assert_eq!(diagnostics.len(), 2);
        assert_eq!(diagnostics[0].severity, DiagnosticSeverity::Error);
        assert_eq!(diagnostics[1].severity, DiagnosticSeverity::Warning);
    }

    #[test]
    fn parses_lsp_workspace_symbols() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lib.rs");
        std::fs::write(&file, "struct Widget;\n").unwrap();
        let symbols = parse_lsp_symbols(&serde_json::json!([{
            "name": "Widget",
            "kind": 5,
            "location": {
                "uri": file_uri(&file),
                "range": {
                    "start": { "line": 0, "character": 7 },
                    "end": { "line": 0, "character": 13 }
                }
            }
        }]))
        .unwrap();

        assert_eq!(symbols.len(), 1);
        assert_eq!(symbols[0].file, file);
        assert_eq!(symbols[0].line, 1);
        assert_eq!(symbols[0].column, Some(8));
        assert_eq!(symbols[0].text, "Widget");
    }

    #[test]
    fn parses_lsp_publish_diagnostics() {
        let file = tempfile::NamedTempFile::new().unwrap().into_temp_path();
        let diagnostics = parse_publish_diagnostics(&serde_json::json!({
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": file_uri(file.as_ref()),
                "diagnostics": [{
                    "range": {
                        "start": { "line": 2, "character": 4 },
                        "end": { "line": 2, "character": 10 }
                    },
                    "severity": 1,
                    "message": "cannot find value `x` in this scope"
                }]
            }
        }));

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].line, 3);
        assert_eq!(diagnostics[0].column, Some(5));
        assert_eq!(diagnostics[0].severity, DiagnosticSeverity::Error);
    }

    #[tokio::test]
    async fn language_server_missing_command_fails_explicitly() {
        let dir = tempfile::tempdir().unwrap();
        let intel = LanguageServerCodeIntel::with_command(
            dir.path(),
            "dcode-ai-definitely-missing-rust-analyzer",
        );

        let err = intel
            .query_symbols("Widget", Some("*.rs"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("failed to start"));
    }
}
