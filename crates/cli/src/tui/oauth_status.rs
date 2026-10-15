//! Provider sign-in status helpers for the connect modal and toolbar:
//! mapping a provider to its login slug, checking whether it is signed in, and
//! the slash command that switches to it.

use dcode_ai_common::auth::AuthStore;
use dcode_ai_common::provider_runtime::{has_ant_cli, has_claude_cli};

/// Cheap check (no subprocess): the Claude sign-in itself lives in Claude
/// Code / `ant`, so "available" stands in for "signed in" here.
pub(crate) fn oauth_logged_in_for_slug(store: &AuthStore, slug: &str) -> bool {
    match slug {
        "anthropic" => store.anthropic.is_some() || has_claude_cli() || has_ant_cli(),
        "copilot" => store.copilot.is_some(),
        _ => false,
    }
}

pub(crate) fn oauth_switch_command_for_slug(slug: &str) -> Option<&'static str> {
    match slug {
        "copilot" => Some("/provider copilot"),
        "anthropic" => Some("/provider anthropic"),
        _ => None,
    }
}
