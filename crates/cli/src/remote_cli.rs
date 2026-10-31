//! `dcode-ai remote <host>`: run dcode-ai on another machine over SSH and use
//! it from here. SSH carries everything; nothing listens on the network.
//! With `--keep` it runs inside tmux on the server, so it keeps working when
//! the connection drops and the same command attaches to it again.

/// Quote `s` for a POSIX shell (`~/` is kept expandable).
fn sh_quote(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/") {
        return format!("\"$HOME\"/{}", sh_quote(rest));
    }
    if s == "~" {
        return "\"$HOME\"".into();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// The command run on the server.
pub fn remote_command(dir: Option<&str>, keep: bool, args: &[String]) -> String {
    let mut inner = String::new();
    if let Some(dir) = dir.filter(|d| !d.is_empty()) {
        inner.push_str(&format!("cd {} && ", sh_quote(dir)));
    }
    inner.push_str("dcode-ai");
    for a in args {
        inner.push(' ');
        inner.push_str(&sh_quote(a));
    }
    if !keep {
        return inner;
    }
    // One tmux session per folder: reconnecting attaches to the same one.
    let slug: String = dir
        .unwrap_or("home")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(40)
        .collect();
    let name = format!("dcode-ai-{}", if slug.is_empty() { "home" } else { &slug });
    format!(
        "tmux new-session -A -s {} {}",
        sh_quote(&name),
        sh_quote(&inner)
    )
}

/// Run `ssh -t <host> <command>` attached to this terminal; returns its exit
/// code.
pub fn run(host: &str, dir: Option<&str>, keep: bool, args: &[String]) -> anyhow::Result<i32> {
    // A host starting with `-` would be read as an ssh option.
    if host.is_empty() || host.starts_with('-') {
        anyhow::bail!("not a host: {host:?}");
    }
    let command = remote_command(dir, keep, args);
    let status = std::process::Command::new("ssh")
        .arg("-t")
        .arg("--")
        .arg(host)
        .arg(&command)
        .status()
        .map_err(|e| anyhow::anyhow!("could not run ssh: {e}"))?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_server_command() {
        assert_eq!(remote_command(None, false, &[]), "dcode-ai");
        assert_eq!(
            remote_command(Some("~/code/my app"), false, &["-r".into()]),
            "cd \"$HOME\"/'code/my app' && dcode-ai '-r'"
        );
        let kept = remote_command(Some("/srv/app"), true, &[]);
        assert!(kept.starts_with("tmux new-session -A -s 'dcode-ai-srv-app' "));
        assert!(kept.contains("cd '\\''/srv/app'\\'' && dcode-ai"));
    }

    #[test]
    fn quotes_hostile_arguments() {
        let cmd = remote_command(None, false, &["it's; rm -rf /".into()]);
        assert_eq!(cmd, r"dcode-ai 'it'\''s; rm -rf /'");
    }
}
