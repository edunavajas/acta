//! Detección de los CLI instalados. Las apps GUI no heredan el PATH del
//! shell, así que se pregunta a un shell de login (supervisado).
//! Si `claude` es un alias que fija variables (`CLAUDE_CONFIG_DIR=...`), se
//! devuelven también para guardarlas como `extra_env`.

use crate::process::run_supervised;
use acta_core::PortResult;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

/// Un `~/.zshrc` cargado (nvm, asdf, pnpm…) puede tardar >10 s sin TTY.
pub const INTERACTIVE_SHELL_TIMEOUT: Duration = Duration::from_secs(45);
const LOGIN_SHELL_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetectedCli {
    pub path: PathBuf,
    pub env: BTreeMap<String, String>,
}

/// `<shell> -lc 'command -v <name>'`. `Ok(None)` si no existe.
pub async fn detect_cli(shell: &Path, name: &str) -> PortResult<Option<DetectedCli>> {
    let mut cmd = Command::new(shell);
    cmd.arg("-lc").arg(format!("command -v {name}"));
    let out = run_supervised(cmd, None, Duration::from_secs(10)).await?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(parse_command_v(&String::from_utf8_lossy(&out.stdout)))
}

/// Primero un shell INTERACTIVO (`-ic`), que lee `~/.zshrc` y ve los alias
/// (p. ej. `alias claude='CLAUDE_CONFIG_DIR="$HOME/.claude-personal" …'`): es
/// lo que ejecuta el usuario en su terminal, así que manda aunque el binario
/// también esté en el PATH. Si falla, un shell de login (`-lc`).
/// `$HOME`, `${HOME}` y `~` se expanden con `home`.
pub async fn detect_cli_any(shell: &Path, name: &str, home: &Path) -> PortResult<Option<DetectedCli>> {
    for (flag, timeout) in [("-ic", INTERACTIVE_SHELL_TIMEOUT), ("-lc", LOGIN_SHELL_TIMEOUT)] {
        let mut cmd = Command::new(shell);
        cmd.arg(flag).arg(format!("command -v {name}"));
        let out = match run_supervised(cmd, None, timeout).await {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!("{shell:?} {flag}: {e}");
                continue;
            }
        };
        if !out.status.success() {
            continue;
        }
        if let Some(d) = parse_command_v(&String::from_utf8_lossy(&out.stdout)) {
            return Ok(Some(expand_home(d, home)));
        }
    }
    Ok(None)
}

/// Expande `$HOME`, `${HOME}` y un `~` inicial en la ruta y en las variables.
pub fn expand_home(d: DetectedCli, home: &Path) -> DetectedCli {
    let h = home.to_string_lossy();
    let exp = |s: &str| -> String {
        let s = s.replace("${HOME}", &h).replace("$HOME", &h);
        match s.strip_prefix("~/") {
            Some(rest) => format!("{h}/{rest}"),
            None if s == "~" => h.to_string(),
            None => s,
        }
    };
    DetectedCli {
        path: PathBuf::from(exp(&d.path.to_string_lossy())),
        env: d.env.into_iter().map(|(k, v)| (k, exp(&v))).collect(),
    }
}

/// Interpreta la salida de `command -v`: ruta absoluta o definición de alias.
pub fn parse_command_v(output: &str) -> Option<DetectedCli> {
    for line in output.lines().rev() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if (line.starts_with('/') || line.starts_with("~/")) && !line.contains(' ') {
            return Some(DetectedCli { path: line.into(), env: BTreeMap::new() });
        }
        // `alias claude='CLAUDE_CONFIG_DIR=/x /abs/claude'` (zsh) o `claude=...` (bash `alias`).
        let def = line.strip_prefix("alias ").unwrap_or(line);
        if let Some((_, value)) = def.split_once('=') {
            // El valor del alias va entrecomillado: se quita un nivel y se vuelve a trocear.
            let mut words = shell_words(value);
            if let [single] = words.as_slice() {
                words = shell_words(single);
            }
            let mut env = BTreeMap::new();
            let mut path = None;
            for w in words {
                if path.is_none()
                    && let Some((k, v)) = w.split_once('=')
                    && !k.is_empty()
                    && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                {
                    env.insert(k.to_owned(), v.to_owned());
                } else if path.is_none() && (w.starts_with('/') || w.starts_with("~/")) {
                    path = Some(PathBuf::from(w));
                }
            }
            if let Some(path) = path {
                return Some(DetectedCli { path, env });
            }
        }
    }
    None
}

/// Troceo estilo shell mínimo: comillas simples/dobles y barra invertida.
fn shell_words(s: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut in_word = false;
    let mut quote: Option<char> = None;
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('"'), '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                }
            }
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                in_word = true;
            }
            (None, '\\') => {
                if let Some(n) = chars.next() {
                    cur.push(n);
                    in_word = true;
                }
            }
            (None, c) if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut cur));
                    in_word = false;
                }
            }
            (None, c) => {
                cur.push(c);
                in_word = true;
            }
        }
    }
    if in_word {
        words.push(cur);
    }
    words
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::fake_script;

    #[test]
    fn plain_path() {
        let d = parse_command_v("/opt/homebrew/bin/claude\n").expect("detected");
        assert_eq!(d.path, PathBuf::from("/opt/homebrew/bin/claude"));
        assert!(d.env.is_empty());
    }

    #[test]
    fn alias_with_env_assignment() {
        let d = parse_command_v("alias claude='CLAUDE_CONFIG_DIR=\"/Users/e/My Dir/.claude\" /Users/e/.local/bin/claude'\n").expect("detected");
        assert_eq!(d.path, PathBuf::from("/Users/e/.local/bin/claude"));
        assert_eq!(d.env.get("CLAUDE_CONFIG_DIR").map(String::as_str), Some("/Users/e/My Dir/.claude"));
    }

    #[test]
    fn noise_before_path_and_not_found() {
        assert_eq!(parse_command_v("Welcome!\n/usr/local/bin/codex\n").expect("d").path, PathBuf::from("/usr/local/bin/codex"));
        assert!(parse_command_v("").is_none());
        assert!(parse_command_v("claude: not found").is_none());
    }

    #[test]
    fn home_is_expanded_in_alias_env() {
        let d = parse_command_v("alias claude='CLAUDE_CONFIG_DIR=\"$HOME/.claude-personal\" /opt/homebrew/bin/claude'").expect("d");
        let d = expand_home(d, Path::new("/Users/edu"));
        assert_eq!(d.env.get("CLAUDE_CONFIG_DIR").map(String::as_str), Some("/Users/edu/.claude-personal"));
        let d = expand_home(
            DetectedCli { path: "~/bin/x".into(), env: [("A".to_string(), "${HOME}/a".to_string()), ("B".to_string(), "~/b".to_string())].into() },
            Path::new("/h"),
        );
        assert_eq!(d.path, PathBuf::from("/h/bin/x"));
        assert_eq!(d.env["A"], "/h/a");
        assert_eq!(d.env["B"], "/h/b");
    }

    #[tokio::test]
    #[cfg_attr(not(unix), ignore = "el binario falso es un script #!/bin/sh")]
    async fn falls_back_to_interactive_shell_for_aliases() {
        let dir = tempfile::tempdir().expect("dir");
        // -lc no ve el alias (sale 1); -ic sí (lo define ~/.zshrc).
        let shell = fake_script(
            dir.path(),
            "zsh",
            "if [ \"$1\" = \"-ic\" ]; then echo \"alias claude='CLAUDE_CONFIG_DIR=\\\"\\$HOME/.c\\\" /opt/x/claude'\"; else exit 1; fi\n",
        );
        let d = detect_cli_any(&shell, "claude", Path::new("/Users/e")).await.expect("ok").expect("found");
        assert_eq!(d.path, PathBuf::from("/opt/x/claude"));
        assert_eq!(d.env.get("CLAUDE_CONFIG_DIR").map(String::as_str), Some("/Users/e/.c"));
    }

    /// Caso real (16-nov-2026): `/opt/homebrew/bin` está en el PATH del
    /// shell de login, así que `-lc` encuentra el binario SIN el alias y se
    /// perdía `CLAUDE_CONFIG_DIR`. En su terminal manda el alias: gana `-ic`.
    #[tokio::test]
    #[cfg_attr(not(unix), ignore = "el binario falso es un script #!/bin/sh")]
    async fn interactive_alias_wins_over_login_path() {
        let dir = tempfile::tempdir().expect("dir");
        let shell = fake_script(
            dir.path(),
            "zsh",
            "if [ \"$1\" = \"-ic\" ]; then echo \"alias claude='CLAUDE_CONFIG_DIR=\\\"\\$HOME/.c\\\" /opt/x/claude'\"; else echo /opt/x/claude; fi\n",
        );
        let d = detect_cli_any(&shell, "claude", Path::new("/Users/e")).await.expect("ok").expect("found");
        assert_eq!(d.path, PathBuf::from("/opt/x/claude"));
        assert_eq!(d.env.get("CLAUDE_CONFIG_DIR").map(String::as_str), Some("/Users/e/.c"));
    }

    /// Un `~/.zshrc` real tarda 12,3 s en arrancar sin TTY (nvm, asdf…):
    /// con 10 s el shell interactivo moría por timeout y se perdía el alias.
    #[test]
    fn interactive_shell_gets_time_for_a_slow_zshrc() {
        assert!(INTERACTIVE_SHELL_TIMEOUT >= Duration::from_secs(30));
    }

    #[tokio::test]
    #[cfg_attr(not(unix), ignore = "el binario falso es un script #!/bin/sh")]
    async fn login_path_is_used_when_interactive_shell_fails() {
        let dir = tempfile::tempdir().expect("dir");
        let shell = fake_script(dir.path(), "zsh", "if [ \"$1\" = \"-ic\" ]; then exit 1; else echo /opt/y/codex; fi\n");
        let d = detect_cli_any(&shell, "codex", Path::new("/Users/e")).await.expect("ok").expect("found");
        assert_eq!(d.path, PathBuf::from("/opt/y/codex"));
        assert!(d.env.is_empty());
    }

    #[tokio::test]
    #[cfg_attr(not(unix), ignore = "el binario falso es un script #!/bin/sh")]
    async fn runs_shell_supervised() {
        let dir = tempfile::tempdir().expect("dir");
        // Shell falso: ignora -lc y "encuentra" el binario.
        let shell = fake_script(dir.path(), "fakeshell", "echo /fake/bin/claude\n");
        let d = detect_cli(&shell, "claude").await.expect("ok").expect("found");
        assert_eq!(d.path, PathBuf::from("/fake/bin/claude"));
        let none = fake_script(dir.path(), "emptyshell", "exit 1\n");
        assert!(detect_cli(&none, "claude").await.expect("ok").is_none());
    }
}
