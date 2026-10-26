//! Small numbered decisions drawn in place of the composer once the agent is
//! idle: merging a sub-agent's branch, applying `/memory tidy`.

use std::path::PathBuf;

/// What a [`ChoicePrompt`] decides; the chosen option index goes with it.
#[derive(Debug, Clone, PartialEq)]
pub enum ChoiceAction {
    /// Options: merge · keep the branch · discard.
    SubagentChanges {
        child_session_id: String,
        task: String,
        worktree_path: PathBuf,
        base_branch: String,
    },
    /// Options: remove the listed notes · cancel.
    MemoryTidy {
        /// (memory file, note id) pairs to remove.
        remove: Vec<(PathBuf, String)>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChoicePrompt {
    pub title: String,
    pub body: Vec<String>,
    pub options: Vec<String>,
    pub index: usize,
    /// Option chosen by Esc.
    pub esc_index: usize,
    /// Extra keys shown under the options (e.g. "d view diff").
    pub hint: Option<String>,
    pub action: ChoiceAction,
}

/// A key for the active choice prompt (crossterm-decoupled).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceKey {
    Up,
    Down,
    Accept,
    Cancel,
    Char(char),
}

/// What a key did to the prompt.
#[derive(Debug, Clone, PartialEq)]
pub enum ChoiceOutcome {
    Stay,
    /// Option `index` was chosen; the prompt is done.
    Chosen(ChoiceAction, usize),
    /// A prompt-specific key (like `d`) the caller handles.
    Key(char),
}

impl ChoicePrompt {
    #[allow(clippy::too_many_arguments)]
    pub fn subagent_changes(
        child_session_id: String,
        task: String,
        branch: String,
        base_branch: String,
        worktree_path: PathBuf,
        files: &[String],
        insertions: usize,
        deletions: usize,
    ) -> Self {
        let task_line: String = task.lines().next().unwrap_or("").chars().take(70).collect();
        let mut body = vec![
            format!("\"{task_line}\""),
            format!(
                "{branch} · {} file{} +{insertions} -{deletions}",
                files.len(),
                if files.len() == 1 { "" } else { "s" }
            ),
        ];
        const SHOWN: usize = 6;
        body.extend(files.iter().take(SHOWN).map(|f| format!("  {f}")));
        if files.len() > SHOWN {
            body.push(format!("  … {} more", files.len() - SHOWN));
        }
        Self {
            title: "Sub-agent changes".into(),
            body,
            options: vec![
                format!("Merge into {base_branch}"),
                "Keep the branch for later".into(),
                "Discard the changes".into(),
            ],
            index: 0,
            esc_index: 1,
            hint: Some("d view diff · Esc keep for later".into()),
            action: ChoiceAction::SubagentChanges {
                child_session_id,
                task,
                worktree_path,
                base_branch,
            },
        }
    }

    pub fn handle_key(&mut self, key: ChoiceKey) -> ChoiceOutcome {
        let last = self.options.len().saturating_sub(1);
        match key {
            ChoiceKey::Up => self.index = self.index.saturating_sub(1),
            ChoiceKey::Down => self.index = (self.index + 1).min(last),
            ChoiceKey::Accept => return ChoiceOutcome::Chosen(self.action.clone(), self.index),
            ChoiceKey::Cancel => {
                return ChoiceOutcome::Chosen(self.action.clone(), self.esc_index.min(last));
            }
            ChoiceKey::Char(c) => {
                if let Some(n) = c.to_digit(10).map(|d| d as usize)
                    && (1..=self.options.len()).contains(&n)
                {
                    return ChoiceOutcome::Chosen(self.action.clone(), n - 1);
                }
                match c {
                    'k' => self.index = self.index.saturating_sub(1),
                    'j' => self.index = (self.index + 1).min(last),
                    c => return ChoiceOutcome::Key(c),
                }
            }
        }
        ChoiceOutcome::Stay
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn prompt() -> ChoicePrompt {
        ChoicePrompt::subagent_changes(
            "child-1".into(),
            "add retries".into(),
            "dcode-ai/child-1".into(),
            "main".into(),
            PathBuf::from("/w"),
            &["M src/a.rs".into(), "A src/b.rs".into()],
            12,
            3,
        )
    }

    #[test]
    fn subagent_prompt_summarises_the_branch() {
        let p = prompt();
        assert_eq!(p.options[0], "Merge into main");
        assert!(p.body[1].contains("2 files +12 -3"), "{:?}", p.body);
        assert!(p.body.iter().any(|l| l.contains("A src/b.rs")));
    }

    #[test]
    fn keys_pick_options_and_esc_keeps_the_branch() {
        let mut p = prompt();
        assert_eq!(p.handle_key(ChoiceKey::Down), ChoiceOutcome::Stay);
        assert!(matches!(
            p.handle_key(ChoiceKey::Accept),
            ChoiceOutcome::Chosen(_, 1)
        ));
        assert!(matches!(
            p.handle_key(ChoiceKey::Char('3')),
            ChoiceOutcome::Chosen(_, 2)
        ));
        assert!(matches!(
            p.handle_key(ChoiceKey::Cancel),
            ChoiceOutcome::Chosen(_, 1)
        ));
        assert_eq!(p.handle_key(ChoiceKey::Char('d')), ChoiceOutcome::Key('d'));
    }
}
