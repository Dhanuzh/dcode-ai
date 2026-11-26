use super::*;

#[test]
fn parses_permission_aliases() {
    assert_eq!(
        parse_permission_mode("accept-edits"),
        Some(PermissionMode::AcceptEdits)
    );
    assert_eq!(
        parse_permission_mode("dontask"),
        Some(PermissionMode::DontAsk)
    );
    assert_eq!(
        parse_permission_mode("bypass_permissions"),
        Some(PermissionMode::BypassPermissions)
    );
    assert_eq!(parse_permission_mode("invalid"), None);
}

#[test]
fn parses_logout_aliases() {
    assert!(matches!(
        parse_logout_target("claude"),
        Some(LogoutTarget::Anthropic)
    ));
    assert!(matches!(
        parse_logout_target("github"),
        Some(LogoutTarget::Copilot)
    ));
    assert!(matches!(
        parse_logout_target("all"),
        Some(LogoutTarget::All)
    ));
    assert!(parse_logout_target("openai").is_none());
}
