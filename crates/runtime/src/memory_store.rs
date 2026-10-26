use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryNote {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub content: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MemoryState {
    #[serde(default)]
    pub notes: Vec<MemoryNote>,
}

/// A note `/memory tidy` would remove, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TidyRemoval {
    pub id: String,
    pub content: String,
    pub reason: String,
}

fn normalize(text: &str) -> String {
    text.to_lowercase()
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn word_set(norm: &str) -> std::collections::HashSet<&str> {
    norm.split(' ').filter(|w| w.chars().count() >= 3).collect()
}

/// Notes to drop: empty ones, exact and near duplicates, and notes whose
/// text another note already holds. The newest (or fuller) copy is kept.
pub fn tidy_plan(notes: &[MemoryNote]) -> Vec<TidyRemoval> {
    let mut order: Vec<&MemoryNote> = notes.iter().collect();
    order.sort_by_key(|n| std::cmp::Reverse(n.created_at));
    let mut kept: Vec<(String, &MemoryNote)> = Vec::new();
    let mut out = Vec::new();
    let remove = |note: &MemoryNote, reason: String| TidyRemoval {
        id: note.id.clone(),
        content: note.content.clone(),
        reason,
    };
    'notes: for note in order {
        let norm = normalize(&note.content);
        if norm.is_empty() {
            out.push(remove(note, "empty".into()));
            continue;
        }
        let words = word_set(&norm);
        for slot in kept.iter_mut() {
            let (k_norm, k_note) = (&slot.0, slot.1);
            if *k_norm == norm {
                out.push(remove(note, format!("same as {}", k_note.id)));
                continue 'notes;
            }
            if norm.len() >= 12 && k_norm.contains(&norm) {
                out.push(remove(note, format!("already in {}", k_note.id)));
                continue 'notes;
            }
            if k_norm.len() >= 12 && norm.contains(k_norm.as_str()) {
                // This older note says more: keep it instead.
                out.push(remove(k_note, format!("already in {}", note.id)));
                *slot = (norm, note);
                continue 'notes;
            }
            let k_words = word_set(k_norm);
            if words.len() >= 4 && k_words.len() >= 4 {
                let shared = words.intersection(&k_words).count();
                let union = words.union(&k_words).count();
                if shared * 10 >= union * 8 {
                    out.push(remove(note, format!("near copy of {}", k_note.id)));
                    continue 'notes;
                }
            }
        }
        kept.push((norm, note));
    }
    out
}

pub struct MemoryStore {
    path: PathBuf,
}

impl MemoryStore {
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub async fn load(&self) -> Result<MemoryState, String> {
        let raw = match tokio::fs::read_to_string(&self.path).await {
            Ok(raw) => raw,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(MemoryState::default());
            }
            Err(err) => return Err(err.to_string()),
        };
        serde_json::from_str(&raw).map_err(|err| err.to_string())
    }

    pub async fn save(&self, state: &MemoryState) -> Result<(), String> {
        if let Some(parent) = self.path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|err| err.to_string())?;
        }
        let raw = serde_json::to_string_pretty(state).map_err(|err| err.to_string())?;
        tokio::fs::write(&self.path, raw)
            .await
            .map_err(|err| err.to_string())
    }

    pub async fn append_note(
        &self,
        note: MemoryNote,
        max_notes: usize,
    ) -> Result<MemoryState, String> {
        let mut state = self.load().await?;
        state.notes.push(note);
        if state.notes.len() > max_notes {
            let drain = state.notes.len() - max_notes;
            state.notes.drain(0..drain);
        }
        self.save(&state).await?;
        Ok(state)
    }
}

#[cfg(test)]
mod tidy_tests {
    use super::*;

    fn note(id: &str, minutes: i64, content: &str) -> MemoryNote {
        MemoryNote {
            id: id.into(),
            created_at: chrono::DateTime::from_timestamp(1_800_000_000 + minutes * 60, 0)
                .expect("time"),
            kind: "note".into(),
            title: None,
            content: content.into(),
        }
    }

    #[test]
    fn tidy_drops_copies_and_keeps_the_newest_or_fuller_note() {
        let notes = vec![
            note("a", 1, "Use pnpm, not npm, in this repo."),
            note("b", 2, "use PNPM not npm in this repo"),
            note("c", 3, "Run tests with cargo nextest"),
            note(
                "d",
                4,
                "Run tests with cargo nextest; integration tests need Docker running",
            ),
            note("e", 5, "   "),
            note("f", 6, "The API server listens on port 8080 locally"),
            note("g", 7, "The API server listens locally on port 8080"),
            note("h", 8, "Deploys go through the staging branch first"),
        ];
        let plan = tidy_plan(&notes);
        let ids: Vec<&str> = plan.iter().map(|r| r.id.as_str()).collect();
        assert!(ids.contains(&"a"), "{plan:?}");
        assert!(ids.contains(&"c"), "{plan:?}");
        assert!(ids.contains(&"e"), "{plan:?}");
        assert!(ids.contains(&"f"), "{plan:?}");
        assert!(!ids.contains(&"b") && !ids.contains(&"d") && !ids.contains(&"g"));
        assert!(!ids.contains(&"h"));
    }
}
