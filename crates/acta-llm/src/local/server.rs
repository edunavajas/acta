//! Supervisor del `llama-server` del modelo local integrado.
//!
//! Un solo proceso hijo, con las reglas de todo acta
//! (docs/referencia-meetily-zombis.md): `tokio::process` +
//! `kill_on_drop(true)` y `wait()` en TODOS los caminos (parada, cambio de
//! modelo, arranque fallido, tiempo agotado, hijo que muere solo).
//!
//! - Arranca la primera vez que hace falta y escucha solo en `127.0.0.1`, en
//!   un puerto libre. Está listo cuando `GET /health` responde 200.
//! - Va en un proceso aparte a propósito: si llama.cpp se queda sin memoria
//!   con un modelo grande, muere él y no la grabación en curso.
//! - Se para solo tras un rato sin uso para devolver la memoria
//!   ([`LlamaSupervisor::stop_if_idle`]) y al cerrar la app.
//! - Si acta muere de golpe (`kill -9`), el hijo queda huérfano: su PID se
//!   apunta en un fichero y [`kill_stale`] lo mata en el siguiente arranque.

use crate::process::run_supervised;
use crate::providers::common::tail;
use acta_core::{PortError, PortResult};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::process::{Child, Command};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSpec {
    pub binary: PathBuf,
    pub model_path: PathBuf,
    pub model_id: String,
    pub context_tokens: u32,
    /// stdout + stderr del servidor (se trunca en cada arranque).
    pub log_path: Option<PathBuf>,
    /// Dónde apuntar el PID para [`kill_stale`].
    pub pid_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy)]
pub struct SupervisorTimings {
    /// Tiempo máximo hasta que `/health` responde (cargar 6 GB tarda).
    pub start_timeout: Duration,
    pub poll: Duration,
}

impl Default for SupervisorTimings {
    fn default() -> Self {
        Self { start_timeout: Duration::from_secs(180), poll: Duration::from_millis(250) }
    }
}

/// Argumentos de `llama-server`. Todas las capas a la GPU (Metal), un solo
/// hueco (las peticiones hacen cola), plantilla de chat del modelo y sin
/// razonamiento: el acta quiere el JSON, no el «pensar en voz alta».
pub fn server_args(spec: &ServerSpec, port: u16) -> Vec<String> {
    let mut args: Vec<String> = ["-m".into(), spec.model_path.to_string_lossy().into_owned()].into();
    for (flag, value) in [
        ("--host", "127.0.0.1".to_owned()),
        ("--port", port.to_string()),
        ("-c", spec.context_tokens.to_string()),
        ("-ngl", "99".into()),
        ("-np", "1".into()),
        ("--reasoning-budget", "0".into()),
        ("--alias", spec.model_id.clone()),
    ] {
        args.push(flag.into());
        args.push(value);
    }
    args.extend(["--jinja".into(), "--no-webui".into()]);
    args
}

struct Running {
    child: Child,
    base_url: String,
    spec: ServerSpec,
}

#[derive(Debug)]
struct Usage {
    in_flight: AtomicUsize,
    last_used: Mutex<Instant>,
}

/// Mientras exista, hay una petición en curso y el servidor no se para por inactividad.
#[derive(Debug)]
pub struct Lease(Arc<Usage>);

impl Drop for Lease {
    fn drop(&mut self) {
        *self.0.last_used.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        self.0.in_flight.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct LlamaSupervisor {
    running: tokio::sync::Mutex<Option<Running>>,
    usage: Arc<Usage>,
    timings: SupervisorTimings,
    client: reqwest::Client,
}

impl Default for LlamaSupervisor {
    fn default() -> Self {
        Self::new(SupervisorTimings::default())
    }
}

async fn free_port() -> PortResult<u16> {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    Ok(listener.local_addr()?.port())
}

/// Mata y RECOGE al hijo. Siempre las dos cosas.
async fn kill_and_reap(mut running: Running) {
    let _ = running.child.start_kill();
    let _ = running.child.wait().await;
    if let Some(p) = &running.spec.pid_file {
        let _ = tokio::fs::remove_file(p).await;
    }
}

async fn log_tail(spec: &ServerSpec) -> String {
    match &spec.log_path {
        Some(p) => tail(&tokio::fs::read(p).await.unwrap_or_default()),
        None => String::new(),
    }
}

impl LlamaSupervisor {
    pub fn new(timings: SupervisorTimings) -> Self {
        Self {
            running: tokio::sync::Mutex::new(None),
            usage: Arc::new(Usage { in_flight: AtomicUsize::new(0), last_used: Mutex::new(Instant::now()) }),
            timings,
            client: reqwest::Client::new(),
        }
    }

    fn lease(&self) -> Lease {
        self.usage.in_flight.fetch_add(1, Ordering::SeqCst);
        Lease(self.usage.clone())
    }

    /// Deja en marcha un servidor con ese modelo y devuelve su URL base
    /// (`http://127.0.0.1:<puerto>`) y un [`Lease`] para la petición.
    pub async fn ensure(&self, spec: &ServerSpec) -> PortResult<(String, Lease)> {
        let lease = self.lease();
        let mut guard = self.running.lock().await;
        if let Some(mut current) = guard.take() {
            // `try_wait` también recoge a un hijo que murió solo.
            let alive = matches!(current.child.try_wait(), Ok(None));
            if alive && current.spec == *spec {
                let url = current.base_url.clone();
                *guard = Some(current);
                return Ok((url, lease));
            }
            if !alive {
                tracing::warn!(model = %current.spec.model_id, "llama-server había muerto: se reinicia");
            }
            kill_and_reap(current).await;
        }
        let running = self.start(spec).await?;
        let url = running.base_url.clone();
        *guard = Some(running);
        Ok((url, lease))
    }

    async fn start(&self, spec: &ServerSpec) -> PortResult<Running> {
        if !spec.binary.is_file() {
            return Err(PortError::Unavailable(format!("llama-server no encontrado en {}", spec.binary.display())));
        }
        if !spec.model_path.is_file() {
            return Err(PortError::Unavailable(format!("el modelo no está descargado ({})", spec.model_path.display())));
        }
        let port = free_port().await?;
        let (out, err) = match &spec.log_path {
            Some(p) => {
                if let Some(dir) = p.parent() {
                    tokio::fs::create_dir_all(dir).await?;
                }
                let f = std::fs::File::create(p)?;
                (Stdio::from(f.try_clone()?), Stdio::from(f))
            }
            None => (Stdio::null(), Stdio::null()),
        };
        let mut cmd = Command::new(&spec.binary);
        cmd.args(server_args(spec, port)).kill_on_drop(true).stdin(Stdio::null()).stdout(out).stderr(err);
        // Las dylib van junto al binario; el cwd no importa pero que no sea el del usuario.
        if let Some(dir) = spec.binary.parent() {
            cmd.current_dir(dir);
        }
        let child = cmd.spawn().map_err(|e| PortError::Unavailable(format!("no se pudo lanzar llama-server: {e}")))?;
        let mut running = Running { child, base_url: format!("http://127.0.0.1:{port}"), spec: spec.clone() };
        if let (Some(p), Some(pid)) = (&spec.pid_file, running.child.id()) {
            if let Some(dir) = p.parent() {
                let _ = tokio::fs::create_dir_all(dir).await;
            }
            let _ = tokio::fs::write(p, pid.to_string()).await;
        }

        let health = format!("{}/health", running.base_url);
        let deadline = Instant::now() + self.timings.start_timeout;
        loop {
            match running.child.try_wait() {
                Ok(None) => {}
                Ok(Some(status)) => {
                    // Ya recogido por `try_wait`.
                    let log = log_tail(spec).await;
                    if let Some(p) = &spec.pid_file {
                        let _ = tokio::fs::remove_file(p).await;
                    }
                    return Err(PortError::Unavailable(format!("llama-server terminó al arrancar ({status}). {log}")));
                }
                Err(e) => {
                    kill_and_reap(running).await;
                    return Err(PortError::Io(format!("llama-server: {e}")));
                }
            }
            let ready = tokio::time::timeout(Duration::from_secs(2), self.client.get(&health).send())
                .await
                .ok()
                .and_then(Result::ok)
                .is_some_and(|r| r.status().is_success());
            if ready {
                return Ok(running);
            }
            if Instant::now() >= deadline {
                let log = log_tail(spec).await;
                kill_and_reap(running).await;
                return Err(PortError::Timeout(format!(
                    "el modelo local no terminó de cargar en {:?}. {log}",
                    self.timings.start_timeout
                )));
            }
            tokio::time::sleep(self.timings.poll).await;
        }
    }

    /// Para el servidor (si lo hay) y lo recoge.
    pub async fn stop(&self) {
        if let Some(running) = self.running.lock().await.take() {
            kill_and_reap(running).await;
        }
    }

    /// Para el servidor si lleva `max_idle` sin peticiones y no hay ninguna en curso.
    pub async fn stop_if_idle(&self, max_idle: Duration) -> bool {
        let mut guard = self.running.lock().await;
        let idle = self.usage.in_flight.load(Ordering::SeqCst) == 0
            && self.usage.last_used.lock().unwrap_or_else(|e| e.into_inner()).elapsed() >= max_idle;
        match guard.take() {
            Some(running) if idle => {
                tracing::info!(model = %running.spec.model_id, "modelo local sin uso: se descarga de memoria");
                kill_and_reap(running).await;
                true
            }
            other => {
                *guard = other;
                false
            }
        }
    }

    /// Modelo cargado ahora mismo (y PID, para diagnósticos y pruebas).
    pub async fn running(&self) -> Option<(String, Option<u32>)> {
        let mut guard = self.running.lock().await;
        let current = guard.as_mut()?;
        matches!(current.child.try_wait(), Ok(None)).then(|| (current.spec.model_id.clone(), current.child.id()))
    }
}

/// Mata el `llama-server` que dejó huérfano un cierre brusco de acta. Solo
/// actúa si el PID apuntado sigue vivo Y su línea de comando es un
/// `llama-server` (los PID se reutilizan).
pub async fn kill_stale(pid_file: &Path) -> bool {
    let Ok(text) = tokio::fs::read_to_string(pid_file).await else { return false };
    let _ = tokio::fs::remove_file(pid_file).await;
    let Ok(pid) = text.trim().parse::<u32>() else { return false };
    let mut ps = Command::new("/bin/ps");
    ps.args(["-p", &pid.to_string(), "-o", "command="]);
    let Ok(out) = run_supervised(ps, None, Duration::from_secs(5)).await else { return false };
    if !String::from_utf8_lossy(&out.stdout).contains("llama-server") {
        return false;
    }
    let mut kill = Command::new("/bin/kill");
    kill.arg(pid.to_string());
    let killed = run_supervised(kill, None, Duration::from_secs(5)).await.is_ok_and(|o| o.status.success());
    if killed {
        tracing::warn!(pid, "se ha parado un llama-server huérfano de una sesión anterior");
    }
    killed
}

#[cfg(test)]
pub(crate) mod testing {
    use std::path::{Path, PathBuf};

    /// `llama-server` de mentira (Perl, que viene con macOS): escucha en
    /// `--port`, responde 200 a `/health` y una respuesta de chat fija a lo
    /// demás. Guarda sus argumentos en `<binario>.args`.
    pub const FAKE_SERVER: &str = r#"#!/usr/bin/perl
use strict; use IO::Socket::INET;
my $port = 0;
for (my $i = 0; $i < @ARGV; $i++) { $port = $ARGV[$i + 1] if $ARGV[$i] eq '--port'; }
open(my $fh, '>', "$0.args"); print $fh join("\n", @ARGV); close($fh);
my $srv = IO::Socket::INET->new(LocalAddr => '127.0.0.1', LocalPort => $port, Listen => 16, ReuseAddr => 1) or die "bind: $!";
while (my $c = $srv->accept) {
  my ($head, $len) = ('', 0);
  while (my $line = <$c>) { $head .= $line; $len = $1 if $line =~ /^content-length:\s*(\d+)/i; last if $line =~ /^\r?\n$/; }
  my $body = ''; read($c, $body, $len) if $len > 0;
  my $out = $head =~ m{^GET /health} ? '{"status":"ok"}'
    : '{"model":"fake","choices":[{"message":{"role":"assistant","content":"{\"a\": 1}"}}],"usage":{"prompt_tokens":3,"completion_tokens":2}}';
  print $c "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: " . length($out) . "\r\nConnection: close\r\n\r\n$out";
  close($c);
}
"#;

    pub fn write_executable(path: &Path, body: &str) -> PathBuf {
        use std::io::Write;
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let mut f = std::fs::File::create(path).expect("create");
        f.write_all(body.as_bytes()).expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            f.set_permissions(std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
        f.sync_all().expect("sync");
        path.to_path_buf()
    }

    pub fn fake_server(dir: &Path) -> PathBuf {
        write_executable(&dir.join("llama-server"), FAKE_SERVER)
    }

    /// ¿Sigue vivo el proceso? (`kill -0`).
    pub async fn alive(pid: u32) -> bool {
        let mut cmd = tokio::process::Command::new("/bin/kill");
        cmd.args(["-0", &pid.to_string()]);
        crate::process::run_supervised(cmd, None, std::time::Duration::from_secs(5)).await.is_ok_and(|o| o.status.success())
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;

    fn spec(dir: &Path, binary: PathBuf, model_id: &str) -> ServerSpec {
        let model_path = dir.join(format!("{model_id}.gguf"));
        std::fs::write(&model_path, b"gguf").expect("model");
        ServerSpec {
            binary,
            model_path,
            model_id: model_id.into(),
            context_tokens: 4096,
            log_path: Some(dir.join("logs/llama-server.log")),
            pid_file: Some(dir.join("llama-server.pid")),
        }
    }

    fn quick() -> LlamaSupervisor {
        LlamaSupervisor::new(SupervisorTimings { start_timeout: Duration::from_secs(10), poll: Duration::from_millis(30) })
    }

    async fn wait_dead(pid: u32) -> bool {
        for _ in 0..100 {
            if !alive(pid).await {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    #[test]
    fn args_bind_to_loopback_use_metal_and_disable_reasoning() {
        let s = ServerSpec {
            binary: "/x/llama-server".into(),
            model_path: "/m/q.gguf".into(),
            model_id: "qwen".into(),
            context_tokens: 16_384,
            log_path: None,
            pid_file: None,
        };
        let a = server_args(&s, 4321).join(" ");
        for part in ["-m /m/q.gguf", "--host 127.0.0.1", "--port 4321", "-c 16384", "-ngl 99", "-np 1", "--reasoning-budget 0", "--alias qwen", "--jinja", "--no-webui"] {
            assert!(a.contains(part), "falta «{part}» en {a}");
        }
        assert!(!a.contains("0.0.0.0"));
    }

    #[tokio::test]
    #[cfg_attr(not(unix), ignore = "el binario falso es un script #!/bin/sh")]
    async fn starts_lazily_waits_for_health_and_reuses_the_same_process() {
        let d = tempfile::tempdir().expect("tmp");
        let sup = quick();
        assert_eq!(sup.running().await, None);
        let s = spec(d.path(), fake_server(d.path()), "tiny");
        let (url, _lease) = sup.ensure(&s).await.expect("start");
        assert!(url.starts_with("http://127.0.0.1:"));
        let (model, pid) = sup.running().await.expect("running");
        assert_eq!(model, "tiny");
        let pid = pid.expect("pid");
        assert_eq!(std::fs::read_to_string(d.path().join("llama-server.pid")).expect("pidfile"), pid.to_string());
        let args = std::fs::read_to_string(d.path().join("llama-server.args")).expect("args");
        assert!(args.contains("tiny.gguf") && args.contains("--reasoning-budget"));

        let (url2, _lease2) = sup.ensure(&s).await.expect("reuse");
        assert_eq!(url2, url);
        assert_eq!(sup.running().await.and_then(|r| r.1), Some(pid));

        sup.stop().await;
        assert!(wait_dead(pid).await, "parado y recogido");
        assert_eq!(sup.running().await, None);
        assert!(!d.path().join("llama-server.pid").exists());
    }

    #[tokio::test]
    #[cfg_attr(not(unix), ignore = "el binario falso es un script #!/bin/sh")]
    async fn changing_the_model_replaces_the_process() {
        let d = tempfile::tempdir().expect("tmp");
        let sup = quick();
        let bin = fake_server(d.path());
        let (_, lease) = sup.ensure(&spec(d.path(), bin.clone(), "tiny")).await.expect("tiny");
        drop(lease);
        let first = sup.running().await.and_then(|r| r.1).expect("pid");
        sup.ensure(&spec(d.path(), bin, "large")).await.expect("large");
        let (model, second) = sup.running().await.expect("running");
        assert_eq!(model, "large");
        assert_ne!(second, Some(first));
        assert!(wait_dead(first).await, "el anterior no queda vivo");
        sup.stop().await;
    }

    #[tokio::test]
    #[cfg_attr(not(unix), ignore = "el binario falso es un script #!/bin/sh")]
    async fn a_server_that_dies_is_restarted_on_the_next_request() {
        let d = tempfile::tempdir().expect("tmp");
        let sup = quick();
        let s = spec(d.path(), fake_server(d.path()), "tiny");
        drop(sup.ensure(&s).await.expect("start"));
        let first = sup.running().await.and_then(|r| r.1).expect("pid");
        let mut kill = Command::new("/bin/kill");
        kill.args(["-9", &first.to_string()]);
        run_supervised(kill, None, Duration::from_secs(5)).await.expect("kill");
        // Muerto pero sin recoger (zombi) hasta que el supervisor lo mira.
        for _ in 0..100 {
            if sup.running().await.is_none() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(sup.running().await, None);
        let (url, _lease) = sup.ensure(&s).await.expect("restart");
        let second = sup.running().await.and_then(|r| r.1).expect("pid");
        assert_ne!(second, first);
        assert!(wait_dead(first).await, "el que murió quedó recogido, no zombi");
        assert!(reqwest::get(format!("{url}/health")).await.expect("health").status().is_success());
        sup.stop().await;
    }

    #[tokio::test]
    #[cfg_attr(not(unix), ignore = "el binario falso es un script #!/bin/sh")]
    async fn a_binary_that_exits_at_once_is_an_error_with_its_log() {
        let d = tempfile::tempdir().expect("tmp");
        let bin = write_executable(&d.path().join("llama-server"), "#!/bin/sh\necho 'error: unknown model architecture' >&2\nexit 1\n");
        let sup = quick();
        let err = sup.ensure(&spec(d.path(), bin, "tiny")).await.expect_err("fails");
        assert!(matches!(&err, PortError::Unavailable(m) if m.contains("unknown model architecture")), "{err}");
        assert_eq!(sup.running().await, None);
        assert!(!d.path().join("llama-server.pid").exists());
    }

    #[tokio::test]
    #[cfg_attr(not(unix), ignore = "el binario falso es un script #!/bin/sh")]
    async fn a_server_that_never_gets_healthy_times_out_and_is_killed() {
        let d = tempfile::tempdir().expect("tmp");
        let bin = write_executable(&d.path().join("llama-server"), "#!/bin/sh\necho $$ > \"$0.mypid\"\nexec /bin/sleep 30\n");
        // 3 s: con la máquina cargada (cargo test en paralelo) el shell tarda en
        // arrancar; con 1,2 s a veces se mataba antes de escribir su pid.
        let sup = LlamaSupervisor::new(SupervisorTimings { start_timeout: Duration::from_millis(3_000), poll: Duration::from_millis(30) });
        let err = sup.ensure(&spec(d.path(), bin, "tiny")).await.expect_err("timeout");
        assert!(matches!(err, PortError::Timeout(_)), "{err}");
        // Si ni llegó a escribir su pid, tampoco llegó a lanzar el `sleep`.
        if let Ok(raw) = std::fs::read_to_string(d.path().join("llama-server.mypid")) {
            let pid: u32 = raw.trim().parse().expect("pid");
            assert!(wait_dead(pid).await, "no queda un sleep huérfano");
        }
        assert!(!d.path().join("llama-server.pid").exists());
        assert_eq!(sup.running().await, None);
    }

    #[tokio::test]
    async fn missing_binary_or_model_is_explained_without_spawning() {
        let d = tempfile::tempdir().expect("tmp");
        let sup = quick();
        let mut s = spec(d.path(), d.path().join("nope"), "tiny");
        assert!(matches!(sup.ensure(&s).await, Err(PortError::Unavailable(m)) if m.contains("llama-server no encontrado")));
        s.binary = fake_server(d.path());
        s.model_path = d.path().join("missing.gguf");
        assert!(matches!(sup.ensure(&s).await, Err(PortError::Unavailable(m)) if m.contains("no está descargado")));
    }

    #[tokio::test]
    #[cfg_attr(not(unix), ignore = "el binario falso es un script #!/bin/sh")]
    async fn idle_stop_waits_for_requests_in_flight() {
        let d = tempfile::tempdir().expect("tmp");
        let sup = quick();
        let s = spec(d.path(), fake_server(d.path()), "tiny");
        let (_, lease) = sup.ensure(&s).await.expect("start");
        assert!(!sup.stop_if_idle(Duration::ZERO).await, "hay una petición en curso");
        drop(lease);
        assert!(!sup.stop_if_idle(Duration::from_secs(60)).await, "aún no lleva un minuto parado");
        let pid = sup.running().await.and_then(|r| r.1).expect("pid");
        assert!(sup.stop_if_idle(Duration::ZERO).await);
        assert!(wait_dead(pid).await);
        assert!(!sup.stop_if_idle(Duration::ZERO).await, "nada que parar");
    }

    #[tokio::test]
    #[cfg_attr(not(unix), ignore = "el binario falso es un script #!/bin/sh")]
    async fn an_orphan_from_a_previous_run_is_killed_but_never_an_unrelated_process() {
        let d = tempfile::tempdir().expect("tmp");
        // Huérfano: un llama-server lanzado por "otra sesión" (otro supervisor que se olvida).
        let other = quick();
        let s = spec(d.path(), fake_server(d.path()), "tiny");
        drop(other.ensure(&s).await.expect("start"));
        let orphan = other.running().await.and_then(|r| r.1).expect("pid");
        let pid_file = d.path().join("llama-server.pid");
        assert!(kill_stale(&pid_file).await);
        assert!(!pid_file.exists());
        // Su padre (el otro supervisor, en este mismo proceso) lo recoge.
        other.stop().await;
        assert!(wait_dead(orphan).await);

        // Un PID reutilizado por otra cosa (este mismo test) no se toca.
        std::fs::write(&pid_file, std::process::id().to_string()).expect("write");
        assert!(!kill_stale(&pid_file).await);
        assert!(alive(std::process::id()).await);
        assert!(!kill_stale(&d.path().join("no-such.pid")).await);
    }
}
