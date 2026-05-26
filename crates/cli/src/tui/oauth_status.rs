//! OAuth/provider login status helpers for the connect modal and toolbar:
//! mapping a provider to its login slug, checking whether it is logged in, and
//! the slash command that switches to it. Extracted from `tui::app`.

use dcode_ai_common::auth::AuthStore;
use dcode_ai_common::config::ProviderKind;
use dcode_ai_common::provider_runtime::has_claude_cli;

pub(crate) fn oauth_login_provider_slug(kind: ProviderKind) -> Option<&'static str> {
    match kind {
        ProviderKind::OpenAi => Some("openai"),
        ProviderKind::Anthropic => Some("anthropic"),
        ProviderKind::Antigravity => Some("antigravity"),
        // Copilot uses the OpenAI provider surface at runtime, but auth is a distinct login flow.
        ProviderKind::OpenCodeZen | ProviderKind::OpenRouter => None,
    }
}

pub(crate) fn oauth_logged_in_for_slug(store: &AuthStore, slug: &str) -> bool {
    match slug {
        "openai" => store.openai_oauth.is_some(),
        "anthropic" => store.anthropic.is_some() || has_claude_cli(),
        "copilot" => store.copilot.is_some(),
        "antigravity" => store.antigravity.is_some(),
        "opencodezen" => store.opencodezen_oauth.is_some(),
        _ => false,
    }
}

pub(crate) fn oauth_switch_command_for_slug(slug: &str) -> Option<&'static str> {
    match slug {
        "openai" => Some("/provider codex"),
        "copilot" => Some("/provider copilot"),
        "anthropic" => Some("/provider anthropic"),
        "antigravity" => Some("/provider antigravity"),
        "opencodezen" => Some("/provider opencodezen"),
        _ => None,
    }
}
