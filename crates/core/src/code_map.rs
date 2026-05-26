//! Repo-map ranking: a lightweight, dependency-free approximation of the
//! Aider-style "which files matter most" map. We build a reference graph over
//! source files — each file scores by how many *other* files mention its module
//! name (file stem) as an identifier — and return the highest-ranked files.
//!
//! This is a heuristic for context selection, not a parser: it does not resolve
//! imports precisely. It is fast (single pass, token sets) and good enough to
//! surface the hub files of an unfamiliar codebase.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// Directories never worth walking.
const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "node_modules",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
    ".next",
    "vendor",
];

/// Source extensions we map.
const CODE_EXTS: &[&str] = &[
    "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "rb", "c", "h", "cpp", "hpp", "cs", "php",
    "swift", "kt", "scala",
];

/// Stems too generic to be meaningful reference targets.
const GENERIC_STEMS: &[&str] = &[
    "mod", "lib", "main", "index", "init", "test", "tests", "types", "utils", "util", "common",
    "config", "app",
];

/// Hard cap on files scanned, to bound work on huge trees.
const MAX_SCAN: usize = 5_000;

/// PageRank damping factor and iteration count (standard values).
const PAGERANK_DAMPING: f64 = 0.85;
const PAGERANK_ITERATIONS: usize = 40;

/// A file and its computed importance.
#[derive(Debug, Clone, PartialEq)]
pub struct RankedFile {
    /// Workspace-relative path.
    pub path: String,
    /// Number of distinct other files that reference this file's module name.
    pub score: usize,
    /// PageRank importance — weights a reference by the importance of the file
    /// making it, so a file used by central files ranks above one used by leaves
    /// even at equal raw counts. Primary sort key.
    pub rank: f64,
}

/// Build a ranked repo map for `root`, returning at most `max_files` entries
/// sorted by PageRank (desc), then path (asc) for stability.
pub fn build_repo_map(root: &Path, max_files: usize) -> Vec<RankedFile> {
    let files = collect_source_files(root);
    let n = files.len();
    if n == 0 {
        return Vec::new();
    }

    // Map module stem -> file indices defining it. A stem may map to several
    // files (e.g. many `index.ts`); generic stems are excluded as ref targets.
    let index_of: BTreeMap<&str, usize> = files
        .iter()
        .enumerate()
        .map(|(i, p)| (p.as_str(), i))
        .collect();
    let mut stem_to_idxs: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, rel) in files.iter().enumerate() {
        if let Some(stem) = module_stem(rel) {
            stem_to_idxs.entry(stem).or_default().push(i);
        }
    }

    // Directed edges: referrer -> referenced (distinct targets), plus inbound
    // reference counts (the displayed `score`).
    let mut out_edges: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut inbound: Vec<usize> = vec![0; n];
    for (i, rel) in files.iter().enumerate() {
        let full = root.join(rel);
        let Ok(content) = std::fs::read_to_string(&full) else {
            continue;
        };
        let mut targets: Vec<usize> = Vec::new();
        for stem in identifier_tokens(&content) {
            if GENERIC_STEMS.contains(&stem.as_str()) {
                continue;
            }
            if let Some(idxs) = stem_to_idxs.get(&stem) {
                for &t in idxs {
                    if t != i && !targets.contains(&t) {
                        targets.push(t);
                        inbound[t] += 1;
                    }
                }
            }
        }
        out_edges[i] = targets;
    }

    let rank = pagerank(&out_edges, n);

    let mut ranked: Vec<RankedFile> = files
        .iter()
        .enumerate()
        .map(|(i, path)| RankedFile {
            path: path.clone(),
            score: inbound[i],
            rank: rank[i],
        })
        .collect();
    // Sort by PageRank desc, then path asc for determinism.
    ranked.sort_by(|a, b| {
        b.rank
            .partial_cmp(&a.rank)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.path.cmp(&b.path))
    });
    ranked.truncate(max_files);
    let _ = index_of; // reserved for future symbol-level edges
    ranked
}

/// Iterative PageRank over a directed graph given as out-adjacency lists.
/// Dangling nodes (no out-edges) redistribute their mass uniformly.
fn pagerank(out_edges: &[Vec<usize>], n: usize) -> Vec<f64> {
    let d = PAGERANK_DAMPING;
    let base = (1.0 - d) / n as f64;
    let mut rank = vec![1.0 / n as f64; n];
    for _ in 0..PAGERANK_ITERATIONS {
        let dangling: f64 = (0..n)
            .filter(|&i| out_edges[i].is_empty())
            .map(|i| rank[i])
            .sum();
        let mut next = vec![base + d * dangling / n as f64; n];
        for (i, outs) in out_edges.iter().enumerate() {
            if outs.is_empty() {
                continue;
            }
            let share = d * rank[i] / outs.len() as f64;
            for &t in outs {
                next[t] += share;
            }
        }
        rank = next;
    }
    rank
}

/// The module-name stem used as a reference key (lowercased file stem).
fn module_stem(rel_path: &str) -> Option<String> {
    let stem = Path::new(rel_path).file_stem()?.to_str()?.to_lowercase();
    if stem.len() < 3 {
        return None;
    }
    Some(stem)
}

/// Split source text into the set of identifier-like tokens (lowercased).
fn identifier_tokens(content: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    let mut cur = String::new();
    for ch in content.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            cur.push(ch.to_ascii_lowercase());
        } else if !cur.is_empty() {
            if cur.len() >= 3 {
                out.insert(std::mem::take(&mut cur));
            } else {
                cur.clear();
            }
        }
    }
    if cur.len() >= 3 {
        out.insert(cur);
    }
    out
}

/// Walk `root` collecting workspace-relative paths of source files, skipping
/// ignored directories and hidden entries. Bounded by [`MAX_SCAN`].
fn collect_source_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if out.len() >= MAX_SCAN {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                if SKIP_DIRS.contains(&name.as_ref()) {
                    continue;
                }
                stack.push(path);
            } else if file_type.is_file()
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|ext| CODE_EXTS.contains(&ext))
                && let Ok(rel) = path.strip_prefix(root)
                && let Some(rel) = rel.to_str()
            {
                out.push(rel.replace('\\', "/"));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn hub_file_outranks_leaf() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("src")).unwrap();
        // `parser` is referenced by two other files; `leaf` by none.
        fs::write(root.join("src/parser.rs"), "pub fn parse() {}").unwrap();
        fs::write(root.join("src/leaf.rs"), "pub fn helper() {}").unwrap();
        fs::write(
            root.join("src/a.rs"),
            "use crate::parser; fn x(){ parser::parse(); }",
        )
        .unwrap();
        fs::write(
            root.join("src/b.rs"),
            "use crate::parser; fn y(){ parser::parse(); }",
        )
        .unwrap();

        let map = build_repo_map(root, 10);
        let parser = map.iter().find(|f| f.path.ends_with("parser.rs")).unwrap();
        let leaf = map.iter().find(|f| f.path.ends_with("leaf.rs")).unwrap();
        assert!(
            parser.score > leaf.score,
            "parser {} should outrank leaf {}",
            parser.score,
            leaf.score
        );
        assert_eq!(parser.score, 2);
    }

    #[test]
    fn skips_ignored_dirs_and_non_source() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("target/junk.rs"), "fn junk(){}").unwrap();
        fs::write(root.join("README.md"), "# docs").unwrap();
        fs::write(root.join("real.rs"), "fn real(){}").unwrap();

        let map = build_repo_map(root, 10);
        assert!(map.iter().any(|f| f.path == "real.rs"));
        assert!(!map.iter().any(|f| f.path.contains("target")));
        assert!(!map.iter().any(|f| f.path.ends_with(".md")));
    }

    #[test]
    fn pagerank_rewards_references_from_important_files() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        // `hub` is referenced by three leaves → high rank.
        for leaf in ["aaa", "bbb", "ccc"] {
            fs::write(root.join(format!("{leaf}.rs")), "fn x() { hubfile() }").unwrap();
        }
        fs::write(root.join("hubfile.rs"), "fn h() { corefile() }").unwrap();
        fs::write(root.join("corefile.rs"), "fn c() {}").unwrap();
        // `lonely` is referenced once, by an otherwise-unreferenced file.
        fs::write(root.join("isolated.rs"), "fn i() { lonelyfile() }").unwrap();
        fs::write(root.join("lonelyfile.rs"), "fn l() {}").unwrap();

        let map = build_repo_map(root, 50);
        let core = map.iter().find(|f| f.path == "corefile.rs").unwrap();
        let lonely = map.iter().find(|f| f.path == "lonelyfile.rs").unwrap();
        // Equal raw inbound counts...
        assert_eq!(core.score, 1);
        assert_eq!(lonely.score, 1);
        // ...but core is referenced by the high-rank hub, so it ranks higher.
        assert!(
            core.rank > lonely.rank,
            "core rank {} should exceed lonely rank {}",
            core.rank,
            lonely.rank
        );
    }

    #[test]
    fn respects_max_files() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        for i in 0..10 {
            fs::write(root.join(format!("file{i}.rs")), "fn a(){}").unwrap();
        }
        assert_eq!(build_repo_map(root, 3).len(), 3);
    }
}
