#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::zombie_processes)]
//! Aceptación de audio de Windows (M2, §4.3 de AGENTS.md) con TODO real:
//! sidecar `acta-win`, `WinAudioSource` sobre el protocolo v1, supervisor y
//! escritor de segmentos de `acta-audio`, con los datos en un directorio
//! temporal (nunca sobre datos de usuario) y sonido propio del sistema.
//!
//! Un único test, ignorado por defecto porque necesita tarjeta de sonido, un
//! micrófono por defecto y ~1 min:
//!
//! ```bash
//! cargo test -p acta-desktop --test windows_audio -- --ignored --nocapture
//! ```
//!
//! Fases:
//! 1. Grabación real de ~40 s: un `powershell.exe` reproduce en bucle un WAV
//!    de `C:\Windows\Media` (`SoundPlayer::PlayLooping`) y su PID es el
//!    objetivo del `capture.start`. Se exigen los artefactos de §4.3: dos
//!    pistas, rollover de segmentos de 30 s, PCM 16 kHz mono s16, cierre por
//!    `.tmp` + rename, manifiesto que lista lo escrito. (No se usa
//!    `[console]::beep`: no renderiza desde la sesión del proceso y el
//!    loopback por proceso capturaría silencio.)
//! 2. Muerte brusca: `taskkill` del sidecar a mitad de una grabación. La app
//!    no se cuelga (el cliente lo relanza y la captura se repite, o para por
//!    su cuenta conservando lo grabado), y la recuperación de arranque
//!    reconcilia lo escrito según el contrato de `acta-audio::recovery`, sin
//!    dejar ningún `.tmp` huérfano.
//! 3. Cero huérfanos: los procesos que el test lanzó (`powershell`,
//!    `acta-win`) están muertos, comprobado con `tasklist`, no asumido.
//!
//! Lo que queda dicho (S4/S5), nunca en silencio: la pista `mic` puede ser
//! silencio puro si no entra sonido por el micrófono físico y el test lo
//! avisa sin fallar; que la pista no exista entera SÍ es un fallo. Si el
//! equipo no tiene micrófono por defecto, el `capture.start` entero falla y
//! el test lo dice con su mensaje.

use acta_audio::recovery::scan_incomplete;
use acta_audio::supervisor::{
    spawn_with, SupervisorConfig, SupervisorEvent, SupervisorHandle, SupervisorPorts,
};
use acta_audio::writer::read_manifest;
use acta_core::api::SessionSnapshot;
use acta_core::detection::{
    Confidence, DetectionConfig, DetectionEvent, MeetingApp, MeetingDetector, Platform,
    SpeakerNameSource, SpeakerSignal,
};
use acta_core::ids::MeetingId;
use acta_core::layout::DataLayout;
use acta_core::library::{MeetingStore, MeetingStatus};
use acta_core::minutes::{ImportantMomentDetector, KeywordHit};
use acta_core::processing::{ProcessingPipeline, ProcessingStep, ProgressSink};
use acta_core::recording::{
    AudioFormat, AudioSource, CaptureRequest, ManifestStatus, MicTarget, PromptAnswer, SessionManifest, SessionEvent,
    SessionState, StopReason, SystemTarget, Track,
};
use acta_core::settings::{RecordingSettings, Settings};
use acta_core::time::{Clock, SystemClock, UnixMs};
use acta_core::{EventStream, PortError, PortResult};
use acta_mac_bridge::client::{SidecarClient, SidecarConfig};
use acta_store::SqliteStore;
use acta_win_bridge::audio::WinAudioSource;
use acta_win_bridge::locate::locate_sidecar;
use futures::FutureExt;
use hound::{SampleFormat, WavReader, WavSpec};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc};
use tokio::{process::Command, time::sleep};

/// Se graban ~40 s para forzar al menos un rollover del segmento de 30 s.
const RECORD_TARGET_MS: u64 = 40_000;

/// Un segmento CERRADO de 30 s mide exactamente 30 s (el escritor rellena los
/// huecos); el margen es para tolerar relojes y trims del propio escritor.
const FULL_SEGMENT_MIN: u64 = 25 * 16_000;
const FULL_SEGMENT_MAX: u64 = 31 * 16_000;

fn now() -> UnixMs {
    SystemClock.now()
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// Fakes de los puertos que esta aceptación no verifica (M3-M5 en Windows):
// construimos una reunión a mano con el PID del powershell como PID de audio.
// Copian los idioms de crates/acta-audio/tests/support.
// ---------------------------------------------------------------------------

mod shim {
    use acta_core::EventStream;
    use tokio::sync::mpsc;
    pub fn receiver_stream<T: Send + 'static>(rx: mpsc::UnboundedReceiver<T>) -> EventStream<T> {
        Box::pin(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|x| (x, rx))
        }))
    }
}

#[derive(Default)]
struct FakeDetector {
    tx: Mutex<Option<mpsc::UnboundedSender<DetectionEvent>>>,
}
impl FakeDetector {
    fn push(&self, ev: DetectionEvent) {
        lock(&self.tx)
            .as_ref()
            .expect("el detector debe estar en marcha")
            .send(ev)
            .expect("canal de detección cerrado");
    }
}
#[async_trait::async_trait]
impl MeetingDetector for FakeDetector {
    async fn start(&self, _config: DetectionConfig) -> PortResult<EventStream<DetectionEvent>> {
        let (tx, rx) = mpsc::unbounded_channel();
        *lock(&self.tx) = Some(tx);
        Ok(shim::receiver_stream(rx))
    }
    async fn stop(&self) -> PortResult<()> {
        *lock(&self.tx) = None;
        Ok(())
    }
}

/// Nombres de hablantes falsos: en Windows aún no hay adaptador (M5).
#[derive(Default)]
struct FakeSpeakers {
    tx: Mutex<Option<mpsc::UnboundedSender<SpeakerSignal>>>,
}
#[async_trait::async_trait]
impl SpeakerNameSource for FakeSpeakers {
    async fn watch(&self, _app: &MeetingApp) -> PortResult<EventStream<SpeakerSignal>> {
        let (tx, rx) = mpsc::unbounded_channel();
        *lock(&self.tx) = Some(tx);
        Ok(shim::receiver_stream(rx))
    }
    async fn unwatch(&self) -> PortResult<()> {
        *lock(&self.tx) = None;
        Ok(())
    }
}

/// Momentos importantes: el real vive en acta-llm.
struct NoMoments;
impl ImportantMomentDetector for NoMoments {
    fn detect(&self, text: &str, track: Track, at: UnixMs) -> Option<KeywordHit> {
        text.contains("importante").then(|| KeywordHit { keyword: "importante".into(), track, at })
    }
}

/// Cola de procesado falsa: registra y termina al instante (transcribir no es
/// objeto de esta aceptación; el audio ya se comprueba en los ficheros).
#[derive(Default)]
struct FakePipeline {
    runs: Mutex<Vec<MeetingId>>,
}
#[async_trait::async_trait]
impl ProcessingPipeline for FakePipeline {
    async fn run(&self, id: MeetingId, _steps: &[ProcessingStep], progress: &dyn ProgressSink) -> PortResult<()> {
        lock(&self.runs).push(id);
        progress.progress(id, ProcessingStep::Transcribing, 0.5);
        Ok(())
    }
}

struct Fakes {
    detector: Arc<FakeDetector>,
    speakers: Arc<FakeSpeakers>,
    pipeline: Arc<FakePipeline>,
}
impl Fakes {
    fn new() -> Self {
        Self { detector: Arc::default(), speakers: Arc::default(), pipeline: Arc::default() }
    }
}

fn ports(layout: &DataLayout, audio: WinAudioSource, fakes: &Fakes, store: Arc<SqliteStore>) -> SupervisorPorts {
    SupervisorPorts {
        layout: layout.clone(),
        clock: Arc::new(SystemClock),
        audio: Arc::new(audio),
        detector: fakes.detector.clone(),
        speakers: fakes.speakers.clone(),
        screen: None,
        clipboard: None,
        live: None,
        live_asr: None,
        live_sessions: Arc::default(),
        moments: Arc::new(NoMoments),
        store,
        pipeline: fakes.pipeline.clone(),
        disk_free: Arc::new(|_| Some(500 * 1024 * 1024 * 1024)),
    }
}

fn meeting_app(pid: u32) -> MeetingApp {
    MeetingApp {
        platform: Platform::Teams,
        bundle_id: "test.acta-win-audio".into(),
        pid: pid as i32,
        audio_pids: vec![pid as i32],
        name: "PowerShell (fuente de prueba)".into(),
        title: Some("Reunión de prueba".into()),
        url: None,
    }
}

// ---------------------------------------------------------------------------
// Procesos: todo lo que lanza el test se mata y se espera, también si el test
// falla (catch_unwind + quiesce). El sidecar lo supervisa `SidecarClient`.
// ---------------------------------------------------------------------------

#[derive(Default)]
struct Janitor {
    children: tokio::sync::Mutex<Vec<tokio::process::Child>>,
    clients: tokio::sync::Mutex<Vec<SidecarClient>>,
    /// (pid, qué es) para la comprobación final con `tasklist`.
    watched: Mutex<Vec<(u32, &'static str)>>,
}
impl Janitor {
    fn watch(&self, pid: u32, what: &'static str) {
        lock(&self.watched).push((pid, what));
    }
    async fn keep_child(&self, child: tokio::process::Child, what: &'static str) -> u32 {
        let pid = child.id().expect("el hijo recién lanzado tiene PID");
        self.watch(pid, what);
        self.children.lock().await.push(child);
        pid
    }
    /// Para sidecars (shutdown ordenado) y mata y espera a todos los hijos.
    async fn quiesce(&self) {
        for client in self.clients.lock().await.drain(..) {
            let _ = client.shutdown().await;
        }
        let mut children = self.children.lock().await;
        for child in children.iter_mut() {
            let _ = child.start_kill();
        }
        for child in children.iter_mut() {
            let _ = child.wait().await;
        }
    }
}

/// WAV propio del sistema como fuente sonora (§4.1: nunca grabaciones de
/// terceros).
fn pick_source_wav() -> PathBuf {
    const CANDIDATES: [&str; 8] = [
        "ding.wav", "Ring05.wav", "notify.wav", "chimes.wav", "Alarm01.wav", "recycle.wav",
        "Speech On.wav", "chord.wav",
    ];
    for name in CANDIDATES {
        let p = Path::new(r"C:\Windows\Media").join(name);
        if p.is_file() {
            return p;
        }
    }
    panic!("no hay ningún WAV en C:\\Windows\\Media para usar como fuente sonora");
}

/// Localizador para el test: usa el orden normal del adaptador (env,
/// ejecutable, `sidecars/acta-win/target`) y añade el `target/` del workspace
/// de la raíz, donde cae `cargo build -p acta-win` en desarrollo.
fn locate_sidecar_binary() -> PathBuf {
    if let Ok(p) = locate_sidecar(None) {
        return p;
    }
    // `src-tauri` está tres niveles bajo la raíz del repo.
    let root_target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../target");
    for profile in ["debug", "release"] {
        let p = root_target.join(profile).join("acta-win.exe");
        if p.is_file() {
            return p;
        }
    }
    panic!(
        "sidecar acta-win no encontrado: compílalo (cargo build -p acta-win). Se buscó con locate_sidecar y en {root_target:?}\\{{debug,release}}"
    );
}

async fn spawn_sound_player(janitor: &Janitor, wav: &Path) -> u32 {
    // `PlayLooping` es asíncrono: el proceso sigue vivo reproduciendo en bucle
    // hasta el Sleep. El audio se renderiza EN el proceso, así que el loopback
    // de ese proceso lo captura.
    let script = format!(
        "$p = New-Object System.Media.SoundPlayer '{}'; $p.PlayLooping(); Start-Sleep -Seconds 600",
        wav.display()
    );
    let child = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("no se pudo lanzar powershell.exe como fuente sonora");
    janitor.keep_child(child, "powershell.exe (SoundPlayer en bucle)").await
}

// Logs de sidecar en una ruta PERSISTENTE (el tempdir muere con el test):
// sirve para depurar esta aceptación ejecutándola con --nocapture.
fn log_fase(nombre: &str) -> PathBuf {
    let dir = std::env::temp_dir().join("acta-windows-audio-test");
    let _ = std::fs::create_dir_all(&dir);
    dir.join(nombre)
}

async fn start_sidecar(janitor: &Janitor, log_file: PathBuf, what: &'static str) -> SidecarClient {
    let binary = locate_sidecar_binary();
    let config = SidecarConfig { binary, log_file: Some(log_file), ..SidecarConfig::default() };
    let client = SidecarClient::start(config)
        .await
        .expect("el sidecar acta-win no arrancó ni contestó hello");
    if let Some(pid) = client.pid() {
        janitor.watch(pid, what);
    }
    janitor.clients.lock().await.push(client.clone());
    client
}

async fn taskkill(pid: u32) {
    let out = Command::new("taskkill.exe")
        .args(["/F", "/PID", &pid.to_string()])
        .output()
        .await
        .expect("taskkill no se pudo ejecutar");
    assert!(
        out.status.success(),
        "taskkill /F /PID {pid} falló: {}",
        String::from_utf8_lossy(&out.stdout).trim()
    );
}

/// (imagen, pid) de los procesos vivos según `tasklist`.
async fn tasklist_pids() -> Vec<(String, u32)> {
    let out = Command::new("tasklist.exe")
        .args(["/FO", "CSV", "/NH"])
        .output()
        .await
        .expect("tasklist no se pudo ejecutar");
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .filter_map(|line| {
            // "nombre","pid","sesión","sesión#","memoria"
            let mut fields = line.split("\",\"");
            let name = fields.next()?.trim_start_matches('"').to_owned();
            let pid = fields.next()?.trim_matches('"').parse::<u32>().ok()?;
            Some((name, pid))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Trazas: stderr + copia capturada, para comprobar los avisos (nunca en silencio)
// ---------------------------------------------------------------------------

#[derive(Clone, Default)]
struct TraceTee {
    captured: Arc<Mutex<Vec<u8>>>,
}

struct TraceTeeWriter {
    captured: Arc<Mutex<Vec<u8>>>,
}

impl std::io::Write for TraceTeeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        // Stderr primero (útil para depurar con --nocapture) y copia después.
        let n = std::io::stderr().lock().write(buf)?;
        lock(&self.captured).extend_from_slice(&buf[..n]);
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stderr().flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for TraceTee {
    type Writer = TraceTeeWriter;
    fn make_writer(&'a self) -> Self::Writer {
        TraceTeeWriter { captured: Arc::clone(&self.captured) }
    }
}

fn trace_count(logs: &TraceTee, marker: &str) -> usize {
    std::str::from_utf8(&lock(&logs.captured))
        .unwrap_or_default()
        .matches(marker)
        .count()
}

// ---------------------------------------------------------------------------
// Esperas con límite y mensajes útiles
// ---------------------------------------------------------------------------

async fn wait_for_event<F>(
    rx: &mut broadcast::Receiver<SupervisorEvent>,
    mut pred: F,
    what: &str,
    limit: Duration,
) -> SupervisorEvent
where
    F: FnMut(&SupervisorEvent) -> bool,
{
    let start = Instant::now();
    loop {
        match rx.try_recv() {
            Ok(ev) if pred(&ev) => return ev,
            Ok(_) | Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(broadcast::error::TryRecvError::Empty) => {
                assert!(start.elapsed() < limit, "no llegó: {what} (límite {limit:?})");
                sleep(Duration::from_millis(50)).await;
            }
            Err(broadcast::error::TryRecvError::Closed) => {
                panic!("canal de eventos cerrado esperando: {what}")
            }
        }
    }
}

async fn wait_state(
    rx: &mut broadcast::Receiver<SupervisorEvent>,
    name: &str,
    limit: Duration,
) -> SessionSnapshot {
    match wait_for_event(
        rx,
        |e| matches!(e, SupervisorEvent::Snapshot(s) if s.state.name() == name),
        &format!("estado {name}"),
        limit,
    )
    .await
    {
        SupervisorEvent::Snapshot(s) => s,
        other => panic!("evento inesperado: {other:?}"),
    }
}

async fn wait_elapsed(h: &SupervisorHandle, target_ms: u64, limit: Duration) {
    let start = Instant::now();
    loop {
        let snap = h.snapshot().await.expect("el supervisor no responde");
        if snap.elapsed_ms >= target_ms {
            return;
        }
        assert_eq!(
            snap.state.name(),
            "recording",
            "la grabación se cortó antes de los {target_ms} ms"
        );
        assert!(start.elapsed() < limit, "no se grabaron {target_ms} ms en {limit:?}");
        sleep(Duration::from_millis(200)).await;
    }
}

fn recording_id_of(snap: &SessionSnapshot) -> MeetingId {
    match &snap.state {
        SessionState::Recording(r) => r.meeting_id,
        other => panic!("no está grabando: {other:?}"),
    }
}

/// Deteción fake → prompt "¿Grabar?" → aceptar. Devuelve el id de la reunión.
async fn detected_then_accept(
    fakes: &Fakes,
    h: &SupervisorHandle,
    rx: &mut broadcast::Receiver<SupervisorEvent>,
    app: MeetingApp,
) -> MeetingId {
    fakes
        .detector
        .push(DetectionEvent::MeetingStarted { app, confidence: Confidence::High, at: now() });
    let snap = wait_state(rx, "detected", Duration::from_secs(15)).await;
    let SessionState::Detected { prompt, .. } = snap.state else {
        panic!("el snapshot detected no trae prompt");
    };
    let snap = h
        .send(SessionEvent::PromptAnswered { prompt_id: prompt.id, answer: PromptAnswer::Accept, at: now() })
        .await
        .expect("aceptar el prompt de grabación falló");
    recording_id_of(&snap)
}

/// El puente temporal de PID (ver la documentación del módulo de
/// `acta-win-bridge`), comprobado sobre el sidecar real:
/// 1. `system` activo sin PID y sin origen → `PortError::Unavailable` con el
///    mensaje completo (qué falta, cuándo llega y cómo forzarlo);
/// 2. `system` activo sin PID pero con el puente configurado → `capture.start`
///    aceptado (con aviso, una sola vez por arranque) y `system_pids` en eco;
/// 3. los bundle_ids del supervisor (lado macOS) se descartan con aviso y la
///    captura sale por PID.
async fn assert_pid_bridge(client: &SidecarClient, ps_pid: u32, logs: &TraceTee) {
    let request = |pids: &[i32], bundle_ids: &[&str]| CaptureRequest {
        meeting_id: MeetingId::new(),
        format: AudioFormat::WORKING,
        frame_ms: 100,
        mic: MicTarget { enabled: true, device_uid: None },
        system: SystemTarget {
            enabled: true,
            pids: pids.to_vec(),
            bundle_ids: bundle_ids.iter().map(|s| (*s).to_owned()).collect(),
        },
    };

    // 1) Sin origen: el error debe explicar el cómo y el cuándo, nunca fallar callado.
    let err = match WinAudioSource::new(client.clone()).start(request(&[], &[])).await {
        Ok(_) => panic!("sin PID ni origen capture.start debería fallar con Unavailable"),
        Err(e) => e,
    };
    let PortError::Unavailable(msg) = err else {
        panic!("se esperaba PortError::Unavailable, es {err:?}");
    };
    assert!(
        msg.contains("PID") && msg.contains("M3") && msg.contains("ACTA_WIN_PIDS"),
        "el mensaje debe explicar qué falta (PID), cuándo llega (M3) y cómo forzarlo: {msg}"
    );

    // 2) Con el puente configurado: capture.start aceptado con los PID del puente.
    let before = trace_count(logs, "puente temporal de PID");
    let source = WinAudioSource::with_system_pids(client.clone(), vec![ps_pid as i32]);
    let (started, _) = source
        .start(request(&[], &[]))
        .await
        .expect("el puente de PID no sustituyó los PID ausentes en capture.start");
    assert_eq!(started.system_pids, vec![ps_pid as i32]);
    assert_eq!(
        trace_count(logs, "puente temporal de PID"),
        before + 1,
        "el uso del puente debe avisar una sola vez por arranque"
    );
    source.stop().await.expect("stop del caso con puente falló");

    // 3) El supervisor siempre envía bundle_ids (lado macOS): descartados con
    //    aviso, la captura sale por PID.
    let before = trace_count(logs, "se descartan los bundle_ids");
    let source = WinAudioSource::new(client.clone());
    let (started, _) = source
        .start(request(&[ps_pid as i32], &["test.acta-win-audio"]))
        .await
        .expect("los bundle_ids no soportados bloquearon una captura con PID válido");
    assert_eq!(started.system_pids, vec![ps_pid as i32]);
    assert_eq!(
        trace_count(logs, "se descartan los bundle_ids"),
        before + 1,
        "el descarte de bundle_ids debe avisar"
    );
    source.stop().await.expect("stop del caso bundle_ids falló");
}

// ---------------------------------------------------------------------------
// Artefactos de una grabación cerrada con éxito
// ---------------------------------------------------------------------------

fn wav_files(dir: &Path, what: &str) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{what}: no se pudo listar {}: {e}", dir.display()))
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "wav"))
        .collect();
    v.sort();
    v
}

struct WavContents {
    spec: WavSpec,
    /// Muestras (mono: una muestra por frame).
    frames: u64,
    peak_abs: u16,
}

fn read_wav(path: &Path, what: &str) -> WavContents {
    let mut reader = WavReader::open(path)
        .unwrap_or_else(|e| panic!("{what}: {} no es un WAV legible: {e}", path.display()));
    let spec = reader.spec();
    let mut frames = 0u64;
    let mut peak_abs = 0u16;
    for sample in reader.samples::<i16>() {
        let s = sample.unwrap_or_else(|e| panic!("{what}: {}: muestra ilegible: {e}", path.display()));
        frames += 1;
        peak_abs = peak_abs.max(s.unsigned_abs());
    }
    WavContents { spec, frames, peak_abs }
}

/// Ficheros con sufijos sospechosos (`.tmp`, `.installing`) bajo un directorio.
fn files_with_suffix(dir: &Path, suffixes: &[&str], out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            files_with_suffix(&p, suffixes, out);
        } else if let Some(name) = p.file_name().and_then(|n| n.to_str())
            && suffixes.iter().any(|s| name.ends_with(s))
        {
            out.push(p);
        }
    }
}

fn assert_no_partial_files(meeting_dir: &Path, what: &str) {
    let mut partials = Vec::new();
    files_with_suffix(meeting_dir, &[".tmp", ".installing"], &mut partials);
    assert!(
        partials.is_empty(),
        "{what}: quedan ficheros a medias (el cierre es .tmp + fsync + rename): {partials:?}"
    );
}

fn track_peak_index(track: Track) -> usize {
    match track {
        Track::Mic => 0,
        Track::System => 1,
    }
}

/// Comprueba los artefactos de §4.3 de una grabación cerrada con éxito. Cada
/// aserción es independiente y dice qué falló. Devuelve el pico por pista
/// (`|muestra|` máxima, 0 = silencio puro) para el aviso del micrófono.
fn assert_accepted_artifacts(layout: &DataLayout, id: MeetingId) -> [(Track, u16); 2] {
    let meeting = layout.meeting_dir(id);
    let manifest =
        read_manifest(layout, id).expect("no se pudo leer el manifest.json: la grabación no dejó manifiesto");
    assert_eq!(manifest.status, ManifestStatus::Stopped, "el manifiesto no quedó cerrado");
    assert_eq!(manifest.tracks.len(), 2, "el manifiesto debe traer mic y system");

    let mut peaks = [0u16; 2];
    for track in Track::ALL {
        let what = format!("pista {}", track.as_str());
        let dir = layout.track_dir(id, track);
        assert!(dir.is_dir(), "{what}: falta la carpeta {}", dir.display());
        let files = wav_files(&dir, &what);
        assert!(
            files.len() >= 2,
            "{what}: se esperaban ≥ 2 segmentos (rollover de 30 s en ~40 s) y hay {}",
            files.len()
        );

        for (i, path) in files.iter().enumerate() {
            let c = read_wav(path, &what);
            assert_eq!(
                (
                    c.spec.sample_rate,
                    c.spec.channels,
                    c.spec.bits_per_sample,
                    c.spec.sample_format
                ),
                (16_000u32, 1u16, 16u16, SampleFormat::Int),
                "{what}: {} no es PCM 16 kHz mono s16 (spec: {:?})",
                path.display(),
                c.spec
            );
            if i + 1 < files.len() {
                assert!(
                    c.frames >= FULL_SEGMENT_MIN && c.frames <= FULL_SEGMENT_MAX,
                    "{what}: el segmento intermedio {} mide {} muestras ({} ms), esperado entre 25 y 31 s",
                    path.display(),
                    c.frames,
                    c.frames * 1000 / 16_000
                );
            } else {
                assert!(c.frames > 0, "{what}: el último segmento está vacío (0 muestras)");
            }
            println!(
                "{} · {} muestras ({:.3} s) · {} Hz, {} ch, {} bits, {:?}, pico |i16|={} : {}",
                what,
                c.frames,
                c.frames as f64 / 16_000.0,
                c.spec.sample_rate,
                c.spec.channels,
                c.spec.bits_per_sample,
                c.spec.sample_format,
                c.peak_abs,
                path.file_name().and_then(|n| n.to_str()).unwrap_or_default(),
            );
            peaks[track_peak_index(track)] = peaks[track_peak_index(track)].max(c.peak_abs);
        }

        // El manifiesto menciona exactamente los segmentos que hay en disco.
        let track_manifest = manifest
            .tracks
            .iter()
            .find(|t| t.track == track)
            .expect("el manifiesto trae las dos pistas");
        assert_eq!(
            track_manifest.segments.len(),
            files.len(),
            "{what}: el manifiesto lista {} segmentos pero en disco hay {}",
            track_manifest.segments.len(),
            files.len()
        );
        for (segment, path) in track_manifest.segments.iter().zip(&files) {
            assert!(
                segment.samples > 0,
                "{what}: el segmento {} del manifiesto está vacío",
                segment.file
            );
            assert!(
                path.file_name().is_some_and(|n| segment.file.ends_with(&*n.to_string_lossy())),
                "{what}: el manifiesto no corresponde con el disco: {} vs {}",
                segment.file,
                path.display()
            );
        }
    }

    assert_no_partial_files(&meeting, "grabación cerrada con éxito");

    // La pista `system` es la que prueba el loopback del proceso: un bucle de
    // sonido activo en 40 s no puede dejar todo ceros. La `mic` sí puede.
    let mic_peak = peaks[track_peak_index(Track::Mic)];
    let system_peak = peaks[track_peak_index(Track::System)];
    assert!(
        system_peak > 0,
        "la pista system salió en silencio puro: el loopback del proceso no capturó audio"
    );
    if mic_peak == 0 {
        eprintln!("AVISO (no es fallo): la pista mic salió en silencio puro (no entró sonido por el micrófono en esta máquina).");
    }
    eprintln!(
        "Picos medidos |i16|: mic = {mic_peak} ({:.1} dBFS), system = {system_peak} ({:.1} dBFS)",
        20.0 * (f64::from(mic_peak.max(1)) / 32_768.0).log10(),
        20.0 * (f64::from(system_peak.max(1)) / 32_768.0).log10()
    );
    [
        (Track::Mic, mic_peak),
        (Track::System, system_peak),
    ]
}

fn closed_segments(layout: &DataLayout, id: MeetingId, track: Track) -> usize {
    read_manifest(layout, id)
        .expect("no se pudo leer el manifiesto")
        .tracks
        .iter()
        .find(|t| t.track == track)
        .expect("pista en el manifiesto")
        .segments
        .len()
}

fn track_samples_sum(manifest: &SessionManifest, track: Track) -> u64 {
    manifest
        .tracks
        .iter()
        .find(|t| t.track == track)
        .map(|t| t.segments.iter().map(|s| s.samples).sum())
        .unwrap_or(0)
}

async fn eventually<F>(mut f: F, what: &str, limit: Duration)
where
    F: AsyncFnMut() -> Option<String>,
{
    let start = Instant::now();
    loop {
        match f().await {
            None => return,
            Some(msg) => {
                assert!(start.elapsed() < limit, "no se cumplió a tiempo ({what}): {msg}");
                sleep(Duration::from_millis(200)).await;
            }
        }
    }
}
// ---------------------------------------------------------------------------
// El test
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "necesita tarjeta de sonido, micrófono por defecto y ~1 min (audio real)"]
async fn windows_audio_acceptance() {
    // INFO a stderr (con copia capturada para los avisos): la supervisión del
    // sidecar avisa ahí de muertes y relanzamientos; es lo que da cara a un
    // fallo de la fase 1.
    let logs = TraceTee::default();
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_writer(logs.clone())
        .try_init();

    let janitor = Arc::new(Janitor::default());
    let result = std::panic::AssertUnwindSafe(async { run(&janitor, &logs).await })
        .catch_unwind()
        .await;
    janitor.quiesce().await;

    // Cero huérfanos, comprobado con tasklist (también cuando el test falla).
    let alive = tasklist_pids().await;
    let watched = lock(&janitor.watched).drain(..).collect::<Vec<_>>();
    for (pid, what) in &watched {
        assert!(
            !alive.iter().any(|(_, p)| p == pid),
            "huérfano: {what} (PID {pid}) sigue vivo tras el test"
        );
    }
    let sidecar_alive: Vec<&(String, u32)> =
        alive.iter().filter(|(n, _)| n.eq_ignore_ascii_case("acta-win.exe")).collect();
    assert!(
        sidecar_alive.is_empty(),
        "quedan procesos acta-win.exe vivos: {sidecar_alive:?}"
    );
    eprintln!(
        "Procesos al final: 0 huérfanos de los lanzados por el test ({} PIDs vigilados) y ninguna instancia de acta-win.exe.",
        watched.len()
    );

    if let Err(payload) = result {
        std::panic::resume_unwind(payload);
    }
}

async fn run(janitor: &Arc<Janitor>, logs: &TraceTee) {
    let dir = tempfile::tempdir().expect("no se pudo crear el directorio temporal del test");
    let layout = DataLayout::new(dir.path());
    std::fs::create_dir_all(layout.logs_dir()).expect("no se pudo crear logs/");

    println!(
        "Sidecar: {}",
        locate_sidecar_binary().display()
    );
    let wav = pick_source_wav();
    println!("Fuente sonora: {}", wav.display());
    let ps_pid = spawn_sound_player(janitor, &wav).await;
    println!("powershell (SoundPlayer en bucle): PID {ps_pid}");

    // --- Fase 1: grabación real de ~40 s con sonido de verdad --------------
    let client =
        start_sidecar(janitor, log_fase("acta-win-fase1.log"), "sidecar acta-win (fase 1)").await;
    let store = Arc::new(
        SqliteStore::open(&layout.db_path())
            .expect("no se pudo abrir la base de datos del directorio temporal")
            .with_layout(layout.clone()),
    );
    let fakes = Fakes::new();
    let audio = WinAudioSource::new(client.clone());
    let handle = spawn_with(ports(&layout, audio, &fakes, store.clone()), Settings::default(), SupervisorConfig::default())
        .await
        .expect("el supervisor no arrancó");
    let mut rx = handle.subscribe();
    let id = detected_then_accept(&fakes, &handle, &mut rx, meeting_app(ps_pid)).await;
    println!("Fase 1: grabación {id} en marcha (capture.start dirigido al PID {ps_pid})");

    wait_elapsed(&handle, RECORD_TARGET_MS, Duration::from_secs(150)).await;
    handle
        .send(SessionEvent::StopRequested { reason: StopReason::User, at: now() })
        .await
        .expect("parar la grabación falló");
    wait_state(&mut rx, "ready", Duration::from_secs(60)).await;

    let peaks = assert_accepted_artifacts(&layout, id);
    let meeting = store
        .get_meeting(id)
        .await
        .expect("la base de datos no respondió")
        .expect("la reunión no está en la base de datos");
    assert_eq!(meeting.status, MeetingStatus::Ready, "la reunión no terminó en Ready");
    println!(
        "Fase 1 OK: {} cerrada como Ready; picos |i16| mic = {}, system = {}",
        id, peaks[0].1, peaks[1].1
    );

    // --- Fase 1.b: puente temporal de PID sobre el sidecar real ----------
    // El sidecar de la fase 1 queda idle tras cerrar la grabación: los tres
    // casos del puente alternan `capture.start`/`capture.stop` sobre él.
    assert_pid_bridge(&client, ps_pid, logs).await;
    println!("Fase 1.b OK: puente de PID (sin origen explicó, con origen capturó, bundle_ids descartados con aviso)");

    handle.shutdown().await.expect("apagar el supervisor de la fase 1 falló");
    client.shutdown().await.expect("apagar el sidecar de la fase 1 falló");

    // --- Fase 2: taskkill del sidecar + muerte brusca de la app ------------
    // La grabación de la fase 2 corre en un runtime current_thread propio de
    // un hilo de SO (dentro del runtime del test no se puede construir otro):
    // al soltarlo (drop) muere la "app" en marcha con un `.tmp` a medias y el
    // manifiesto en `recording`.
    let janitor_fase2 = Arc::clone(janitor);
    let root_fase2 = dir.path().to_path_buf();
    let crash = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime de la fase 2");
        rt.block_on(phase_2_kill(&janitor_fase2, &root_fase2, ps_pid))
    })
    .join()
    .expect("el hilo de la fase 2 murió antes de terminar");

    assert_crash_state(&layout, crash.meeting_id, crash.closed_before_kill).await;
    recovered_by_startup(
        dir.path(),
        crash.meeting_id,
        crash.closed_before_kill,
        crash.closed_before_kill_mic,
        crash.client,
    )
    .await;
}

struct Phase2Out {
    meeting_id: MeetingId,
    /// Segmentos de `system` ya cerrados en el manifiesto justo tras el
    /// taskkill (lo escrito ANTES de morir debe sobrevivir a la recuperación).
    closed_before_kill: usize,
    /// Igual para `mic`: la supervivencia se comprueba en las dos pistas.
    closed_before_kill_mic: usize,
    /// Manija del sidecar, muerto con la "app": la recuperación de arranque no
    /// lo necesita vivo y no se lanza un sidecar nuevo para esto.
    client: SidecarClient,
}

/// Fase 2a: grabación en marcha, `taskkill` del sidecar a mitad. La app no se
/// cuelga: responde al instante y la grabación continúa (el cliente relanza
/// el sidecar y `WinAudioSource` repite `capture.start`) o para por su cuenta
/// conservando lo grabado si el cliente se rinde.
async fn phase_2_kill(janitor: &Janitor, root: &Path, ps_pid: u32) -> Phase2Out {
    let layout = DataLayout::new(root);
    let client = start_sidecar(
        janitor,
        log_fase("acta-win-fase2.log"),
        "sidecar acta-win (fase 2, taskkill)",
    )
    .await;
    let store = Arc::new(
        SqliteStore::open(&layout.db_path())
            .expect("no se pudo abrir la base de datos en la fase 2")
            .with_layout(layout.clone()),
    );
    let fakes = Fakes::new();
    let audio = WinAudioSource::new(client.clone());
    let mut settings = Settings::default();
    // Segmentos de 1 s: con ~3 s de grabación ya hay segmentos cerrados y el
    // `.tmp` del segmento en curso (lo que la recuperación debe reconciliar).
    settings.recording = RecordingSettings { segment_seconds: 1, ..settings.recording.clone() };
    let handle = spawn_with(ports(&layout, audio, &fakes, store), settings, SupervisorConfig::default())
        .await
        .expect("el supervisor de la fase 2 no arrancó");
    let mut rx = handle.subscribe();
    let id = detected_then_accept(&fakes, &handle, &mut rx, meeting_app(ps_pid)).await;
    wait_elapsed(&handle, 2_500, Duration::from_secs(60)).await;

    let sidecar_pid = client
        .pid()
        .expect("el sidecar de la fase 2 no está vivo justo antes del taskkill");
    println!("Fase 2a: taskkill del sidecar (PID {sidecar_pid}) con la grabación {id} en marcha");
    taskkill(sidecar_pid).await;

    let closed_before = closed_segments(&layout, id, Track::System);
    let closed_mic_before = closed_segments(&layout, id, Track::Mic);
    let deadline = Instant::now() + Duration::from_secs(60);
    let outcome;
    loop {
        // El supervisor responde: no está muerto ni bloqueado por la muerte.
        let snap = tokio::time::timeout(Duration::from_secs(5), handle.snapshot())
            .await
            .expect("el supervisor estaba colgado tras matar el sidecar (taskkill)")
            .expect("el supervisor devolvió error tras matar el sidecar");
        match snap.state.name() {
            // Se rindió: paró por su cuenta y conserva lo grabado (encola el
            // procesado).
            "processing" | "ready" | "failed" => {
                outcome = format!("paró por su cuenta en {}", snap.state.name());
                break;
            }
            _ => {
                let closed = closed_segments(&layout, id, Track::System);
                if closed > closed_before {
                    outcome = format!(
                        "sigue grabando tras el relanzamiento ({closed} segmentos cerrados vs {closed_before})"
                    );
                    break;
                }
            }
        }
        assert!(
            Instant::now() < deadline,
            "tras el taskkill la grabación no continuó ni se paró en 60 s (estado {})",
            snap.state.name()
        );
        sleep(Duration::from_millis(250)).await;
    }
    println!("Fase 2a OK: la app no se cuelga — {outcome}");
    // El sidecar relanzado por el cliente también queda vigilado para la
    // comprobación de huérfanos (morirá con el drop del runtime).
    if let Some(relaunched) = client.pid() {
        janitor.watch(relaunched, "sidecar acta-win (fase 2, relanzado)");
    }
    // NO se para la grabación: el drop del runtime la mata como un crash.
    Phase2Out {
        meeting_id: id,
        closed_before_kill: closed_before,
        closed_before_kill_mic: closed_mic_before,
        client,
    }
}

/// La muerte brusca dejó el estado de crash genuino: manifiesto en
/// `recording` (pendiente de recuperación) y algún `.wav.tmp` a medias.
async fn assert_crash_state(layout: &DataLayout, id: MeetingId, closed_before: usize) {
    let incomplete = scan_incomplete(layout).expect("scan_incomplete falló");
    assert_eq!(
        incomplete,
        vec![id],
        "la reunión muerta bruscamente debería estar pendiente de recuperación"
    );
    let mut tmps = Vec::new();
    files_with_suffix(&layout.track_dir(id, Track::Mic), &[".tmp"], &mut tmps);
    files_with_suffix(&layout.track_dir(id, Track::System), &[".tmp"], &mut tmps);
    assert!(
        !tmps.is_empty(),
        "sin el crash no hay `.wav.tmp` a medias: la simulación de muerte brusca no dejó trabajo para la recuperación"
    );
    assert!(
        closed_before > 0,
        "no había segmentos cerrados antes del crash: lo escrito antes de morir no puede sobrevivir si no existía"
    );
    println!(
        "Muerte brusca confirmada: manifiesto en recording, {} `.wav.tmp` a medias, {} segmentos ya cerrados",
        tmps.len(),
        closed_before
    );
}

/// La recuperación de arranque (contrato de `acta-audio::recovery`) reconcilia
/// lo escrito y no queda ningún `.tmp` huérfano.
async fn recovered_by_startup(
    root: &Path,
    id: MeetingId,
    closed_before_kill: usize,
    closed_mic_before_kill: usize,
    client: SidecarClient,
) {
    let layout = DataLayout::new(root);
    let store = Arc::new(
        SqliteStore::open(&layout.db_path())
            .expect("no se pudo reabrir la base de datos")
            .with_layout(layout.clone()),
    );
    let fakes = Fakes::new();
    // El sidecar de la fase 2 murió con la "app": no se necesita vivo (la
    // recuperación se hace al arrancar, sin capturar) y NO se lanza otro.
    let audio = WinAudioSource::new(client);
    let handle = spawn_with(ports(&layout, audio, &fakes, store.clone()), Settings::default(), SupervisorConfig::default())
        .await
        .expect("el supervisor de recuperación no arrancó");
    let mut rx = handle.subscribe();

    eventually(
        async || {
            match store.get_meeting(id).await.expect("la base de datos no respondió") {
                Some(m) if m.recovered => None,
                Some(_) => Some("la reunión existe pero aún no está marcada como recuperada".into()),
                None => Some("la reunión no existe en la base de datos".into()),
            }
        },
        "reunión recuperada",
        Duration::from_secs(90),
    )
    .await;
    wait_state(&mut rx, "ready", Duration::from_secs(120)).await;
    handle.shutdown().await.expect("apagar el supervisor de recuperación falló");

    let manifest = read_manifest(&layout, id).expect("no hay manifiesto tras la recuperación");
    assert_eq!(manifest.status, ManifestStatus::Stopped, "la recuperación no cerró el manifiesto");
    assert_eq!(
        manifest.stop_reason,
        Some(StopReason::Error),
        "una muerte brusca debe quedar como stop_reason Error"
    );
    for track in Track::ALL {
        let what = format!("pista {} recuperada", track.as_str());
        let dir = layout.track_dir(id, track);
        let files = wav_files(&dir, &what);
        assert!(!files.is_empty(), "{what}: la recuperación perdió todos los segmentos");
        let track_manifest = manifest
            .tracks
            .iter()
            .find(|t| t.track == track)
            .expect("pista en el manifiesto recuperado");
        assert_eq!(
            track_manifest.segments.len(),
            files.len(),
            "{what}: el manifiesto recuperado no concuerda con el disco ({} vs {})",
            track_manifest.segments.len(),
            files.len()
        );
        for path in &files {
            let c = read_wav(path, &what);
            assert_eq!(
                (
                    c.spec.sample_rate,
                    c.spec.channels,
                    c.spec.bits_per_sample,
                    c.spec.sample_format
                ),
                (16_000u32, 1u16, 16u16, SampleFormat::Int),
                "{what}: {} no es PCM 16 kHz mono s16 tras la recuperación",
                path.display()
            );
            println!(
                "{} · {} muestras ({:.3} s) · {} Hz, {} ch, {} bits, {:?} : {}",
                what,
                c.frames,
                c.frames as f64 / 16_000.0,
                c.spec.sample_rate,
                c.spec.channels,
                c.spec.bits_per_sample,
                c.spec.sample_format,
                path.file_name().and_then(|n| n.to_str()).unwrap_or_default(),
            );
        }
    }
    // Lo escrito antes de morir sobrevivió: cada pista tiene al menos los
    // segmentos que ya estaban cerrados en el manifiesto antes del taskkill.
    // En `system` y en `mic` por igual: la supervivencia no es solo de la
    // pista que el bucle del taskkill usa para detectar el relanzamiento.
    for (what, track, closed) in
        [("system", Track::System, closed_before_kill), ("mic", Track::Mic, closed_mic_before_kill)]
    {
        assert!(closed > 0, "no había segmentos cerrados de {what} antes del crash");
        let samples = track_samples_sum(&manifest, track);
        assert!(
            samples >= u64::try_from(closed).expect("recuento de segmentos") * 16_000,
            "la recuperación perdió audio de {what}: {samples} muestras tras el crash, pero {closed} segmentos ya estaban cerrados"
        );
    }
    assert_no_partial_files(&layout.meeting_dir(id), "reunión recuperada");
    let pending = scan_incomplete(&layout).expect("scan_incomplete falló tras la recuperación");
    assert!(pending.is_empty(), "quedan reuniones pendientes de recuperación: {pending:?}");
    let system_samples = track_samples_sum(&manifest, Track::System);
    let mic_samples = track_samples_sum(&manifest, Track::Mic);
    println!(
        "Fase 2b OK: recuperación de arranque reconcilió {system_samples} muestras de system y {mic_samples} de mic, y dejó cero `.tmp` huérfanos"
    );
}
