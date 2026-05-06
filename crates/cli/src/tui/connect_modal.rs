//! OpenCode-style "Connect a provider" list (search + sections).
//!
//! Only providers that `dcode-ai` can actually use are listed; layout mirrors common
//! "Popular / Other" grouping from tools like OpenCode.

use dcode_ai_common::config::ProviderKind;

#[derive(Debug, Clone, Copy)]
pub struct CatalogEntry {
    pub kind: ProviderKind,
    pub title: &'static str,
    pub subtitle: &'static str,
    pub oauth_login_slug: Option<&'static str>,
}

/// Provider catalog for the connect modal.
pub const CONNECT_CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        kind: ProviderKind::Antigravity,
        title: "Antigravity",
        subtitle: "Gemini/Claude/GPT-OSS (OAuth)",
        oauth_login_slug: Some("antigravity"),
    },
    CatalogEntry {
        kind: ProviderKind::OpenAi,
        title: "OpenAI Codex",
        subtitle: "GPT models via OpenAI OAuth",
        oauth_login_slug: Some("openai"),
    },
    CatalogEntry {
        kind: ProviderKind::OpenAi,
        title: "Copilot",
        subtitle: "GitHub Copilot (OAuth)",
        oauth_login_slug: Some("copilot"),
    },
    CatalogEntry {
        kind: ProviderKind::Anthropic,
        title: "Anthropic",
        subtitle: "Claude (connect/login)",
        oauth_login_slug: Some("anthropic"),
    },
    CatalogEntry {
        kind: ProviderKind::OpenCodeZen,
        title: "MiniMax (OpenCode Zen)",
        subtitle: "MiniMax M2.5, Kimi, GLM",
        oauth_login_slug: Some("opencodezen"),
    },
    CatalogEntry {
        kind: ProviderKind::OpenRouter,
        title: "OpenRouter",
        subtitle: "Multi-model routing (connect/login)",
        oauth_login_slug: None,
    },
];

#[derive(Debug, Clone)]
pub enum ConnectRow {
    Provider {
        kind: ProviderKind,
        title: &'static str,
        subtitle: &'static str,
        oauth_login_slug: Option<&'static str>,
    },
}

fn matches_filter(entry: &CatalogEntry, q: &str) -> bool {
    if q.is_empty() {
        return true;
    }
    let q = q.to_ascii_lowercase();
    entry.title.to_ascii_lowercase().contains(&q)
        || entry.subtitle.to_ascii_lowercase().contains(&q)
        || entry.kind.display_name().to_ascii_lowercase().contains(&q)
}

/// Build flat rows: section headers (only when section has matches) + provider lines.
pub fn build_connect_rows(search: &str) -> Vec<ConnectRow> {
    let q = search.trim();
    CONNECT_CATALOG
        .iter()
        .filter(|e| matches_filter(e, q))
        .map(|e| ConnectRow::Provider {
            kind: e.kind,
            title: e.title,
            subtitle: e.subtitle,
            oauth_login_slug: e.oauth_login_slug,
        })
        .collect()
}

/// Indices into `rows` that are selectable providers (skip section headers).
pub fn selectable_row_indices(rows: &[ConnectRow]) -> Vec<usize> {
    (0..rows.len()).collect()
}

/// Which `rows` index is highlighted given selection index among selectables only.
pub fn row_index_for_selection(rows: &[ConnectRow], selection: usize) -> Option<usize> {
    rows.get(selection).map(|_| selection)
}

pub fn clamp_selection(selection: usize, rows: &[ConnectRow]) -> usize {
    let n = rows.len();
    if n == 0 { 0 } else { selection.min(n - 1) }
}

/// Pulsing chevron prefix for the selected row. 250ms duty cycle.
pub fn selection_pulse(elapsed_ms: u128) -> &'static str {
    const F: &[&str] = &["▶ ", "▷ ", "▶ ", "▸ "];
    F[(elapsed_ms / 220) as usize % F.len()]
}

/// Sparkle frame for the modal title. 500ms duty cycle.
pub fn title_sparkle(elapsed_ms: u128) -> &'static str {
    const F: &[&str] = &["✦", "✧", "✦", "✧"];
    F[(elapsed_ms / 500) as usize % F.len()]
}

/// Animated trailing dots for "not logged in" status; max 3 dots.
pub fn status_dots(elapsed_ms: u128) -> &'static str {
    const F: &[&str] = &["", ".", "..", "..."];
    F[(elapsed_ms / 380) as usize % F.len()]
}

pub fn provider_at_selection(
    rows: &[ConnectRow],
    selection: usize,
) -> Option<(ProviderKind, &'static str, Option<&'static str>)> {
    match rows.get(selection)? {
        ConnectRow::Provider {
            kind,
            title,
            oauth_login_slug,
            ..
        } => Some((*kind, *title, *oauth_login_slug)),
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
    fn empty_search_includes_all_providers() {
        let rows = build_connect_rows("");
        let n = selectable_row_indices(&rows).len();
        assert_eq!(n, CONNECT_CATALOG.len());
    }

    #[test]
    fn opencodezen_row_uses_oauth_slug() {
        let rows = build_connect_rows("opencode");
        let found = rows.into_iter().find_map(|row| match row {
            ConnectRow::Provider {
                title: "OpenCode Zen",
                oauth_login_slug,
                ..
            } => oauth_login_slug,
            _ => None,
        });
        assert_eq!(found, Some("opencodezen"));
    }
}
