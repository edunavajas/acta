//! Lanzamiento SUPERVISADO de procesos hijo (patrón obligatorio en todo acta).
//!
//! Lección de Meetily (docs/referencia-meetily-zombis.md): si falla la
//! escritura en stdin y se sale con `?` antes de `wait()`, el hijo queda
//! zombi. Aquí stdin, stdout y stderr se procesan EN PARALELO y `wait()` se
//! llama SIEMPRE, también tras error de escritura o timeout (kill + wait).

use acta_core::{PortError, PortResult};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

#[derive(Debug)]
pub struct ProcessOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// Error al escribir stdin (p. ej. broken pipe), informado DESPUÉS de recoger al hijo.
    pub stdin_error: Option<String>,
}

/// Ejecuta `cmd`, le pasa `stdin` (si hay), recoge stdout/stderr y espera.
/// En timeout: `start_kill` + `wait` y devuelve `PortError::Timeout`.
pub async fn run_supervised(
    mut cmd: Command,
    stdin: Option<Vec<u8>>,
    timeout: Duration,
) -> PortResult<ProcessOutput> {
    cmd.kill_on_drop(true)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| PortError::Unavailable(format!("spawn failed: {e}")))?;

    let stdin_pipe = child.stdin.take();
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();

    let write = async move {
        match (stdin_pipe, stdin) {
            (Some(mut pipe), Some(data)) => {
                let r = pipe.write_all(&data).await;
                drop(pipe); // EOF para el hijo
                r.err().map(|e| e.to_string())
            }
            _ => None,
        }
    };
    let read_out = async move {
        let mut buf = Vec::new();
        if let Some(mut p) = stdout_pipe {
            let _ = p.read_to_end(&mut buf).await;
        }
        buf
    };
    let read_err = async move {
        let mut buf = Vec::new();
        if let Some(mut p) = stderr_pipe {
            let _ = p.read_to_end(&mut buf).await;
        }
        buf
    };

    let finished = tokio::time::timeout(timeout, async {
        let (stdin_error, stdout, stderr) = tokio::join!(write, read_out, read_err);
        let status = child.wait().await;
        (stdin_error, stdout, stderr, status)
    })
    .await;

    match finished {
        Ok((stdin_error, stdout, stderr, status)) => Ok(ProcessOutput {
            status: status?,
            stdout,
            stderr,
            stdin_error,
        }),
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await; // recoger SIEMPRE
            Err(PortError::Timeout(format!("process exceeded {timeout:?}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[cfg_attr(not(unix), ignore = "lanza /bin/cat, /bin/sh y /bin/sleep")]
    async fn pipes_stdin_to_stdout() {
        let out = run_supervised(Command::new("/bin/cat"), Some(b"hola".to_vec()), Duration::from_secs(5))
            .await
            .expect("run");
        assert!(out.status.success());
        assert_eq!(out.stdout, b"hola");
    }

    #[tokio::test]
    #[cfg_attr(not(unix), ignore = "lanza /bin/cat, /bin/sh y /bin/sleep")]
    async fn child_that_exits_early_is_still_reaped() {
        // El hijo sale sin leer stdin: la escritura puede dar broken pipe,
        // pero se recoge su estado igualmente (nada de zombis).
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "exit 3"]);
        let out = run_supervised(cmd, Some(vec![b'x'; 4 * 1024 * 1024]), Duration::from_secs(5))
            .await
            .expect("run");
        assert_eq!(out.status.code(), Some(3));
    }

    #[tokio::test]
    #[cfg_attr(not(unix), ignore = "lanza /bin/cat, /bin/sh y /bin/sleep")]
    async fn timeout_kills_and_reaps() {
        let mut cmd = Command::new("/bin/sleep");
        cmd.arg("5");
        let err = run_supervised(cmd, None, Duration::from_millis(100))
            .await
            .expect_err("timeout");
        assert!(matches!(err, PortError::Timeout(_)));
    }
}
