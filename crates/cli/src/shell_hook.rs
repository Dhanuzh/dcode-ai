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

/// What dcode-ai is asked when Alt+F opens it on a failed command.
pub fn fix_prompt(cmd: &str, exit: i32, cwd: &Path) -> String {
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
}
