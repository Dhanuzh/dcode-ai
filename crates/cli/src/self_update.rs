//! `dcode-ai update`: replace this binary with the latest GitHub release.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

const CURRENT: &str = env!("CARGO_PKG_VERSION");
const LATEST_URL: &str = "https://api.github.com/repos/Dhanuzh/dcode-ai/releases/latest";

/// The release asset name stem for this platform (`dcode-ai-<target>`).
fn target() -> Option<&'static str> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some("x86_64-unknown-linux-gnu")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("aarch64-apple-darwin")
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Some("x86_64-apple-darwin")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Some("x86_64-pc-windows-msvc")
    } else {
        None
    }
}

fn parse_ver(v: &str) -> (u64, u64, u64) {
    let mut p = v
        .trim()
        .trim_start_matches('v')
        .split(['.', '-', '+'])
        .filter_map(|x| x.parse::<u64>().ok());
    (
        p.next().unwrap_or(0),
        p.next().unwrap_or(0),
        p.next().unwrap_or(0),
    )
}

/// The hex digest in a `sha256sum`-style line (`<hex>  <file>`).
fn expected_digest(sha_file: &str) -> Option<String> {
    let hex = sha_file
        .trim_start_matches('\u{feff}')
        .split_whitespace()
        .next()?
        .to_ascii_lowercase();
    (hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit())).then_some(hex)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub async fn run(check_only: bool, force: bool) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("dcode-ai/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(120))
        .build()?;
    let release: serde_json::Value = client
        .get(LATEST_URL)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let tag = release["tag_name"].as_str().unwrap_or_default().to_string();
    if tag.is_empty() {
        anyhow::bail!("could not read the latest release");
    }
    let newer = parse_ver(&tag) > parse_ver(CURRENT);
    if !newer && !force {
        println!("dcode-ai v{CURRENT} is the latest version.");
        return Ok(());
    }
    if check_only {
        println!("Update available: v{CURRENT} → {tag}. Run `dcode-ai update`.");
        return Ok(());
    }
    let target = target().ok_or_else(|| {
        anyhow::anyhow!("no prebuilt binary for this platform; build from source instead")
    })?;
    let ext = if cfg!(windows) { "zip" } else { "tar.gz" };
    let archive_name = format!("dcode-ai-{target}.{ext}");
    let url_of = |name: &str| {
        release["assets"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|a| a["name"].as_str() == Some(name))
            .and_then(|a| a["browser_download_url"].as_str())
            .map(String::from)
    };
    let archive_url = url_of(&archive_name)
        .ok_or_else(|| anyhow::anyhow!("release {tag} has no {archive_name}"))?;
    let sha_url = url_of(&format!("{archive_name}.sha256"))
        .ok_or_else(|| anyhow::anyhow!("release {tag} has no checksum for {archive_name}"))?;

    println!("Downloading {archive_name} ({tag})…");
    let bytes = client
        .get(&archive_url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    let sha_text = client
        .get(&sha_url)
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    let want =
        expected_digest(&sha_text).ok_or_else(|| anyhow::anyhow!("unreadable checksum file"))?;
    let got = hex(&Sha256::digest(&bytes));
    if got != want {
        anyhow::bail!(
            "checksum mismatch for {archive_name} (expected {want}, got {got}); not installing"
        );
    }

    let work = std::env::temp_dir().join(format!("dcode-ai-update-{}", std::process::id()));
    std::fs::create_dir_all(&work)?;
    let archive = work.join(&archive_name);
    std::fs::write(&archive, &bytes)?;
    let status = std::process::Command::new("tar")
        .arg(if cfg!(windows) { "-xf" } else { "-xzf" })
        .arg(&archive)
        .arg("-C")
        .arg(&work)
        .status()
        .map_err(|e| anyhow::anyhow!("could not run tar: {e}"))?;
    if !status.success() {
        anyhow::bail!("could not unpack {archive_name}");
    }
    let bin_name = if cfg!(windows) {
        "dcode-ai.exe"
    } else {
        "dcode-ai"
    };
    let new_bin = find_file(&work, bin_name)
        .ok_or_else(|| anyhow::anyhow!("{archive_name} does not contain {bin_name}"))?;

    let exe = std::env::current_exe()?;
    let exe = exe.canonicalize().unwrap_or(exe);
    replace_binary(&new_bin, &exe)?;
    let _ = std::fs::remove_dir_all(&work);
    println!("Updated dcode-ai v{CURRENT} → {tag} ({}).", exe.display());
    Ok(())
}

fn find_file(dir: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if p.is_dir() {
            if let Some(found) = find_file(&p, name) {
                return Some(found);
            }
        } else if p.file_name().is_some_and(|n| n == name) {
            return Some(p);
        }
    }
    None
}

/// Swap `new` in for the running binary at `exe`.
fn replace_binary(new: &Path, exe: &Path) -> anyhow::Result<()> {
    let staged = exe.with_extension("new");
    std::fs::copy(new, &staged).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            anyhow::anyhow!(
                "no permission to write {} — run `sudo dcode-ai update`",
                exe.display()
            )
        } else {
            anyhow::anyhow!("could not write {}: {e}", staged.display())
        }
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    }
    // Windows can't overwrite a running exe, but it can rename it.
    if cfg!(windows) {
        let old = exe.with_extension("old.exe");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(exe, &old)?;
    }
    std::fs::rename(&staged, exe)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_sha256sum_and_powershell_checksums() {
        let h = "a".repeat(64);
        assert_eq!(
            expected_digest(&format!("{h}  dcode-ai.tar.gz\n")),
            Some(h.clone())
        );
        assert_eq!(
            expected_digest(&format!("\u{feff}{}  dcode-ai.zip\r\n", h.to_uppercase())),
            Some(h)
        );
        assert_eq!(expected_digest("nope"), None);
        assert!(parse_ver("v1.0.12") > parse_ver("1.0.9"));
    }

    #[test]
    fn replaces_a_binary_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("dcode-ai");
        let new = dir.path().join("fresh");
        std::fs::write(&exe, "old").unwrap();
        std::fs::write(&new, "new").unwrap();
        replace_binary(&new, &exe).unwrap();
        assert_eq!(std::fs::read_to_string(&exe).unwrap(), "new");
        assert!(!exe.with_extension("new").exists());
    }
}
