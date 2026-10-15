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
}

/// Provider catalog for the connect modal. Every entry is a sign-in flow.
pub const CONNECT_CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        section: "Providers",
        kind: ProviderKind::Anthropic,
        title: "Claude",
        subtitle: "Your Claude account via Claude Code (`claude`)",
        action: ConnectAction::OAuthLogin("anthropic"),
    },
    CatalogEntry {
        section: "Providers",
        kind: ProviderKind::Copilot,
        title: "GitHub Copilot",
        subtitle: "Copilot models via GitHub device login",
        action: ConnectAction::OAuthLogin("copilot"),
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
    fn filter_matches_title_and_provider() {
        let rows = build_connect_rows("copilot");
        assert!(rows.iter().any(|r| matches!(
            r,
            ConnectRow::Provider {
                title: "GitHub Copilot",
                ..
            }
        )));
        assert!(!rows.iter().any(|r| matches!(
            r,
            ConnectRow::Provider {
                title: "Claude",
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
    fn section_headers_are_not_selectable() {
        let rows = build_connect_rows("");
        assert!(matches!(rows.first(), Some(ConnectRow::Section { .. })));
        assert_eq!(selectable_row_indices(&rows).len(), CONNECT_CATALOG.len());
    }
}
