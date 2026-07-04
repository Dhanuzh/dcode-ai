/// Antigravity (Google) OAuth desktop-app credentials.
///
/// Desktop-app client secrets are *public by design* — they ship embedded in
/// every copy of the app. However, GitHub secret-scanning flags them, so we
/// keep the values out of source code and load them from environment variables
/// (set via `.env` or the build environment).
///
/// Override at runtime via `DCODE_ANTIGRAVITY_CLIENT_ID` /
/// `DCODE_ANTIGRAVITY_CLIENT_SECRET`.

pub fn antigravity_client_id() -> String {
    std::env::var("DCODE_ANTIGRAVITY_CLIENT_ID")
        .expect("DCODE_ANTIGRAVITY_CLIENT_ID not set — source your .env file")
}

pub fn antigravity_client_secret() -> String {
    std::env::var("DCODE_ANTIGRAVITY_CLIENT_SECRET")
        .expect("DCODE_ANTIGRAVITY_CLIENT_SECRET not set — source your .env file")
}
