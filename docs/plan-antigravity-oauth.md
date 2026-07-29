# Plan: Add Antigravity OAuth Provider

## Overview
Add a new `AntigravityOAuth` provider alongside the existing `Antigravity` provider. The existing provider stays untouched.

## Key Differences from Existing Antigravity

| Aspect | Existing `Antigravity` | New `AntigravityOAuth` |
|--------|----------------------|----------------------|
| Kind | `ProviderKind::Antigravity` | `ProviderKind::AntigravityOAuth` |
| Auth storage | `auth.antigravity` | `auth.antigravity_oauth` |
| OAuth flow | Google Cloud Code Assist | Google OAuth (Gemini API) |
| API endpoint | Cloud Code Assist v1internal | `generativelanguage.googleapis.com` |
| Scopes | cloud-platform, userinfo, cclog | generativelanguage (AI Studio) |
| Client ID | Bundled via `DCODE_ANTIGRAVITY_CLIENT_ID` | Bundled via `DCODE_ANTIGRAVITY_OAUTH_CLIENT_ID` |
| Default project | `rising-fact-p41fc` | None (AI Studio doesn't need one) |

## Files to Change

### 1. `crates/common/src/auth.rs`
- Add `AntigravityOAuthAuth` struct (access_token, refresh_token, expires_at)
- Add `pub antigravity_oauth: Option<AntigravityOAuthAuth>` to `AuthStore`
- Add `LoggedProvider::AntigravityOAuth` variant

### 2. `crates/common/src/config.rs`
- Add `ProviderKind::AntigravityOAuth` variant
- Add to `ProviderKind::ALL` array
- Implement `Display`, `from_str` for the new variant
- Add `base_url_for` support

### 3. `crates/common/src/secrets.rs`
- Add `antigravity_oauth_client_id()` function
- Add `antigravity_oauth_client_secret()` function

### 4. `crates/common/src/model_caps.rs`
- Add `ProviderKind::AntigravityOAuth` to the OpenAI-compatible branch

### 5. `crates/core/src/provider/antigravity_oauth.rs` (NEW)
- New provider struct `AntigravityOAuthProvider`
- Uses `generativelanguage.googleapis.com` endpoint
- Gemini API format (similar to OpenAI-compatible but with Gemini specifics)
- Token refresh via `refresh_antigravity_oauth_access_token`

### 6. `crates/core/src/provider/mod.rs`
- Add `pub mod antigravity_oauth;`

### 7. `crates/core/src/provider/factory.rs`
- Import `AntigravityOAuthProvider`
- Add match arm: `ProviderKind::AntigravityOAuth => Ok(Box::new(AntigravityOAuthProvider::from_config(config)?))`

### 8. `crates/cli/src/oauth_login.rs`
- Add `OAuthProvider::AntigravityOAuth` variant
- Add `LogoutTarget::AntigravityOAuth` variant
- Implement `login_antigravity_oauth()` function
- Update `show_auth_status()` to include new provider
- Update `logout_with_output()` for new provider

### 9. `crates/cli/src/tui/oauth_status.rs`
- Add `ProviderKind::AntigravityOAuth` slug mapping

### 10. `crates/cli/src/tui/connect_modal.rs`
- Add new provider row to connect modal

### 11. `crates/cli/src/web_server.rs`
- Add `build_info()` support for new provider

## OAuth Flow for Antigravity OAuth

1. **Login**: Browser → Google Accounts → Authorization code → Localhost callback
2. **Token Exchange**: Code → `oauth2.googleapis.com/token` → access_token + refresh_token
3. **API Calls**: Bearer token → `generativelanguage.googleapis.com/v1beta/models/{model}:streamGenerateContent`
4. **Token Refresh**: refresh_token → new access_token (when expired)

## Scopes
```
https://www.googleapis.com/auth/generative-language
https://www.googleapis.com/auth/userinfo.email
```

## Default Model
`gemini-2.5-flash` (or configurable via `provider.antigravity_oauth.model`)

## Implementation Order
1. Common types (auth, config, secrets) — no behavior change
2. Provider implementation — core logic
3. Factory wiring — enables the provider
4. CLI login/logout — user-facing
5. TUI/web integration — UI display
