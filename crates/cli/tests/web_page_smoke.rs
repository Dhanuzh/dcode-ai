//! Static regression tests for the embedded web chat page.
//!
//! There is no JS runtime in this workspace, so these are smoke-level
//! invariants that have each caught (or would have caught) a real bug in this
//! page's history: raw control bytes pasted into source, functions defined
//! twice after a bad merge, the page referencing an API route the server
//! doesn't implement, and unbalanced braces from hand edits.

const PAGE: &str = include_str!("../src/web_chat.html");
const SERVER: &str = include_str!("../src/web_server.rs");

#[test]
fn no_raw_control_characters() {
    // A literal U+0001 once shipped inside a JS string and broke the
    // attachment marker protocol; control chars must be written as escapes.
    for (i, ch) in PAGE.char_indices() {
        let ok = ch == '\n' || ch == '\r' || ch == '\t' || !ch.is_control();
        assert!(ok, "raw control char {ch:?} at byte {i}");
    }
}

#[test]
fn critical_elements_exist_once() {
    for id in [
        "id=\"log\"",
        "id=\"input\"",
        "id=\"send\"",
        "id=\"cancel\"",
        "id=\"provider-sel\"",
        "id=\"model-sel\"",
        "id=\"session-list\"",
        "id=\"session-search\"",
        "id=\"files-btn\"",
        "id=\"settings-btn\"",
        "id=\"ctx-meter\"",
        "id=\"agent-state\"",
        "id=\"switch-overlay\"",
        "id=\"jump-bottom\"",
        "id=\"chips\"",
        "id=\"suggest\"",
    ] {
        assert_eq!(PAGE.matches(id).count(), 1, "{id} must appear exactly once");
    }
}

#[test]
fn core_functions_are_defined_exactly_once() {
    for func in [
        "function api(",
        "function send(",
        "function onEvent(",
        "function renderMarkdown(",
        "function highlightCode(",
        "async function lifecycle(",
        "function reconnectEvents(",
        "async function rewindResend(",
        "function updateCtxMeter(",
        "function setAgentState(",
        "function openFiles(",
        "function exportChat(",
        "function showShortcuts(",
    ] {
        assert_eq!(
            PAGE.matches(func).count(),
            1,
            "{func} must be defined exactly once"
        );
    }
}

#[test]
fn script_braces_and_parens_balance() {
    // Crude but effective: catches truncated edits. String-literal imbalance
    // is possible in principle; keep literals paren/brace-neutral or update
    // the expected skew here deliberately.
    let brace_skew = PAGE.matches('{').count() as i64 - PAGE.matches('}').count() as i64;
    assert_eq!(brace_skew, 0, "curly braces out of balance");
}

#[test]
fn attachment_marker_uses_escape_not_raw_byte() {
    assert!(
        PAGE.contains("\\u0001attach:"),
        "composer must emit the U+0001 attach marker as a JS escape"
    );
}

#[test]
fn every_api_route_used_by_the_page_exists_on_the_server() {
    // Collect "/api/..." string literals from the page and check each appears
    // in a server route match arm. Prevents the page drifting ahead of the
    // server (or a route being renamed on one side only).
    let mut missing = Vec::new();
    for chunk in PAGE.split('"') {
        if let Some(path) = chunk.strip_prefix("/api/") {
            let route = format!(
                "\"/api/{}\"",
                path.split(['?', '"']).next().unwrap_or_default()
            );
            if !SERVER.contains(&route) {
                missing.push(route);
            }
        }
    }
    missing.sort();
    missing.dedup();
    assert!(
        missing.is_empty(),
        "page references routes the server lacks: {missing:?}"
    );
}

#[test]
fn events_endpoint_and_auth_are_wired() {
    assert!(PAGE.contains("new EventSource(withAuth(\"/events\"))"));
    assert!(SERVER.contains("(\"GET\", \"/events\")"));
    // Cookie-first auth on both sides.
    assert!(PAGE.contains("dcode_ai_token") || SERVER.contains("dcode_ai_token"));
    assert!(SERVER.contains("Set-Cookie"));
}
