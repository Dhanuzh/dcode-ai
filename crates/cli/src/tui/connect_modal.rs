//! Connect-provider popup catalog and selection helpers.

use dcode_ai_common::config::ProviderKind;

#[derive(Debug, Clone, Copy)]
pub struct CatalogEntry {
    pub section: &'static str,
    pub kind: ProviderKind,
    pub title: &'static str,
    pub subtitle: &'static str,
    pub action: ConnectAction,
}

#[derive(Debug, Clone, Copy)]
pub enum ConnectAction {
    OAuthLogin(&'static str),
    PromptApiKey(ProviderKind),
    Submit(&'static str),
}

/// Provider catalog for the connect modal.
pub const CONNECT_CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        section: "Recommended",
        kind: ProviderKind::OpenCodeZen,
        title: "MiniMax (OpenCode Zen)",
        // OpenCode Zen is API-key only (no OAuth): sign in at opencode.ai/auth,
        // add billing, copy the key. This prompts for it and saves to config.
        subtitle: "MiniMax/Kimi/GLM — paste API key from opencode.ai/auth",
        action: ConnectAction::PromptApiKey(ProviderKind::OpenCodeZen),
    },
    CatalogEntry {
        section: "Recommended",
        kind: ProviderKind::Anthropic,
        title: "Anthropic",
        subtitle: "Claude models (OAuth)",
        action: ConnectAction::OAuthLogin("anthropic"),
    },
    CatalogEntry {
        section: "Recommended",
        kind: ProviderKind::Anthropic,
        title: "Claude CLI (local)",
        subtitle: "Use local Claude subscription via installed `claude` CLI",
        action: ConnectAction::Submit("/provider anthropic"),
    },
    CatalogEntry {
        section: "Recommended",
        kind: ProviderKind::OpenAi,
        title: "OpenAI Codex",
        subtitle: "GPT/Codex models (OAuth)",
        action: ConnectAction::OAuthLogin("openai"),
    },
    CatalogEntry {
        section: "Recommended",
        kind: ProviderKind::OpenAi,
        title: "GitHub Copilot",
        subtitle: "Copilot models (OAuth)",
        action: ConnectAction::OAuthLogin("copilot"),
    },
    CatalogEntry {
        section: "Recommended",
        kind: ProviderKind::Antigravity,
        title: "Antigravity",
        subtitle: "Gemini 3 Pro via Google Cloud Code Assist (OAuth)",
        action: ConnectAction::OAuthLogin("antigravity"),
    },
    CatalogEntry {
        section: "Recommended",
        kind: ProviderKind::Antigravity,
        title: "Google Cloud project (Vertex AI)",
        subtitle: "Your own GCP project via gcloud ADC — /login vertex <project-id>",
        action: ConnectAction::Submit("/login vertex"),
    },
    CatalogEntry {
        section: "Recommended",
        kind: ProviderKind::OpenRouter,
        title: "OpenRouter",
        subtitle: "300+ models with one API key",
        action: ConnectAction::PromptApiKey(ProviderKind::OpenRouter),
    },
    CatalogEntry {
        section: "OpenAI-compatible",
        kind: ProviderKind::OpenRouter,
        title: "Google Gemini",
        subtitle: "Use via OpenRouter (OPENROUTER_API_KEY)",
        action: ConnectAction::PromptApiKey(ProviderKind::OpenRouter),
    },
    CatalogEntry {
        section: "OpenAI-compatible",
        kind: ProviderKind::OpenRouter,
        title: "Groq",
        subtitle: "Use via OpenRouter (OPENROUTER_API_KEY)",
        action: ConnectAction::PromptApiKey(ProviderKind::OpenRouter),
    },
    CatalogEntry {
        section: "OpenAI-compatible",
        kind: ProviderKind::OpenRouter,
        title: "Grok / xAI",
        subtitle: "Use via OpenRouter (OPENROUTER_API_KEY)",
        action: ConnectAction::PromptApiKey(ProviderKind::OpenRouter),
    },
    CatalogEntry {
        section: "OpenAI-compatible",
        kind: ProviderKind::OpenRouter,
        title: "DeepSeek",
        subtitle: "Use via OpenRouter (OPENROUTER_API_KEY)",
        action: ConnectAction::PromptApiKey(ProviderKind::OpenRouter),
    },
    CatalogEntry {
        section: "OpenAI-compatible",
        kind: ProviderKind::OpenRouter,
        title: "Mistral",
        subtitle: "Use via OpenRouter (OPENROUTER_API_KEY)",
        action: ConnectAction::PromptApiKey(ProviderKind::OpenRouter),
    },
    CatalogEntry {
        section: "OpenAI-compatible",
        kind: ProviderKind::OpenRouter,
        title: "Together AI",
        subtitle: "Use via OpenRouter (OPENROUTER_API_KEY)",
        action: ConnectAction::PromptApiKey(ProviderKind::OpenRouter),
    },
    CatalogEntry {
        section: "OpenAI-compatible",
        kind: ProviderKind::OpenRouter,
        title: "Fireworks AI",
        subtitle: "Use via OpenRouter (OPENROUTER_API_KEY)",
        action: ConnectAction::PromptApiKey(ProviderKind::OpenRouter),
    },
    CatalogEntry {
        section: "Local",
        kind: ProviderKind::OpenAi,
        title: "Ollama",
        subtitle: "No key — auto-detects localhost:11434 and its models",
        action: ConnectAction::Submit("/connect ollama"),
    },
    CatalogEntry {
        section: "Local",
        kind: ProviderKind::OpenAi,
        title: "LM Studio",
        subtitle: "No key — auto-detects localhost:1234 and its models",
        action: ConnectAction::Submit("/connect lmstudio"),
    },
    CatalogEntry {
        section: "Local",
        kind: ProviderKind::OpenAi,
        title: "vLLM",
        subtitle: "No key — auto-detects localhost:8000 and its models",
        action: ConnectAction::Submit("/connect vllm"),
    },
];

#[derive(Debug, Clone)]
pub enum ConnectRow {
    Section {
        title: &'static str,
    },
    Provider {
        kind: ProviderKind,
        title: &'static str,
        subtitle: &'static str,
        action: ConnectAction,
    },
}

fn matches_filter(entry: &CatalogEntry, q: &str) -> bool {
    if q.is_empty() {
        return true;
    }
    let q = q.to_ascii_lowercase();
    entry.title.to_ascii_lowercase().contains(&q)
        || entry.subtitle.to_ascii_lowercase().contains(&q)
        || entry.section.to_ascii_lowercase().contains(&q)
        || entry.kind.display_name().to_ascii_lowercase().contains(&q)
}

/// Build rows with section headers only when the section has matched providers.
pub fn build_connect_rows(search: &str) -> Vec<ConnectRow> {
    let q = search.trim();
    let mut rows = Vec::new();
    let mut last_section: Option<&'static str> = None;
    for e in CONNECT_CATALOG.iter().filter(|e| matches_filter(e, q)) {
        if last_section != Some(e.section) {
            rows.push(ConnectRow::Section { title: e.section });
            last_section = Some(e.section);
        }
        rows.push(ConnectRow::Provider {
            kind: e.kind,
            title: e.title,
            subtitle: e.subtitle,
            action: e.action,
        });
    }
    rows
}

/// Indices into `rows` that are selectable providers (skip section headers).
pub fn selectable_row_indices(rows: &[ConnectRow]) -> Vec<usize> {
    rows.iter()
        .enumerate()
        .filter_map(|(i, row)| match row {
            ConnectRow::Provider { .. } => Some(i),
            ConnectRow::Section { .. } => None,
        })
        .collect()
}

/// Which `rows` index is highlighted given selection index among selectables only.
pub fn row_index_for_selection(rows: &[ConnectRow], selection: usize) -> Option<usize> {
    selectable_row_indices(rows).get(selection).copied()
}

pub fn clamp_selection(selection: usize, rows: &[ConnectRow]) -> usize {
    let n = selectable_row_indices(rows).len();
    if n == 0 { 0 } else { selection.min(n - 1) }
}

/// Pulsing chevron prefix for the selected row. 250ms duty cycle.
#[allow(dead_code)]
pub fn selection_pulse(elapsed_ms: u128) -> &'static str {
    const F: &[&str] = &["▶ ", "▷ ", "▶ ", "▸ "];
    F[(elapsed_ms / 220) as usize % F.len()]
}

/// Sparkle frame for the modal title. 500ms duty cycle.
#[allow(dead_code)]
pub fn title_sparkle(elapsed_ms: u128) -> &'static str {
    const F: &[&str] = &["✦", "✧", "✦", "✧"];
    F[(elapsed_ms / 500) as usize % F.len()]
}

/// Animated trailing dots for "not logged in" status; max 3 dots.
#[allow(dead_code)]
pub fn status_dots(elapsed_ms: u128) -> &'static str {
    const F: &[&str] = &["", ".", "..", "..."];
    F[(elapsed_ms / 380) as usize % F.len()]
}

pub fn provider_at_selection(
    rows: &[ConnectRow],
    selection: usize,
) -> Option<(ProviderKind, &'static str, ConnectAction)> {
    let row_index = row_index_for_selection(rows, selection)?;
    match rows.get(row_index)? {
        ConnectRow::Provider {
            kind,
            title,
            action,
            ..
        } => Some((*kind, *title, *action)),
        ConnectRow::Section { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_openai_shows_only_openai_under_popular() {
        let rows = build_connect_rows("openai");
        assert!(rows.iter().any(|r| matches!(
            r,
            ConnectRow::Provider {
                title: "OpenAI Codex",
                ..
            }
        )));
        assert!(!rows.iter().any(|r| matches!(
            r,
            ConnectRow::Provider {
                title: "Anthropic",
                ..
            }
        )));
    }

    #[test]
    fn empty_search_includes_all_catalog_entries() {
        let rows = build_connect_rows("");
        let n = selectable_row_indices(&rows).len();
        assert_eq!(n, CONNECT_CATALOG.len());
    }

    #[test]
    fn opencodezen_row_prompts_for_api_key() {
        // OpenCode Zen is API-key only (no OAuth), so its row prompts for a key.
        let rows = build_connect_rows("minimax");
        let found = rows.into_iter().find_map(|row| match row {
            ConnectRow::Provider {
                kind: ProviderKind::OpenCodeZen,
                action: ConnectAction::PromptApiKey(kind),
                ..
            } => Some(kind),
            _ => None,
        });
        assert_eq!(found, Some(ProviderKind::OpenCodeZen));
    }

    #[test]
    fn section_headers_are_not_selectable() {
        let rows = build_connect_rows("");
        assert!(matches!(rows.first(), Some(ConnectRow::Section { .. })));
        assert_eq!(selectable_row_indices(&rows).len(), CONNECT_CATALOG.len());
    }
}
