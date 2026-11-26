//! `AudioSource` sobre el sidecar `acta-win`: `capture.*` + eventos `audio.*`.
//!
//! Espejo de `MacAudioSource` (acta-mac-bridge) sobre el mismo protocolo v1:
//! - Los frames llegan en base64 s16le y se entregan como `Vec<i16>`.
//! - Si el sidecar muere y el cliente lo relanza, se repite `capture.start`
//!   (con los PIDs vigentes) y, si estaba en pausa, `capture.pause`; después se
//!   emite `AudioEvent::Restarted` con el nuevo origen de `pts_us`.
//! - Si el cliente se rinde: `TrackState::Failed` en ambas pistas y fin.

use acta_mac_bridge::client::{SidecarClient, SidecarMessage};
use acta_mac_bridge::protocol::{
    AudioFrameEvent, CaptureRetargetArgs, CaptureStartArgs, CaptureStartResult, Command, MicArgs, SystemArgs,
    TrackName, TrackStateName,
};
use crate::task::{Slot, spawn_stream};
use acta_core::recording::{AudioEvent, AudioFrame, AudioSource, CaptureRequest, CaptureStarted, SystemTarget, Track, TrackState};
use acta_core::time::UnixMs;
use acta_core::{EventStream, PortError, PortResult};
use async_trait::async_trait;
use base64::Engine;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{mpsc, oneshot};

const START_TIMEOUT: Duration = Duration::from_secs(20);

/// Puente temporal hasta M3 (adaptador de detección de reuniones en Windows).
///
/// El sidecar exige al menos un PID para la pista `system` y no admite la
/// dirección por bundle_ids (solo loopback por PID); el supervisor los envía
/// siempre (viene del lado macOS) y en Windows nadie incluye los PID de la app
/// de reunión, porque eso será tarea de la detección de reuniones (M3).
/// Mientras tanto, y de forma explícita y ruidosa (nunca en silencio, S4):
///
/// - Si el `capture.start` pide sistema sin PID, se obtienen de un origen
///   explícito: la configuración del adaptador (`WinAudioSource::new` sin
///   parámetros no lo usa → el test o la app pasan la lista con
///   `with_system_pids`) o la variable de entorno `ACTA_WIN_PIDS`
///   (formato `1234,5678`, la lee el propio adaptador).
/// - Si no hay origen, `start`/`retarget` fallan con `PortError::Unavailable`
///   explicando qué falta y cómo forzarlo para probar.
/// - Los bundle_ids se descartan con `tracing::warn`.
/// - Cuando el origen aporta los PID, se avisa con `tracing::warn` UNA vez
///   por arranque (WARN_PID_BRIDGE) de que se está usando el puente y de
///   que desaparecerá con M3.
static WARN_PID_BRIDGE: std::sync::Once = std::sync::Once::new();

/// Variable de entorno leída por el adaptador como origen de PID (`ACTA_WIN_PIDS`):
/// los PID van separados por comas (p. ej. `1234,5678`).
const PIDS_ENV: &str = "ACTA_WIN_PIDS";

/// Mensaje exacto con el que falla el arranque si la pista `system` está
/// activa y no hay PID ni origen del que obtenerlos.
const MISSING_PIDS_MESSAGE: &str = "la captura del audio del sistema (loopback por proceso) necesita al menos un PID del proceso que emite el sonido (por ejemplo la app de reunión); en Windows no hay adaptador de detección de reuniones (M3), que es quien aportaría ese PID. Para forzarlo en pruebas, define la variable de entorno ACTA_WIN_PIDS con los PID separados por comas (p. ej. 1234,5678) o pasa la lista al adaptador con WinAudioSource::with_system_pids.";

/// PID escritos en `ACTA_WIN_PIDS` (separados por comas; se ignora lo blanco).
/// La lectura es segura (`env::set_var` es lo que es `unsafe` en edition 2024);
/// si la variable existe pero no aporta ningún PID válido, quien llama decae
/// en el error de PID ausente con su mensaje completo.
fn pids_from_env() -> Vec<i32> {
    std::env::var(PIDS_ENV)
        .unwrap_or_default()
        .split(',')
        .filter_map(|s| s.trim().parse::<i32>().ok())
        .collect()
}

/// Argumentos `system` resultantes para el sidecar a partir del objetivo del
/// supervisor: sin bundle_ids (no soportados en Windows) y con los PID
/// resueltos del objetivo, del puente (`override_pids`) o de `ACTA_WIN_PIDS`.
fn resolve_system(override_pids: &[i32], system: SystemTarget) -> PortResult<SystemArgs> {
    if !system.enabled {
        return Ok(SystemArgs { enabled: false, pids: vec![], bundle_ids: vec![] });
    }
    let mut pids = system.pids;
    if pids.is_empty() {
        pids = override_pids.to_vec();
        if pids.is_empty() {
            pids = pids_from_env();
        }
        if pids.is_empty() {
            return Err(PortError::Unavailable(MISSING_PIDS_MESSAGE.into()));
        }
        WARN_PID_BRIDGE.call_once(|| {
            tracing::warn!(
                "puente temporal de PID: la captura del audio del sistema usa los PID de ACTA_WIN_PIDS o de with_system_pids porque no hay adaptador de detección de reuniones (M3); el puente desaparecerá cuando M3 aporte los PID por sí mismo"
            );
        });
    }
    if !system.bundle_ids.is_empty() {
        tracing::warn!(
            "se descartan los bundle_ids {:?}: el loopback de Windows solo admite dirección por PID (lo trae la detección de reuniones, M3)",
            system.bundle_ids
        );
    }
    Ok(SystemArgs { enabled: true, pids, bundle_ids: vec![] })
}

#[derive(Debug, Default)]
struct State {
    args: Option<CaptureStartArgs>,
    paused: bool,
    /// Pide a la tarea que drene lo ya publicado y termine.
    drain: Option<oneshot::Sender<()>>,
}

pub struct WinAudioSource {
    client: SidecarClient,
    state: Arc<Mutex<State>>,
    task: Slot,
    /// Lista de PID explícita para `system` mientras no exista M3.
    system_pids_override: Vec<i32>,
}

impl WinAudioSource {
    pub fn new(client: SidecarClient) -> Self {
        Self::with_system_pids(client, Vec::new())
    }

    /// Constructor con el puente temporal de PID (ver la documentación del
    /// módulo): `pids` sustituye a la detección de reuniones (M3) cuando el
    /// `capture.start` llegue con la pista `system` activa y sin PID.
    /// Una lista vacía desactiva el puente.
    pub fn with_system_pids(client: SidecarClient, pids: Vec<i32>) -> Self {
        Self {
            client,
            state: Arc::default(),
            task: Slot::default(),
            system_pids_override: pids,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        lock(&self.state)
    }
}

fn lock(s: &Mutex<State>) -> std::sync::MutexGuard<'_, State> {
    s.lock().unwrap_or_else(|e| e.into_inner())
}

fn track(t: TrackName) -> Track {
    match t {
        TrackName::Mic => Track::Mic,
        TrackName::System => Track::System,
    }
}

fn track_state(s: TrackStateName) -> TrackState {
    match s {
        TrackStateName::Running => TrackState::Running,
        TrackStateName::Stalled => TrackState::Stalled,
        TrackStateName::Restarting => TrackState::Restarting,
        TrackStateName::Stopped => TrackState::Stopped,
        TrackStateName::Failed => TrackState::Failed,
    }
}

/// Decodifica un frame PCM s16le base64.
pub fn decode_frame(f: &AudioFrameEvent) -> PortResult<AudioFrame> {
    if f.format != "s16le" || f.channels != 1 {
        return Err(PortError::InvalidResponse(format!(
            "unsupported frame format {} x{}",
            f.format, f.channels
        )));
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&f.data)
        .map_err(|e| PortError::InvalidResponse(format!("frame base64: {e}")))?;
    if bytes.len() % 2 != 0 {
        return Err(PortError::InvalidResponse("odd PCM byte count".into()));
    }
    let samples = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes(*b))
        .collect();
    Ok(AudioFrame {
        track: track(f.track),
        seq: f.seq,
        pts_us: f.pts_us,
        sample_rate: f.sample_rate,
        samples,
    })
}

fn map_event(event: acta_mac_bridge::protocol::Event) -> Option<AudioEvent> {
    use acta_mac_bridge::protocol::Event;
    Some(match event {
        Event::AudioFrame(f) => match decode_frame(&f) {
            Ok(frame) => AudioEvent::Frame(frame),
            Err(e) => {
                tracing::warn!("dropping bad audio frame: {e}");
                return None;
            }
        },
        Event::AudioDeviceChanged(d) => AudioEvent::DeviceChanged {
            track: track(d.track),
            device_name: d.device_name,
            reason: d.reason,
        },
        Event::AudioTrackState(s) => AudioEvent::TrackState {
            track: track(s.track),
            state: track_state(s.state),
            message: s.message,
        },
        Event::AudioOverrun(o) => AudioEvent::Overrun {
            track: track(o.track),
            dropped_frames: o.dropped_frames,
        },
        _ => return None,
    })
}

async fn fail_both(tx: &mpsc::Sender<AudioEvent>, message: &str) {
    for track in Track::ALL {
        let _ = tx
            .send(AudioEvent::TrackState {
                track,
                state: TrackState::Failed,
                message: Some(message.to_owned()),
            })
            .await;
    }
}

/// Tras un reinicio del sidecar: repite el arranque y la pausa si procede.
async fn recapture(client: &SidecarClient, state: &Mutex<State>) -> PortResult<Option<UnixMs>> {
    let (args, paused) = {
        let s = lock(state);
        (s.args.clone(), s.paused)
    };
    let Some(args) = args else { return Ok(None) };
    let res: CaptureStartResult = client
        .call_typed(Command::CaptureStart(args), Some(START_TIMEOUT))
        .await?;
    if paused {
        client.call(Command::CapturePause, None).await?;
    }
    Ok(Some(UnixMs(res.started_at_unix_ms)))
}

#[async_trait]
impl AudioSource for WinAudioSource {
    async fn start(&self, request: CaptureRequest) -> PortResult<(CaptureStarted, EventStream<AudioEvent>)> {
        self.task.cancel();
        let args = CaptureStartArgs {
            session_id: request.meeting_id.to_string(),
            sample_rate: request.format.sample_rate,
            frame_ms: request.frame_ms,
            mic: MicArgs {
                enabled: request.mic.enabled,
                device_uid: request.mic.device_uid,
            },
            system: resolve_system(&self.system_pids_override, request.system)?,
        };
        // Suscribirse ANTES de arrancar para no perder los primeros frames.
        let mut rx = self.client.subscribe();
        let res: CaptureStartResult = self
            .client
            .call_typed(Command::CaptureStart(args.clone()), Some(START_TIMEOUT))
            .await?;
        let (drain_tx, mut drain_rx) = oneshot::channel();
        *self.lock() = State {
            args: Some(args),
            paused: false,
            drain: Some(drain_tx),
        };
        let client = self.client.clone();
        let state = self.state.clone();
        let (stream, handle) = spawn_stream(move |tx| async move {
            loop {
                let msg = tokio::select! {
                    biased;
                    _ = &mut drain_rx => {
                        // Todo lo publicado antes de la respuesta a capture.stop
                        // ya está en el broadcast: se entrega y se termina.
                        while let Ok(SidecarMessage::Event(env)) = rx.try_recv() {
                            if let Some(ev) = map_event(env.event)
                                && tx.send(ev).await.is_err() {
                                    return;
                                }
                        }
                        return;
                    }
                    msg = rx.recv() => msg,
                };
                let ev = match msg {
                    Ok(SidecarMessage::Event(env)) => match map_event(env.event) {
                        Some(ev) => ev,
                        None => continue,
                    },
                    Ok(SidecarMessage::Restarted { .. }) => match recapture(&client, &state).await {
                        Ok(Some(restarted_at)) => AudioEvent::Restarted { restarted_at },
                        Ok(None) => return,
                        Err(e) => {
                            fail_both(&tx, &format!("capture restart failed: {e}")).await;
                            return;
                        }
                    },
                    Ok(SidecarMessage::GaveUp { message }) => {
                        fail_both(&tx, &message).await;
                        return;
                    }
                    Err(RecvError::Lagged(n)) => {
                        tracing::warn!("audio consumer lagged {n} sidecar messages");
                        for track in Track::ALL {
                            let _ = tx.send(AudioEvent::Overrun { track, dropped_frames: n }).await;
                        }
                        continue;
                    }
                    Err(RecvError::Closed) => return,
                };
                if tx.send(ev).await.is_err() {
                    return;
                }
            }
        });
        self.task.set(handle);
        Ok((
            CaptureStarted {
                started_at: UnixMs(res.started_at_unix_ms),
                mic_device: res.mic_device,
                system_pids: res.system_pids,
            },
            stream,
        ))
    }

    async fn pause(&self) -> PortResult<()> {
        self.client.call(Command::CapturePause, None).await?;
        self.lock().paused = true;
        Ok(())
    }

    async fn resume(&self) -> PortResult<()> {
        self.client.call(Command::CaptureResume, None).await?;
        self.lock().paused = false;
        Ok(())
    }

    async fn stop(&self) -> PortResult<()> {
        self.lock().args = None;
        let res = self.client.call(Command::CaptureStop, None).await;
        let drain = self.lock().drain.take();
        if let Some(drain) = drain {
            let _ = drain.send(());
        }
        if let Some(mut handle) = self.task.take()
            && tokio::time::timeout(Duration::from_secs(2), &mut handle).await.is_err() {
                handle.abort();
            }
        match res {
            // Sidecar caído: no hay nada que parar.
            Ok(_) | Err(PortError::Unavailable(_)) => Ok(()),
            Err(e) => Err(e),
        }
    }

    async fn retarget(&self, system: SystemTarget) -> PortResult<()> {
        // El objetivo reemplaza al completo: un `retarget` a sin PID también
        // pasa por el puente o falla explicando. Los PIDs van RESUELTOS
        // (objetivo → configuración → ACTA_WIN_PIDS), igual que en `start`: si
        // se mandaran los crudos, el override de configuración o de entorno
        // no se honraría aquí.
        let resolved = resolve_system(&self.system_pids_override, system)?;
        // La llamada al sidecar VA PRIMERO: el sidecar de Windows aún no
        // implementa `capture.retarget` (responde `unsupported`, lo fijan los
        // tests de `session.rs`), así que hoy el `retarget` falla SIEMPRE. Si
        // se mutara antes, un retarget fallido dejaría en el estado guardado
        // los PIDs nuevos y una recaptura posterior (tras un reinicio del
        // sidecar) repetiría `capture.start` con PIDs que nunca llegaron a
        // capturarse. Por eso los args guardados solo cambian tras el éxito y
        // un fallo deja el estado exactamente como estaba.
        self.client
            .call(
                Command::CaptureRetarget(CaptureRetargetArgs {
                    pids: resolved.pids.clone(),
                    bundle_ids: resolved.bundle_ids.clone(),
                }),
                None,
            )
            .await?;
        if let Some(args) = self.lock().args.as_mut() {
            args.system = resolved;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acta_mac_bridge::client::SidecarConfig;
    use acta_mac_bridge::protocol::{AudioOverrunEvent, AudioTrackStateEvent, TrackName};
    use acta_core::ids::MeetingId;
    use acta_core::recording::{AudioFormat, MicTarget, TrackState};
    use futures::StreamExt;

    fn frame(data: &str) -> AudioFrameEvent {
        AudioFrameEvent {
            track: TrackName::Mic,
            seq: 3,
            pts_us: 300_000,
            sample_rate: 16_000,
            channels: 1,
            format: "s16le".into(),
            data: data.into(),
        }
    }

    // ------------------------------------------------------------------
    // Retarget contra un sidecar falso de verdad (fprintf...: un proceso que
    // habla el protocolo y graba lo que recibe, como el falso /bin/sh de los
    // tests de acta-mac-bridge pero en Windows: PowerShell). Necesita un
    // proceso de verdad porque `retarget` toca `SidecarClient`.
    // ------------------------------------------------------------------

    fn fake_sidecar(dir: &std::path::Path) -> SidecarConfig {
        let script = dir.join("fake-acta-win.ps1");
        // SOLO ASCII: PowerShell 5.1 lee un .ps1 sin BOM como ANSI.
        std::fs::write(
            &script,
            r#"$ErrorActionPreference = 'Stop'
$dir = '__DIR__'
$requests = Join-Path $dir 'requests'
while ($true) {
  $line = [Console]::In.ReadLine()
  if ($null -eq $line) { break }
  Add-Content -Path $requests -Value $line
  $id = [regex]::Match($line, '"id":(\d+)').Groups[1].Value
  $cmd = [regex]::Match($line, '"cmd":"([^"]+)"').Groups[1].Value
  $reply = '{"type":"response","id":' + $id + ','
  if ($cmd -eq 'hello') {
    $reply += '"ok":true,"result":{"sidecar_version":"fake-win","protocol_version":1,"macos_version":"10.0.26300","capabilities":["mic","process_loopback"]}}'
  } elseif ($cmd -eq 'capture.start') {
    $reply += '"ok":true,"result":{"started_at_unix_ms":1700000000000,"mic_device":"fake","system_pids":[1]}}'
  } elseif ($cmd -eq 'capture.retarget') {
    # Como el sidecar real: capture.retarget aun no esta soportado y responde
    # "unsupported" SIN morir (lo fijan los tests de session.rs).
    $reply += '"ok":false,"error":{"code":"unsupported","message":"fake: sin capture.retarget"}}'
  } elseif ($cmd -eq 'shutdown') {
    [Console]::Out.WriteLine($reply + '"ok":true,"result":{}}')
    break
  } else {
    $reply += '"ok":true,"result":{}}'
  }
  [Console]::Out.WriteLine($reply)
}
exit 0
"#.replace("__DIR__", &dir.display().to_string()),
        ).expect("escribir el sidecar falso");
        SidecarConfig {
            binary: "powershell".into(),
            args: vec![
                "-NoProfile".into(),
                "-ExecutionPolicy".into(),
                "Bypass".into(),
                "-File".into(),
                script.display().to_string(),
            ],
            restart_backoff: Duration::from_millis(100),
            ..SidecarConfig::default()
        }
    }

    /// Lo que ha recibido el sidecar falso (la línea JSON cruda de cada
    /// petición; sin serde_json en este crate se extraen los campos a mano).
    fn requests(dir: &std::path::Path) -> Vec<String> {
        std::fs::read_to_string(dir.join("requests"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// Los PID del primer array `"pids":[...]` de la línea. En `capture.start`
    /// solo el objetivo `system` lleva `pids` y en `capture.retarget` es el
    /// primer campo, así que el primero es el que se busca.
    fn pids_after(line: &str) -> Vec<i32> {
        let Some(start) = line.find("\"pids\":[") else {
            return vec![];
        };
        let rest = &line[start + "\"pids\":[".len()..];
        let end = rest.find(']').expect("array cerrado");
        rest[..end]
            .split(',')
            .filter_map(|s| s.trim().parse::<i32>().ok())
            .collect()
    }

    async fn next_stream_event(stream: &mut EventStream<AudioEvent>) -> AudioEvent {
        tokio::time::timeout(Duration::from_secs(10), stream.next())
            .await
            .expect("evento en plazo")
            .expect("stream abierto")
    }

    /// Espera hasta que se cumpla (max. 5 s) sin bloquear el runtime.
    async fn eventually(ms: u64, mut f: impl FnMut() -> bool) -> bool {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
        while tokio::time::Instant::now() < deadline {
            if f() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        f()
    }

    /// `retarget` fallido (el sidecar responde unsupported, como el real) no
    /// deja los PIDs nuevos en el estado guardado: si se mutara antes de la
    /// llamada, una recaptura posterior tras un reinicio repetiría
    /// capture.start con PIDs que nunca llegaron a capturarse. Y los PIDs que
    /// se mandan al sidecar son los RESUELTOS (objetivo → puente), igual que
    /// en `start`. La muerte del sidecar provoca la recaptura que delata el
    /// estado guardado.
    #[tokio::test]
    async fn failed_retarget_keeps_the_stored_args_and_resolves_pids_like_start() {
        let dir = std::env::temp_dir().join(format!(
            "acta-win-bridge-retarget-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // El borrado se hace en Drop para que un fallo del test no deje basura
        // en el directorio temporal del sistema.
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(dir.clone());
        let client = SidecarClient::start(fake_sidecar(&dir)).await.unwrap();
        // Puente de PID con [7, 8]: un retarget a sin PID en el objetivo los
        // toma del puente, como `start`.
        let source = WinAudioSource::with_system_pids(client.clone(), vec![7, 8]);
        let (_, mut stream) = source
            .start(CaptureRequest {
                meeting_id: MeetingId::from_u128(7),
                format: AudioFormat::WORKING,
                frame_ms: 100,
                mic: MicTarget { enabled: true, device_uid: None },
                system: SystemTarget {
                    enabled: true,
                    pids: vec![1],
                    bundle_ids: vec!["com.microsoft.teams2".into()],
                },
            })
            .await
            .unwrap();

        let error = source
            .retarget(SystemTarget { enabled: true, pids: vec![], bundle_ids: vec![] })
            .await
            .unwrap_err();
        match error {
            PortError::Unavailable(message) => assert!(
                message.contains("unsupported"),
                "el fallo viene del sidecar (unsupported): {message}"
            ),
            other => panic!("se esperaba Unavailable, es {other:?}"),
        }
        let retarget_request = requests(&dir)
            .into_iter()
            .find(|r| r.contains("\"cmd\":\"capture.retarget\""))
            .expect("el sidecar recibió el capture.retarget");
        // PIDs resueltos (puente) en la llamada, no los crudos del objetivo.
        assert_eq!(pids_after(&retarget_request), vec![7, 8]);

        // Muerte brusca del falso: el cliente lo relanza y `WinAudioSource`
        // repite `capture.start` con el estado guardado. Tras un retarget
        // fallido ese estado sigue siendo el ORIGINAL (PIDs [1]).
        let sidecar_pid = client.pid().expect("sidecar vivo antes de matarlo");
        let out = tokio::process::Command::new("taskkill.exe")
            .args(["/F", "/PID", &sidecar_pid.to_string()])
            .kill_on_drop(true)
            .output()
            .await
            .expect("taskkill ejecutado");
        assert!(out.status.success(), "taskkill falló");
        let restarted = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if let AudioEvent::Restarted { .. } = next_stream_event(&mut stream).await {
                    return true;
                }
            }
        })
        .await
        .expect("sin Restarted tras la muerte del sidecar");
        assert!(restarted);
        assert!(
            eventually(5_000, || requests(&dir)
                .iter()
                .filter(|r| r.contains("\"cmd\":\"capture.start\""))
                .count()
                >= 2)
                .await,
            "la recaptura no repitió capture.start"
        );
        let recaptured = requests(&dir)
            .into_iter()
            .filter(|r| r.contains("\"cmd\":\"capture.start\""))
            .nth(1)
            .unwrap();
        assert_eq!(pids_after(&recaptured), vec![1],
            "un retarget fallido NO debía cambiar los PIDs guardados");
        assert!(recaptured.contains("\"bundle_ids\":[]"),
            "los bundle_ids no vuelven al sidecar: {recaptured}");

        source.stop().await.unwrap();
        client.shutdown().await.unwrap();
    }

    #[test]
    fn decodes_little_endian_samples() {
        let f = decode_frame(&frame("AQD+/wCAAAA=")).unwrap(); // 1, -2, i16::MIN, 0
        assert_eq!(f.samples, vec![1, -2, i16::MIN, 0]);
        assert_eq!((f.track, f.seq, f.pts_us), (Track::Mic, 3, 300_000));
    }

    #[test]
    fn rejects_bad_payloads() {
        assert!(decode_frame(&frame("@@@")).is_err());
        assert!(decode_frame(&frame("AQ==")).is_err(), "odd byte count");
        let mut f = frame("AQD+/w==");
        f.format = "f32le".into();
        assert!(decode_frame(&f).is_err());
    }

    #[test]
    fn maps_frame_track_state_and_overrun() {
        // frame
        let ev = map_event(acta_mac_bridge::protocol::Event::AudioFrame(frame("AQD+/w=="))).unwrap();
        match ev {
            AudioEvent::Frame(f) => assert_eq!(f.samples, vec![1, -2]),
            other => panic!("unexpected {other:?}"),
        }
        // track_state
        let ev = map_event(acta_mac_bridge::protocol::Event::AudioTrackState(AudioTrackStateEvent {
            track: TrackName::System,
            state: TrackStateName::Stalled,
            message: Some("no data".into()),
        }))
        .unwrap();
        match ev {
            AudioEvent::TrackState { track, state, message } => {
                assert_eq!((track, state), (Track::System, TrackState::Stalled));
                assert_eq!(message.as_deref(), Some("no data"));
            }
            other => panic!("unexpected {other:?}"),
        }
        // overrun
        let ev = map_event(acta_mac_bridge::protocol::Event::AudioOverrun(AudioOverrunEvent {
            track: TrackName::Mic,
            dropped_frames: 3,
        }))
        .unwrap();
        match ev {
            AudioEvent::Overrun { track, dropped_frames } => {
                assert_eq!(track, Track::Mic);
                assert_eq!(dropped_frames, 3);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn drops_bad_frames_and_foreign_events() {
        // Frame corrupto: se descarta sin romper el stream.
        assert_eq!(map_event(acta_mac_bridge::protocol::Event::AudioFrame(frame("@@@"))), None);
        // Evento que no es de audio (p. ej. `fatal` del ciclo de vida del sidecar).
        assert_eq!(
            map_event(acta_mac_bridge::protocol::Event::Fatal(acta_mac_bridge::protocol::FatalEvent {
                message: "boom".into(),
            })),
            None
        );
    }

    fn target(enabled: bool, pids: &[i32], bundle_ids: &[&str]) -> SystemTarget {
        SystemTarget {
            enabled,
            pids: pids.to_vec(),
            bundle_ids: bundle_ids.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    #[test]
    fn system_disabled_maps_to_sidecar_disabled() {
        let args = resolve_system(&[7], target(false, &[1], &["x"])).unwrap();
        assert_eq!(args, SystemArgs { enabled: false, pids: vec![], bundle_ids: vec![] });
    }

    #[test]
    fn pids_are_kept_and_bundle_ids_discarded() {
        let args = resolve_system(&[], target(true, &[10, 11], &["com.microsoft.teams2"])).unwrap();
        assert_eq!(args, SystemArgs { enabled: true, pids: vec![10, 11], bundle_ids: vec![] });
    }

    #[test]
    fn bridge_override_fills_missing_pids() {
        let args = resolve_system(&[21, 22], target(true, &[], &[])).unwrap();
        assert_eq!(args, SystemArgs { enabled: true, pids: vec![21, 22], bundle_ids: vec![] });
        // Los PID del objetivo mandan sobre el puente.
        let args = resolve_system(&[21], target(true, &[9], &[])).unwrap();
        assert_eq!(args.pids, vec![9]);
    }

    #[test]
    fn missing_every_source_fails_explaining() {
        let err = resolve_system(&[], target(true, &[], &[])).unwrap_err();
        match err {
            PortError::Unavailable(msg) => {
                assert!(msg.contains("PID"), "falta el qué: {msg}");
                assert!(msg.contains("M3"), "falta cuándo llega (M3): {msg}");
                assert!(msg.contains("ACTA_WIN_PIDS"), "falta cómo forzarlo: {msg}");
            }
            other => panic!("se esperaba Unavailable, es {other:?}"),
        }
    }
}
