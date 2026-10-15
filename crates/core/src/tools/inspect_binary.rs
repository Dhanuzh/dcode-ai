//! `inspect_binary`: static, read-only inspection of compiled artifacts
//! (ELF, PE, Mach-O) without running them — format and architecture,
//! sections, imports/exports, printable strings, the crate list embedded by
//! `cargo auditable` and Go build info. Every finding names where it came
//! from (section, offset) so conclusions stay checkable.

use std::io::Read;
use std::path::{Path, PathBuf};

use dcode_ai_common::tool::{ToolCall, ToolDefinition, ToolResult};
use object::{Object, ObjectSection, ObjectSymbol};

use super::ToolExecutor;

/// Largest file read into memory.
const MAX_BYTES: u64 = 512 * 1024 * 1024;
const GO_MAGIC: &[u8] = b"\xff Go buildinf:";

pub struct InspectBinaryTool {
    workspace_root: PathBuf,
}

impl InspectBinaryTool {
    pub fn new(workspace_root: PathBuf) -> Self {
        Self { workspace_root }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    Summary,
    Sections,
    Symbols,
    Imports,
    Strings,
    Deps,
}

impl View {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "" | "summary" => Self::Summary,
            "sections" => Self::Sections,
            "symbols" | "exports" => Self::Symbols,
            "imports" => Self::Imports,
            "strings" => Self::Strings,
            "deps" | "dependencies" => Self::Deps,
            _ => return None,
        })
    }
}

/// Options for one inspection.
#[derive(Debug, Clone)]
pub struct Query {
    view: View,
    filter: Option<String>,
    limit: usize,
    min_len: usize,
}

impl Default for Query {
    fn default() -> Self {
        Self {
            view: View::Summary,
            filter: None,
            limit: 100,
            min_len: 6,
        }
    }
}

#[async_trait::async_trait]
impl ToolExecutor for InspectBinaryTool {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "inspect_binary".into(),
            description: "Statically inspect a compiled binary or library (ELF, PE/.exe/.dll, \
                Mach-O) without running it. view=summary (format, arch, entry point, \
                hardening hints, counts, toolchain facts), sections, symbols (exports), imports \
                (library functions it calls), strings (printable text; use filter), deps (crates \
                embedded by cargo-auditable, Go module list from build info). Use it to work out \
                how a program is built or what it depends on; results cite sections/offsets."
                .into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Binary path (workspace-relative or absolute)"},
                    "view": {"type": "string", "enum": ["summary", "sections", "symbols", "imports", "strings", "deps"]},
                    "filter": {"type": "string", "description": "Case-insensitive substring to keep (names, strings)"},
                    "limit": {"type": "integer", "description": "Max rows (default 100, max 2000)"},
                    "min_len": {"type": "integer", "description": "Shortest string for view=strings (default 6)"}
                },
                "required": ["path"]
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
        let Some(raw) = call.input["path"].as_str().filter(|p| !p.trim().is_empty()) else {
            return fail("`path` is required".into());
        };
        let view_name = call.input["view"].as_str().unwrap_or("summary");
        let Some(view) = View::parse(view_name) else {
            return fail(format!(
                "unknown view `{view_name}` (summary, sections, symbols, imports, strings, deps)"
            ));
        };
        let query = Query {
            view,
            filter: call.input["filter"]
                .as_str()
                .filter(|f| !f.is_empty())
                .map(str::to_ascii_lowercase),
            limit: call.input["limit"].as_u64().unwrap_or(100).clamp(1, 2000) as usize,
            min_len: call.input["min_len"].as_u64().unwrap_or(6).clamp(3, 200) as usize,
        };
        let path = resolve(&self.workspace_root, raw);
        let result =
            tokio::task::spawn_blocking(move || read_capped(&path).map(|b| (path, b))).await;
        let (path, bytes) = match result {
            Ok(Ok(v)) => v,
            Ok(Err(e)) => return fail(e),
            Err(e) => return fail(format!("inspect_binary failed: {e}")),
        };
        match inspect(&path.display().to_string(), &bytes, &query) {
            Ok(output) => ToolResult {
                call_id: call.id.clone(),
                success: true,
                output,
                error: None,
            },
            Err(e) => fail(e),
        }
    }
}

fn resolve(root: &Path, raw: &str) -> PathBuf {
    let p = Path::new(raw.trim());
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        root.join(p)
    }
}

fn read_capped(path: &Path) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a file", path.display()));
    }
    if meta.len() > MAX_BYTES {
        return Err(format!(
            "{} is {} MB; the limit is {} MB",
            path.display(),
            meta.len() / (1024 * 1024),
            MAX_BYTES / (1024 * 1024)
        ));
    }
    let mut buf = Vec::with_capacity(meta.len() as usize);
    std::fs::File::open(path)
        .and_then(|mut f| f.read_to_end(&mut buf))
        .map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(buf)
}

fn keep(filter: &Option<String>, s: &str) -> bool {
    filter
        .as_deref()
        .is_none_or(|f| s.to_ascii_lowercase().contains(f))
}

/// Run one view over `bytes`. Non-object files still support `strings`
/// and Go build info.
pub fn inspect(label: &str, bytes: &[u8], q: &Query) -> Result<String, String> {
    let file = object::File::parse(bytes);
    let mut out = String::new();
    match q.view {
        View::Strings => {
            let rows: Vec<(usize, String)> = printable_strings(bytes, q.min_len)
                .filter(|(_, s)| keep(&q.filter, s))
                .take(q.limit + 1)
                .collect();
            out.push_str(&format!("Strings in {label} (offset: text)\n"));
            for (off, s) in rows.iter().take(q.limit) {
                out.push_str(&format!("{off:#010x}: {}\n", clip(s, 200)));
            }
            if rows.len() > q.limit {
                out.push_str("… more (raise limit or narrow filter)\n");
            }
            if rows.is_empty() {
                out.push_str("(none)\n");
            }
            return Ok(out);
        }
        View::Deps => {
            let mut found = false;
            if let Ok(f) = &file
                && let Some(deps) = auditable_deps(f)?
            {
                found = true;
                out.push_str(&format!(
                    "Rust crates (cargo-auditable, section .dep-v0): {}\n",
                    deps.len()
                ));
                for d in deps
                    .iter()
                    .filter(|d| keep(&q.filter, &d.name))
                    .take(q.limit)
                {
                    out.push_str(&format!(
                        "  {} {}{}{}\n",
                        d.name,
                        d.version,
                        d.source
                            .as_deref()
                            .map(|s| format!(" ({s})"))
                            .unwrap_or_default(),
                        if d.root { " [root]" } else { "" }
                    ));
                }
            }
            if let Some(go) = go_build_info(bytes) {
                found = true;
                out.push_str(&format!(
                    "Go build info (offset {:#x}): {}\n",
                    go.offset, go.version
                ));
                for line in go
                    .modinfo
                    .lines()
                    .filter(|l| keep(&q.filter, l))
                    .take(q.limit)
                {
                    out.push_str(&format!("  {line}\n"));
                }
            }
            if !found {
                out.push_str("No embedded dependency data (no cargo-auditable .dep-v0 section, no Go build info).\n");
            }
            return Ok(out);
        }
        _ => {}
    }

    let f = file.map_err(|e| format!("{label}: not a recognised ELF/PE/Mach-O file ({e})"))?;
    match q.view {
        View::Summary => summary(label, bytes, &f, &mut out)?,
        View::Sections => {
            out.push_str("Sections (name, address, size):\n");
            for s in f
                .sections()
                .filter(|s| keep(&q.filter, s.name().unwrap_or("")))
                .take(q.limit)
            {
                out.push_str(&format!(
                    "  {:<24} {:#012x} {:>10}\n",
                    s.name().unwrap_or("?"),
                    s.address(),
                    s.size()
                ));
            }
        }
        View::Symbols => {
            let exports = f.exports().map_err(|e| e.to_string())?;
            let mut names: Vec<(u64, String)> = if exports.is_empty() {
                f.symbols()
                    .filter(|s| s.is_definition() && s.is_global())
                    .filter_map(|s| Some((s.address(), s.name().ok()?.to_string())))
                    .collect()
            } else {
                exports
                    .iter()
                    .map(|e| (e.address(), String::from_utf8_lossy(e.name()).into_owned()))
                    .collect()
            };
            names.retain(|(_, n)| !n.is_empty() && keep(&q.filter, n));
            names.sort();
            names.dedup();
            out.push_str(&format!("Exported/global symbols: {}\n", names.len()));
            for (addr, n) in names.iter().take(q.limit) {
                out.push_str(&format!("  {addr:#012x} {}\n", clip(n, 160)));
            }
        }
        View::Imports => {
            let imports = f.imports().map_err(|e| e.to_string())?;
            let mut rows: Vec<String> = imports
                .iter()
                .map(|i| {
                    let lib = String::from_utf8_lossy(i.library());
                    let name = String::from_utf8_lossy(i.name());
                    if lib.is_empty() {
                        name.into_owned()
                    } else {
                        format!("{lib}!{name}")
                    }
                })
                .filter(|r| keep(&q.filter, r))
                .collect();
            rows.sort();
            rows.dedup();
            out.push_str(&format!("Imports: {}\n", rows.len()));
            for r in rows.iter().take(q.limit) {
                out.push_str(&format!("  {r}\n"));
            }
        }
        View::Strings | View::Deps => unreachable!(),
    }
    Ok(out)
}

fn summary(
    label: &str,
    bytes: &[u8],
    f: &object::File<'_>,
    out: &mut String,
) -> Result<(), String> {
    use object::ObjectKind;
    let kind = match f.kind() {
        ObjectKind::Executable => "executable",
        ObjectKind::Dynamic => "shared library / PIE executable",
        ObjectKind::Relocatable => "object file",
        ObjectKind::Core => "core dump",
        _ => "unknown",
    };
    out.push_str(&format!("{label}\n"));
    out.push_str(&format!(
        "  format: {:?} · {:?} · {}-bit {} · {kind}\n",
        f.format(),
        f.architecture(),
        if f.is_64() { 64 } else { 32 },
        if f.is_little_endian() { "LE" } else { "BE" },
    ));
    out.push_str(&format!("  entry: {:#x}\n", f.entry()));
    out.push_str(&format!("  size: {} bytes\n", bytes.len()));
    let sections = f.sections().count();
    let symbols = f.symbols().count();
    let imports = f.imports().map(|i| i.len()).unwrap_or(0);
    let exports = f.exports().map(|e| e.len()).unwrap_or(0);
    out.push_str(&format!(
        "  sections: {sections} · symbols: {symbols}{} · imports: {imports} · exports: {exports}\n",
        if symbols == 0 { " (stripped)" } else { "" }
    ));
    if let Ok(Some(id)) = f.build_id() {
        out.push_str(&format!("  build id: {}\n", hex(id)));
    }
    let mut libs: Vec<String> = f
        .imports()
        .unwrap_or_default()
        .iter()
        .map(|i| String::from_utf8_lossy(i.library()).into_owned())
        .filter(|l| !l.is_empty())
        .collect();
    libs.extend(elf_needed(f));
    libs.sort();
    libs.dedup();
    if !libs.is_empty() {
        out.push_str(&format!("  libraries: {}\n", libs.join(", ")));
    }
    let has = |name: &str| f.section_by_name(name).is_some();
    let mut facts = Vec::new();
    if let Ok(Some(deps)) = auditable_deps(f) {
        facts.push(format!(
            "Rust (cargo-auditable: {} crates, view=deps)",
            deps.len()
        ));
    } else if bytes.windows(10).any(|w| w == b"rust_panic") || has(".rustc") {
        facts.push("likely Rust (panic machinery present)".into());
    }
    if let Some(go) = go_build_info(bytes) {
        facts.push(format!("Go {} (view=deps)", go.version));
    }
    if has(".debug_info") || has("__debug_info") {
        facts.push("DWARF debug info".into());
    }
    if has(".gnu_debuglink") {
        facts.push("separate debug file linked".into());
    }
    if f.format() == object::BinaryFormat::Elf {
        let pie = f.kind() == ObjectKind::Dynamic && f.entry() != 0;
        facts.push(format!("PIE: {}", if pie { "yes" } else { "no" }));
        if bytes.windows(17).any(|w| w == b"__stack_chk_fail\0") {
            facts.push("stack protector".into());
        }
    }
    if !facts.is_empty() {
        out.push_str(&format!("  notes: {}\n", facts.join(" · ")));
    }
    out.push_str("  next: view=imports | symbols | sections | strings (filter=…) | deps\n");
    Ok(())
}

/// ELF `DT_NEEDED` entries (shared libraries the loader must find), read
/// from `.dynamic` / `.dynstr`.
fn elf_needed(f: &object::File<'_>) -> Vec<String> {
    if f.format() != object::BinaryFormat::Elf {
        return Vec::new();
    }
    let (Some(dynamic), Some(dynstr)) = (
        f.section_by_name(".dynamic").and_then(|s| s.data().ok()),
        f.section_by_name(".dynstr").and_then(|s| s.data().ok()),
    ) else {
        return Vec::new();
    };
    let word = if f.is_64() { 8 } else { 4 };
    let read = |b: &[u8]| -> u64 {
        let mut v = 0u64;
        for (i, &x) in b.iter().enumerate() {
            let shift = if f.is_little_endian() {
                8 * i
            } else {
                8 * (b.len() - 1 - i)
            };
            v |= (x as u64) << shift;
        }
        v
    };
    let mut out = Vec::new();
    for entry in dynamic.chunks_exact(word * 2) {
        let (tag, val) = (read(&entry[..word]), read(&entry[word..]));
        match tag {
            0 => break, // DT_NULL
            1 => {
                // DT_NEEDED
                if let Some(rest) = dynstr.get(val as usize..) {
                    let end = rest.iter().position(|&c| c == 0).unwrap_or(rest.len());
                    out.push(String::from_utf8_lossy(&rest[..end]).into_owned());
                }
            }
            _ => {}
        }
    }
    out
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max).collect::<String>())
    }
}

/// Runs of printable ASCII at least `min_len` long, with their offsets.
fn printable_strings(bytes: &[u8], min_len: usize) -> impl Iterator<Item = (usize, String)> + '_ {
    let mut i = 0usize;
    std::iter::from_fn(move || {
        while i < bytes.len() {
            let start = i;
            while i < bytes.len() && (bytes[i] == b'\t' || (0x20..0x7f).contains(&bytes[i])) {
                i += 1;
            }
            let end = i;
            i += 1;
            if end - start >= min_len {
                return Some((
                    start,
                    String::from_utf8_lossy(&bytes[start..end]).into_owned(),
                ));
            }
        }
        None
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrateDep {
    pub name: String,
    pub version: String,
    pub source: Option<String>,
    pub root: bool,
}

/// Crates embedded by `cargo auditable` (zlib-compressed JSON in `.dep-v0`).
fn auditable_deps(f: &object::File<'_>) -> Result<Option<Vec<CrateDep>>, String> {
    let Some(section) = f.section_by_name(".dep-v0") else {
        return Ok(None);
    };
    let data = section.data().map_err(|e| e.to_string())?;
    parse_auditable(data).map(Some)
}

fn parse_auditable(compressed: &[u8]) -> Result<Vec<CrateDep>, String> {
    let mut json = String::new();
    flate2::read::ZlibDecoder::new(compressed)
        .take(8 * 1024 * 1024)
        .read_to_string(&mut json)
        .map_err(|e| format!(".dep-v0: cannot decompress ({e})"))?;
    let doc: serde_json::Value =
        serde_json::from_str(&json).map_err(|e| format!(".dep-v0: bad JSON ({e})"))?;
    Ok(doc["packages"]
        .as_array()
        .map(|pkgs| {
            pkgs.iter()
                .filter_map(|p| {
                    Some(CrateDep {
                        name: p["name"].as_str()?.to_string(),
                        version: p["version"].as_str().unwrap_or("?").to_string(),
                        source: p["source"].as_str().map(str::to_string),
                        root: p["root"].as_bool().unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoBuildInfo {
    pub offset: usize,
    pub version: String,
    pub modinfo: String,
}

fn uvarint(b: &[u8]) -> Option<(usize, usize)> {
    let mut v = 0usize;
    for (i, &byte) in b.iter().enumerate().take(10) {
        v |= ((byte & 0x7f) as usize) << (7 * i);
        if byte & 0x80 == 0 {
            return Some((v, i + 1));
        }
    }
    None
}

/// Go 1.18+ build info: header magic, then varint-prefixed version and
/// module info strings (older pointer-based layouts are not decoded).
fn go_build_info(bytes: &[u8]) -> Option<GoBuildInfo> {
    let offset = bytes.windows(GO_MAGIC.len()).position(|w| w == GO_MAGIC)?;
    let header = bytes.get(offset..offset + 32)?;
    if header[15] & 0x2 == 0 {
        return Some(GoBuildInfo {
            offset,
            version: "(pre-1.18 layout, not decoded)".into(),
            modinfo: String::new(),
        });
    }
    let mut rest = &bytes[offset + 32..];
    let mut read_str = || -> Option<String> {
        let (len, n) = uvarint(rest)?;
        let s = rest.get(n..n + len)?;
        rest = &rest[n + len..];
        Some(String::from_utf8_lossy(s).into_owned())
    };
    let version = read_str()?;
    let mut modinfo = read_str().unwrap_or_default();
    // Module info is wrapped in 16-byte sentinels.
    if modinfo.len() >= 33 && modinfo.as_bytes()[modinfo.len() - 17] == b'\n' {
        modinfo = modinfo[16..modinfo.len() - 16].to_string();
    }
    Some(GoBuildInfo {
        offset,
        version,
        modinfo,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn q(view: View) -> Query {
        Query {
            view,
            ..Query::default()
        }
    }

    #[test]
    fn summarises_a_real_executable() {
        let exe = std::env::current_exe().unwrap();
        let bytes = std::fs::read(&exe).unwrap();
        let out = inspect("self", &bytes, &q(View::Summary)).unwrap();
        assert!(out.contains("format: "), "{out}");
        assert!(out.contains("sections: "), "{out}");
        #[cfg(target_os = "linux")]
        assert!(out.contains("libc.so"), "DT_NEEDED libraries: {out}");
        let secs = inspect("self", &bytes, &q(View::Sections)).unwrap();
        assert!(secs.lines().count() > 3, "{secs}");
    }

    #[test]
    fn strings_view_filters_and_reports_offsets() {
        let bytes = b"\x00\x01hello world\x00\x02xy\x00secret_token_here\x00";
        let out = inspect(
            "blob",
            bytes,
            &Query {
                view: View::Strings,
                filter: Some("secret".into()),
                ..Query::default()
            },
        )
        .unwrap();
        assert!(out.contains("0x00000012: secret_token_here"), "{out}");
        assert!(!out.contains("hello"), "{out}");
        // Not an object file: summary is a clear error.
        assert!(inspect("blob", bytes, &q(View::Summary)).is_err());
    }

    #[test]
    fn decodes_cargo_auditable_json() {
        let json = r#"{"packages":[{"name":"app","version":"0.1.0","source":"local","root":true,"dependencies":[1]},{"name":"serde","version":"1.0.200","source":"crates.io"}]}"#;
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(json.as_bytes()).unwrap();
        let deps = parse_auditable(&enc.finish().unwrap()).unwrap();
        assert_eq!(deps.len(), 2);
        assert!(deps[0].root);
        assert_eq!(deps[1].name, "serde");
        assert_eq!(deps[1].source.as_deref(), Some("crates.io"));
    }

    #[test]
    fn decodes_go_build_info() {
        let mut blob = vec![0u8; 7];
        let off = blob.len();
        blob.extend_from_slice(GO_MAGIC);
        blob.push(8); // pointer size
        blob.push(0x2); // inline strings
        blob.resize(off + 32, 0);
        let version = b"go1.22.3";
        blob.push(version.len() as u8);
        blob.extend_from_slice(version);
        let modinfo = format!(
            "{}path\texample.com/app\nmod\texample.com/app\t(devel)\t\n{}",
            "0123456789abcdef", "fedcba9876543210"
        );
        blob.push(modinfo.len() as u8);
        blob.extend_from_slice(modinfo.as_bytes());
        let go = go_build_info(&blob).unwrap();
        assert_eq!(go.offset, off);
        assert_eq!(go.version, "go1.22.3");
        assert!(
            go.modinfo.starts_with("path\texample.com/app"),
            "{:?}",
            go.modinfo
        );
        let out = inspect("go", &blob, &q(View::Deps)).unwrap();
        assert!(
            out.contains("Go build info") && out.contains("go1.22.3"),
            "{out}"
        );
    }
}
