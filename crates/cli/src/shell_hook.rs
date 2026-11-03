//! "Alt+F to fix" from any terminal: a snippet for the shell's rc file that,
//! after a command fails, says `dcode-ai: Alt+F to fix`; Alt+F opens
//! dcode-ai on that command (`dcode-ai fix`). The shell only passes the
//! command and its exit code; dcode-ai never re-runs anything by itself —
//! the agent asks (or uses its own permission rules) like any other command.

use std::path::Path;

/// The key, in each shell's notation (`DCODE_FIX_KEY=g` for Alt+G…).
fn key() -> char {
    std::env::var("DCODE_FIX_KEY")
        .ok()
        .and_then(|k| k.chars().next())
        .filter(|c| c.is_ascii_alphabetic())
        .map(|c| c.to_ascii_lowercase())
        .unwrap_or('f')
}

pub fn snippet(shell: &str) -> Option<String> {
    let k = key();
    let upper = k.to_ascii_uppercase();
    Some(match shell {
        "zsh" => format!(
            r#"# dcode-ai: after a command fails, Alt+{upper} opens dcode-ai on it.
__dcode_ai_cmd=""; __dcode_ai_fail_cmd=""; __dcode_ai_fail_exit=0
__dcode_ai_preexec() {{ __dcode_ai_cmd="$1"; }}
__dcode_ai_precmd() {{
  local ec=$?
  if [[ $ec -ne 0 && -n "$__dcode_ai_cmd" && "$__dcode_ai_cmd" != dcode-ai* ]]; then
    __dcode_ai_fail_cmd="$__dcode_ai_cmd"; __dcode_ai_fail_exit=$ec
    print -P "%F{{8}}dcode-ai: Alt+{upper} to fix%f"
  fi
  __dcode_ai_cmd=""
}}
__dcode_ai_fix() {{
  [[ -z "$__dcode_ai_fail_cmd" ]] && return
  BUFFER="dcode-ai fix --exit $__dcode_ai_fail_exit --cmd ${{(q)__dcode_ai_fail_cmd}}"
  __dcode_ai_fail_cmd=""
  zle accept-line
}}
autoload -Uz add-zsh-hook
add-zsh-hook preexec __dcode_ai_preexec
add-zsh-hook precmd __dcode_ai_precmd
zle -N __dcode_ai_fix
bindkey '\e{k}' __dcode_ai_fix
"#
        ),
        "bash" => format!(
            r#"# dcode-ai: after a command fails, Alt+{upper} opens dcode-ai on it.
__dcode_ai_fail_cmd=""; __dcode_ai_fail_exit=0; __dcode_ai_seen=""
__dcode_ai_prompt() {{
  local ec=$?
  local cmd; cmd=$(HISTTIMEFORMAT= history 1 | sed 's/^ *[0-9]* *//')
  if [[ $ec -ne 0 && "$cmd" != "$__dcode_ai_seen" && "$cmd" != dcode-ai* ]]; then
    __dcode_ai_fail_cmd=$cmd; __dcode_ai_fail_exit=$ec
    printf '\e[90mdcode-ai: Alt+{upper} to fix\e[0m\n'
  fi
  __dcode_ai_seen=$cmd
  return $ec
}}
PROMPT_COMMAND="__dcode_ai_prompt${{PROMPT_COMMAND:+;$PROMPT_COMMAND}}"
__dcode_ai_fix() {{
  [[ -z "$__dcode_ai_fail_cmd" ]] && return
  local c=$__dcode_ai_fail_cmd; __dcode_ai_fail_cmd=""
  dcode-ai fix --exit "$__dcode_ai_fail_exit" --cmd "$c"
}}
bind -x '"\e{k}": __dcode_ai_fix'
"#
        ),
        "fish" => format!(
            r#"# dcode-ai: after a command fails, Alt+{upper} opens dcode-ai on it.
function __dcode_ai_postexec --on-event fish_postexec
    set -l ec $status
    if test $ec -ne 0; and not string match -q 'dcode-ai*' -- $argv[1]
        set -g __dcode_ai_fail_cmd $argv[1]
        set -g __dcode_ai_fail_exit $ec
        set_color brblack; echo "dcode-ai: Alt+{upper} to fix"; set_color normal
    end
end
function __dcode_ai_fix
    if set -q __dcode_ai_fail_cmd
        commandline -r "dcode-ai fix --exit $__dcode_ai_fail_exit --cmd "(string escape -- $__dcode_ai_fail_cmd)
        set -e __dcode_ai_fail_cmd
        commandline -f execute
    end
end
bind \e{k} __dcode_ai_fix
"#
        ),
        "powershell" | "pwsh" => format!(
            r#"# dcode-ai: after a command fails, Alt+{upper} opens dcode-ai on it.
$global:__dcodeAiFail = $null
$global:__dcodeAiPrompt = $function:prompt
function global:prompt {{
    $ok = $?; $code = $LASTEXITCODE
    $last = Get-History -Count 1
    if ($last -and $last.Id -ne $global:__dcodeAiSeen) {{
        $global:__dcodeAiSeen = $last.Id
        if ((-not $ok) -and ($last.CommandLine -notlike 'dcode-ai*')) {{
            $global:__dcodeAiFail = @{{ cmd = $last.CommandLine; code = $(if ($code) {{ $code }} else {{ 1 }}) }}
            Write-Host "dcode-ai: Alt+{upper} to fix" -ForegroundColor DarkGray
        }}
    }}
    & $global:__dcodeAiPrompt
}}
Set-PSReadLineKeyHandler -Chord 'Alt+{k}' -ScriptBlock {{
    if ($global:__dcodeAiFail) {{
        $c = $global:__dcodeAiFail.cmd -replace "'", "''"
        $e = $global:__dcodeAiFail.code
        $global:__dcodeAiFail = $null
        [Microsoft.PowerShell.PSConsoleReadLine]::RevertLine()
        [Microsoft.PowerShell.PSConsoleReadLine]::Insert("dcode-ai fix --exit $e --cmd '$c'")
        [Microsoft.PowerShell.PSConsoleReadLine]::AcceptLine()
    }}
}}
"#
        ),
        _ => return None,
    })
}

const MASK: &str = "[masked]";

/// Words that mark a value as secret (`API_KEY=…`, `?token=…`, `--password …`).
fn secret_name(name: &str) -> bool {
    let n = name.trim_start_matches('-').to_ascii_lowercase();
    [
        "key",
        "token",
        "secret",
        "password",
        "passwd",
        "pass",
        "pwd",
        "auth",
        "credential",
        "cookie",
        "session",
    ]
    .iter()
    .any(|m| n.contains(m))
}

/// Masks secrets typed into a command line: `Bearer …`/`Basic …`, `-u
/// user:pass`, `--password …`-style flags, `NAME=…` where the name says
/// secret, `?token=…` in URLs, `user:pass@` in URLs — then the known token
/// shapes and secret env values (`redact_secrets`). Returns the masked text
/// and how many were masked.
pub fn mask_command(cmd: &str) -> (String, usize) {
    let mut count = 0;
    let words: Vec<&str> = cmd.split_whitespace().collect();
    // `-p` is a password only for these (cargo's `-p` is a package).
    let program = words
        .first()
        .map(|p| p.rsplit('/').next().unwrap_or(p))
        .unwrap_or("");
    let p_is_password = matches!(program, "mysql" | "mysqldump" | "mysqladmin" | "sshpass");
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    for (i, w) in words.iter().enumerate() {
        let prev = if i > 0 { words[i - 1] } else { "" };
        let prev_bare = prev.trim_matches(['"', '\'']).to_ascii_lowercase();
        let quote_end = |s: &str| -> String {
            // Keep a closing quote so the command still reads right.
            let tail: String = s
                .chars()
                .rev()
                .take_while(|c| *c == '"' || *c == '\'')
                .collect();
            format!("{MASK}{tail}")
        };
        // `Authorization: Bearer <token>`, `Basic <b64>`.
        if prev_bare == "bearer"
            || prev_bare == "basic"
            || prev_bare == "token" && prev.ends_with(':')
        {
            out.push(quote_end(w));
            count += 1;
            continue;
        }
        // `-u user:pass` (curl), `--password x`, `--token x`, `-p x`.
        if prev == "-u" || prev == "--user" {
            match w.split_once(':') {
                Some((user, pass)) if !pass.is_empty() => {
                    out.push(format!("{user}:{}", quote_end(pass)));
                    count += 1;
                }
                _ => out.push(w.to_string()),
            }
            continue;
        }
        if (prev.starts_with("--") && secret_name(prev) && !prev.contains('='))
            || (prev == "-p" && p_is_password)
        {
            out.push(quote_end(w));
            count += 1;
            continue;
        }
        // `mysql -pS3cret` (attached).
        if p_is_password && w.starts_with("-p") && w.len() > 2 {
            out.push(format!("-p{MASK}"));
            count += 1;
            continue;
        }
        let mut word = w.to_string();
        // `--token=x`, `API_KEY=x`.
        if let Some((name, value)) = word.split_once('=')
            && !value.is_empty()
            && !name.contains('/')
            && !name.contains('?')
            && secret_name(name)
        {
            word = format!("{name}={}", quote_end(value));
            count += 1;
        }
        // URLs: `user:pass@host` and `?token=…&`.
        if let Some(scheme) = word.find("://") {
            let rest_start = scheme + 3;
            if let Some(at) = word[rest_start..].find('@').map(|a| a + rest_start)
                && let Some(colon) = word[rest_start..at].find(':').map(|c| c + rest_start)
            {
                word = format!("{}{MASK}{}", &word[..colon + 1], &word[at..]);
                count += 1;
            }
            if let Some(q) = word.find('?') {
                let (base, query) = word.split_at(q + 1);
                let parts: Vec<String> = query
                    .split('&')
                    .map(|kv| match kv.split_once('=') {
                        Some((k, v)) if !v.is_empty() && secret_name(k) => {
                            count += 1;
                            format!("{k}={MASK}")
                        }
                        _ => kv.to_string(),
                    })
                    .collect();
                word = format!("{base}{}", parts.join("&"));
            }
        }
        out.push(word);
    }
    let joined = out.join(" ");
    let shaped = dcode_ai_common::redact::redact_secrets(&joined);
    if shaped != joined {
        count += 1;
    }
    (shaped, count)
}

/// What dcode-ai is asked when Alt+F opens it on a failed command (secrets
/// in the command line masked — see [`mask_command`]).
pub fn fix_prompt(cmd: &str, exit: i32, cwd: &Path) -> String {
    let (cmd, _) = mask_command(cmd);
    format!(
        "This command just failed in my terminal (exit code {exit}), in {}:\n\n```\n{}\n```\n\n\
         Find out why it failed and fix it. If running it again is safe and \
         would show the error, run it; if it could change something, ask me first.",
        cwd.display(),
        cmd.trim()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_shell_gets_a_snippet_with_the_key() {
        for sh in ["zsh", "bash", "fish", "powershell"] {
            let s = snippet(sh).unwrap();
            assert!(s.contains("dcode-ai fix --exit"), "{sh}");
            assert!(s.contains("Alt+F to fix"), "{sh}");
        }
        assert!(snippet("zsh").unwrap().contains("bindkey '\\ef'"));
        assert!(snippet("tcsh").is_none());
        let p = fix_prompt("cargo build", 101, Path::new("/w"));
        assert!(p.contains("exit code 101") && p.contains("```\ncargo build\n```"));
    }

    #[test]
    fn secrets_typed_into_a_command_are_masked() {
        let cases = [
            (
                r#"curl -H "Authorization: Bearer abc123secret" https://api.x.com"#,
                "abc123secret",
            ),
            ("curl -u admin:hunter22 https://x.com", "hunter22"),
            ("mysql -p S3cretPw db", "S3cretPw"),
            (
                "deploy --token tok_live_9f8e7d --env prod",
                "tok_live_9f8e7d",
            ),
            ("API_KEY=k-12345 node app.js", "k-12345"),
            (
                "curl https://x.com/v1?user=me&access_token=zz99&x=1",
                "zz99",
            ),
            ("git clone https://me:pa55word@github.com/a/b", "pa55word"),
            (
                "echo ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345",
                "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ012345",
            ),
        ];
        for (cmd, secret) in cases {
            let (masked, n) = mask_command(cmd);
            assert!(!masked.contains(secret), "{cmd} → {masked}");
            assert!(n >= 1, "{cmd}");
        }
        // Nothing secret: unchanged.
        let (plain, n) = mask_command("cargo test --release -p dcode-ai");
        assert_eq!((plain.as_str(), n), ("cargo test --release -p dcode-ai", 0));
        assert!(!fix_prompt("curl -u a:b-pass x", 1, Path::new("/w")).contains("b-pass"));
    }
}
