//! Estado de la sesión del sidecar: qué está capturando y qué responde a
//! cada comando. Los hilos de captura llegan por el trait `TrackLauncher`
//! (impl real en `capture.rs`, fakes en los tests), como en el patrón de
//! acta-mac-bridge. El orden de salida (eventos antes que respuestas) y los
//! mensajes de error siguen al Dispatcher de macOS.

use std::sync::mpsc::SyncSender;

use crate::protocol::{
    self, CaptureStartArgs, CaptureStartResult, ErrorCode, Outgoing, ParsedRequest,
    PROTOCOL_VERSION,
};

/// Capacidad de la cola hacia el escritor de stdout: ~12 s de frames de
/// 100 ms. Si el escritor se atrasa, se descarta lo más viejo y se emite
/// `audio.overrun` (nunca se bloquea el hilo de captura).
pub const CHANNEL_CAPACITY: usize = 128;

/// Qué hacer tras procesar una línea.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    /// Salir con el código dado: `shutdown` y EOF en stdin salen con 0; una
    /// versión de protocolo incompatible, con error claro.
    Exit(i32),
}

impl Outcome {
    /// Código de salida; `Continue` no termina el proceso.
    pub fn code(self) -> Option<i32> {
        match self {
            Self::Continue => None,
            Self::Exit(code) => Some(code),
        }
    }
}

/// Control de las pistas lanzadas: pausar y parar (uniéndose a los hilos).
pub trait TrackControls: Send {
    fn set_paused(&self, paused: bool);
    /// Para todas las pistas y se queda esperando a que terminen.
    fn stop_and_join(self: Box<Self>);
}

/// Resultado de lanzar las pistas (impl real: `capture::spawn_tracks`).
pub struct SpawnedTracks {
    pub mic_device: Option<String>,
    pub system_pids: Vec<i32>,
    pub controls: Box<dyn TrackControls>,
}

/// Error de lanzamiento, ya traducido a un código del contrato.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchError {
    pub code: ErrorCode,
    pub message: String,
}

pub trait TrackLauncher: Send {
    fn launch(
        &mut self,
        args: &CaptureStartArgs,
        out: &SyncSender<Outgoing>,
    ) -> Result<SpawnedTracks, LaunchError>;
}

/// Sesión: comandos ↔ captura.
pub struct Session<L: TrackLauncher> {
    out: SyncSender<Outgoing>,
    launcher: L,
    active: Option<ActiveCapture>,
    shutting_down: bool,
}

struct ActiveCapture {
    controls: Box<dyn TrackControls>,
}

/// El `id` de una petición ya parseada, para emitir `cancelled` al cerrarse.
fn request_id(parsed: &ParsedRequest) -> Option<u64> {
    match parsed {
        ParsedRequest::Hello { id, .. }
        | ParsedRequest::Shutdown { id }
        | ParsedRequest::CaptureStart { id, .. }
        | ParsedRequest::CapturePause { id }
        | ParsedRequest::CaptureResume { id }
        | ParsedRequest::CaptureStop { id }
        | ParsedRequest::NotImplementedYet { id, .. }
        | ParsedRequest::Unknown { id, .. }
        | ParsedRequest::BadLine { id: Some(id) } => Some(*id),
        ParsedRequest::BadLine { id: None } => None,
    }
}

impl<L: TrackLauncher> Session<L> {
    pub fn new(out: SyncSender<Outgoing>, launcher: L) -> Self {
        Self { out, launcher, active: None, shutting_down: false }
    }

    /// Procesa una línea de stdin (el tope de tamaño lo comprueba el lector).
    /// Espejo del `Dispatcher.handle` de macOS: primero se reportan las líneas
    /// corruptas y los comandos desconocidos, el cierre emite `cancelled` para
    /// todo menos `shutdown`, y el resto enruta por comando.
    pub fn handle(&mut self, line: &str) -> Outcome {
        let parsed = protocol::parse_request(line);
        match parsed {
            ParsedRequest::Unknown { id, cmd } => {
                self.fail(id, ErrorCode::Unsupported, format!("unknown command {cmd}"));
                return Outcome::Continue;
            }
            ParsedRequest::NotImplementedYet { id, cmd } => {
                self.fail(
                    id,
                    ErrorCode::Unsupported,
                    format!("{cmd} is not implemented in this sidecar"),
                );
                return Outcome::Continue;
            }
            ParsedRequest::BadLine { id } => {
                if let Some(id) = id {
                    self.fail(id, ErrorCode::InvalidArgs, "bad request");
                } else {
                    eprintln!("acta-win: línea de petición no válida (sin id)");
                }
                return Outcome::Continue;
            }
            _ => {}
        }
        if self.shutting_down && !matches!(parsed, ParsedRequest::Shutdown { .. }) {
            if let Some(id) = request_id(&parsed) {
                self.fail(id, ErrorCode::Cancelled, "shutting down");
            }
            return Outcome::Continue;
        }
        match parsed {
            ParsedRequest::Hello { id, protocol_version }
                if protocol_version == PROTOCOL_VERSION =>
            {
                self.ok(
                    id,
                    serde_json::to_value(protocol::HelloResult {
                        sidecar_version: env!("CARGO_PKG_VERSION").into(),
                        protocol_version: PROTOCOL_VERSION,
                        macos_version: crate::capture::windows_version(),
                        capabilities: vec!["mic".into(), "process_loopback".into()],
                    })
                    .unwrap_or(serde_json::Value::Null),
                );
                Outcome::Continue
            }
            ParsedRequest::Hello { id: _, protocol_version } => {
                // Decisión propia del port (mac solo avisa y sigue): una
                // versión incompatible no tiene respuesta que el cliente pueda
                // aceptar, así que se sale con error claro en stderr.
                eprintln!(
                    "acta-win: versión de protocolo incompatible: host {} sidecar {PROTOCOL_VERSION}",
                    protocol_version
                );
                Outcome::Exit(1)
            }
            ParsedRequest::Shutdown { id } => {
                self.shutting_down = true;
                self.do_stop();
                self.ok_empty(id);
                Outcome::Exit(0)
            }
            ParsedRequest::CaptureStart { id, args } => self.handle_start(id, args),
            ParsedRequest::CapturePause { id } => {
                if let Some(active) = &self.active {
                    active.controls.set_paused(true);
                }
                // mac: pausar sin captura es inocuo (encadenamiento opcional).
                self.ok_empty(id);
                Outcome::Continue
            }
            ParsedRequest::CaptureResume { id } => {
                if let Some(active) = &self.active {
                    active.controls.set_paused(false);
                }
                self.ok_empty(id);
                Outcome::Continue
            }
            ParsedRequest::CaptureStop { id } => {
                self.do_stop();
                self.ok_empty(id);
                Outcome::Continue
            }
            _ => unreachable!("los demás los cubre el match de arriba"),
        }
    }

    /// Para la captura si la hay: `capture.stop` y `shutdown` comparten el
    /// mismo cierre (join de pistas, que emite los `stopped` antes de la
    /// respuesta, como hace `CaptureService.stop` en macOS).
    fn do_stop(&mut self) {
        if let Some(active) = self.active.take() {
            active.controls.stop_and_join();
        }
    }

    fn handle_start(&mut self, id: u64, args: CaptureStartArgs) -> Outcome {
        if self.active.is_some() {
            self.fail(id, ErrorCode::Busy, "capture already running");
            return Outcome::Continue;
        }
        if let Err(error) = Self::validate_start(&args) {
            self.fail(id, error.code, error.message);
            return Outcome::Continue;
        }
        match self.launcher.launch(&args, &self.out) {
            Ok(spawned) => {
                // `pts = 0` corresponde (con la tolerancia del arranque, unos
                // ms de activación) al momento de esta respuesta.
                self.active = Some(ActiveCapture { controls: spawned.controls });
                self.ok(
                    id,
                    serde_json::to_value(CaptureStartResult {
                        started_at_unix_ms: protocol::now_unix_ms(),
                        mic_device: spawned.mic_device,
                        system_pids: spawned.system_pids,
                    })
                    .unwrap_or(serde_json::Value::Null),
                );
            }
            Err(error) => self.fail(id, error.code, error.message),
        }
        Outcome::Continue
    }

    /// EOF en stdin o salida: para la captura si la hay (sin respuesta).
    pub fn on_exit(&mut self) {
        if let Some(active) = self.active.take() {
            active.controls.stop_and_join();
        }
    }

    fn send(&self, outgoing: Outgoing) {
        let _ = self.out.send(outgoing);
    }

    fn fail(&self, id: u64, code: ErrorCode, message: impl Into<String>) {
        self.send(protocol::error_response(id, code, message));
    }

    fn ok(&self, id: u64, result: serde_json::Value) {
        self.send(protocol::ok_response(id, result));
    }

    fn ok_empty(&self, id: u64) {
        self.ok(id, protocol::empty_result());
    }

    /// Valida `capture.start` igual que el sidecar de macOS (orden y
    /// mensajes), más lo que Windows no sabe hacer todavía.
    fn validate_start(args: &CaptureStartArgs) -> Result<(), LaunchError> {
        // Espejo de mac: busy lo comprueba el llamante; aquí sample_rate,
        // frame_ms y «no track enabled», en ese orden.
        if args.sample_rate != 16_000 {
            return Err(LaunchError {
                code: ErrorCode::InvalidArgs,
                message: "sample_rate must be 16000".into(),
            });
        }
        if args.frame_ms == 0 || args.frame_ms > 1000 {
            return Err(LaunchError { code: ErrorCode::InvalidArgs, message: "bad frame_ms".into() });
        }
        if !args.mic.enabled && !args.system.enabled {
            return Err(LaunchError {
                code: ErrorCode::InvalidArgs,
                message: "no track enabled".into(),
            });
        }
        // Windows: solo loopback por PID. Los bundle_ids y el UID del
        // dispositivo de entrada quedan fuera de lo implementado (queda
        // dicho, nunca en silencio).
        if args.system.enabled && !args.system.bundle_ids.is_empty() {
            return Err(LaunchError {
                code: ErrorCode::Unsupported,
                message: "bundle_ids targeting is not supported on Windows; use pids".into(),
            });
        }
        if args.mic.enabled && args.mic.device_uid.is_some() {
            return Err(LaunchError {
                code: ErrorCode::Unsupported,
                message: "mic device_uid selection is not supported on Windows yet".into(),
            });
        }
        if args.system.enabled {
            if args.system.pids.is_empty() {
                return Err(LaunchError {
                    code: ErrorCode::InvalidArgs,
                    message: "system capture needs at least one pid".into(),
                });
            }
            if args.system.pids.iter().any(|&pid| pid <= 0) {
                return Err(LaunchError {
                    code: ErrorCode::InvalidArgs,
                    message: "pids must be positive".into(),
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::protocol::{AudioTrackStateEvent, EventOut, TrackName, TrackStateName};
    use std::sync::{Arc, Mutex};
    use std::sync::mpsc::{sync_channel, Receiver};

    /// Estado compartido del launcher falso, para que los tests vean qué pasó.
    #[derive(Default)]
    struct FakeState {
        paused: bool,
        joined: bool,
    }

    struct FakeControls {
        state: Arc<Mutex<FakeState>>,
        out: SyncSender<Outgoing>,
    }

    impl TrackControls for FakeControls {
        fn set_paused(&self, paused: bool) {
            self.state.lock().unwrap().paused = paused;
        }

        fn stop_and_join(self: Box<Self>) {
            self.state.lock().unwrap().joined = true;
            // Comportamiento del impl real: state `stopped` por pista.
            for track in [TrackName::Mic, TrackName::System] {
                let _ = self.out.send(protocol::event(EventOut::AudioTrackState(
                    AudioTrackStateEvent { track, state: TrackStateName::Stopped, message: None },
                )));
            }
        }
    }

    struct FakeLauncher {
        started: Vec<CaptureStartArgs>,
        fail: bool,
        state: Arc<Mutex<FakeState>>,
    }

    impl TrackLauncher for FakeLauncher {
        fn launch(
            &mut self,
            args: &CaptureStartArgs,
            out: &SyncSender<Outgoing>,
        ) -> Result<SpawnedTracks, LaunchError> {
            if self.fail {
                return Err(LaunchError { code: ErrorCode::Internal, message: "boom".into() });
            }
            self.started.push(args.clone());
            Ok(SpawnedTracks {
                mic_device: Some("dev".into()),
                system_pids: vec![10, 11],
                controls: Box::new(FakeControls { state: self.state.clone(), out: out.clone() }),
            })
        }
    }

    fn start_args(pids: &[i32]) -> CaptureStartArgs {
        serde_json::from_value(serde_json::json!({
            "session_id": "s",
            "sample_rate": 16000,
            "frame_ms": 100,
            "mic": {"enabled": true},
            "system": {"enabled": true, "pids": pids, "bundle_ids": []}
        }))
        .unwrap()
    }

    struct Harness {
        session: Session<FakeLauncher>,
        rx: Receiver<Outgoing>,
        state: Arc<Mutex<FakeState>>,
    }

    impl Harness {
        fn new() -> Self {
            Self::with(|_: &mut FakeLauncher| {})
        }

        fn with(setup: impl FnOnce(&mut FakeLauncher)) -> Self {
            let (tx, rx) = sync_channel(CHANNEL_CAPACITY);
            let state = Arc::new(Mutex::new(FakeState::default()));
            let mut launcher = FakeLauncher { started: vec![], fail: false, state: state.clone() };
            setup(&mut launcher);
            let session = Session::new(tx, launcher);
            Harness { session, rx, state }
        }

        fn handle(&mut self, line: &str) -> Outcome {
            self.session.handle(line)
        }

        fn responses(&self) -> Vec<serde_json::Value> {
            collect(&self.rx)
                .into_iter()
                .filter_map(|outgoing| match outgoing {
                    Outgoing::Response(line) => Some(serde_json::to_value(line).unwrap()),
                    Outgoing::Event(_) => None,
                })
                .collect()
        }

        fn outgoing_json(&self) -> Vec<serde_json::Value> {
            collect(&self.rx).into_iter().map(serde_json::to_value).map(Result::unwrap).collect()
        }

        fn drain(&self) {
            while self.rx.try_recv().is_ok() {}
        }
    }

    /// Vacía el canal sin bloquear.
    fn collect(rx: &Receiver<Outgoing>) -> Vec<Outgoing> {
        let mut out = vec![];
        while let Ok(outgoing) = rx.try_recv() {
            out.push(outgoing);
        }
        out
    }

    #[test]
    fn hello_responds_with_our_protocol_version() {
        let mut harness = Harness::new();
        assert_eq!(harness.handle(r#"{"id":7,"cmd":"hello","args":{"protocol_version":1}}"#), Outcome::Continue);
        let responses = harness.responses();
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["type"], "response");
        assert_eq!(responses[0]["id"], 7);
        assert_eq!(responses[0]["ok"], true);
        assert_eq!(responses[0]["result"]["protocol_version"], PROTOCOL_VERSION);
        assert!(!responses[0]["result"]["sidecar_version"].as_str().unwrap().is_empty());
        assert!(!responses[0]["result"]["capabilities"].as_array().unwrap().is_empty());
        assert!(responses[0]["result"]["macos_version"].as_str().is_some());
    }

    #[test]
    fn hello_with_a_foreign_version_exits_with_a_clear_error() {
        let mut harness = Harness::new();
        assert_eq!(harness.handle(r#"{"id":7,"cmd":"hello","args":{"protocol_version":2}}"#), Outcome::Exit(1));
        assert!(harness.responses().is_empty(), "no hay respuesta que el cliente pueda aceptar");
    }

    #[test]
    fn unknown_command_answers_unsupported() {
        let mut harness = Harness::new();
        harness.handle(r#"{"id":9,"cmd":"nope"}"#);
        let responses = harness.responses();
        assert_eq!(responses[0]["error"]["code"], "unsupported");
        assert_eq!(responses[0]["id"], 9);
    }

    #[test]
    fn commands_not_implemented_yet_answer_unsupported() {
        let mut harness = Harness::new();
        harness.handle(r#"{"id":5,"cmd":"devices.list"}"#);
        harness.handle(r#"{"id":6,"cmd":"capture.retarget","args":{"pids":[1],"bundle_ids":[]}}"#);
        let responses = harness.responses();
        assert_eq!(responses[0]["error"]["code"], "unsupported");
        assert_eq!(responses[1]["error"]["code"], "unsupported");
    }

    #[test]
    fn bad_request_with_id_answers_invalid_args() {
        let mut harness = Harness::new();
        harness.handle(r#"{"id":3,"cmd":"capture.start","args":{"sample_rate":1}}"#);
        harness.handle("{not json}");
        let responses = harness.responses();
        assert_eq!(responses[0]["error"]["code"], "invalid_args");
        assert_eq!(responses[0]["id"], 3);
        assert_eq!(responses.len(), 1, "sin id no se responde");
    }

    #[test]
    fn capture_start_validates_like_the_mac_sidecar() {
        let mut harness = Harness::new();
        let cases = [
            (r#"{"session_id":"s","sample_rate":8000,"frame_ms":100,"mic":{"enabled":true},"system":{"enabled":true,"pids":[1],"bundle_ids":[]}}"#, "invalid_args"),
            (r#"{"session_id":"s","sample_rate":16000,"frame_ms":0,"mic":{"enabled":true},"system":{"enabled":true,"pids":[1],"bundle_ids":[]}}"#, "invalid_args"),
            (r#"{"session_id":"s","sample_rate":16000,"frame_ms":1001,"mic":{"enabled":true},"system":{"enabled":true,"pids":[1],"bundle_ids":[]}}"#, "invalid_args"),
            (r#"{"session_id":"s","sample_rate":16000,"frame_ms":100,"mic":{"enabled":false},"system":{"enabled":false,"pids":[],"bundle_ids":[]}}"#, "invalid_args"),
            (r#"{"session_id":"s","sample_rate":16000,"frame_ms":100,"mic":{"enabled":true},"system":{"enabled":true,"pids":[],"bundle_ids":["com.microsoft.teams2"]}}"#, "unsupported"),
            (r#"{"session_id":"s","sample_rate":16000,"frame_ms":100,"mic":{"enabled":true,"device_uid":"x"},"system":{"enabled":false,"pids":[],"bundle_ids":[]}}"#, "unsupported"),
            (r#"{"session_id":"s","sample_rate":16000,"frame_ms":100,"mic":{"enabled":false},"system":{"enabled":true,"pids":[],"bundle_ids":[]}}"#, "invalid_args"),
            (r#"{"session_id":"s","sample_rate":16000,"frame_ms":100,"mic":{"enabled":false},"system":{"enabled":true,"pids":[0],"bundle_ids":[]}}"#, "invalid_args"),
            (r#"{"session_id":"s","sample_rate":16000,"frame_ms":100,"mic":{"enabled":false},"system":{"enabled":true,"pids":[-3],"bundle_ids":[]}}"#, "invalid_args"),
        ];
        for (args, code) in cases {
            harness.drain();
            let line = format!(r#"{{"id":4,"cmd":"capture.start","args":{args}}}"#);
            harness.handle(&line);
            let responses = harness.responses();
            assert_eq!(responses[0]["error"]["code"], code, "args = {args}");
        }
        assert!(harness.session.launcher.started.is_empty());
    }

    #[test]
    fn capture_start_launches_and_answers_the_contract_result() {
        let mut harness = Harness::new();
        harness.handle(r#"{"id":4,"cmd":"capture.start","args":{"session_id":"s","sample_rate":16000,"frame_ms":100,"mic":{"enabled":true},"system":{"enabled":true,"pids":[10,11],"bundle_ids":[]}}}"#);
        assert_eq!(harness.session.launcher.started.len(), 1);
        let responses = harness.responses();
        assert_eq!(responses[0]["ok"], true);
        assert!(responses[0]["result"]["started_at_unix_ms"].as_i64().unwrap() > 0);
        assert_eq!(responses[0]["result"]["mic_device"], "dev");
        assert_eq!(responses[0]["result"]["system_pids"], serde_json::json!([10, 11]));
    }

    #[test]
    fn capture_start_twice_is_busy() {
        let mut harness = Harness::new();
        harness.handle(&format!(r#"{{"id":1,"cmd":"capture.start","args":{}}}"#, serde_json::to_string(&start_args(&[10])).unwrap()));
        assert_eq!(harness.responses()[0]["ok"], true);
        harness.handle(&format!(r#"{{"id":2,"cmd":"capture.start","args":{}}}"#, serde_json::to_string(&start_args(&[10])).unwrap()));
        let responses = harness.responses();
        assert_eq!(responses.len(), 1);
        assert_eq!(responses[0]["error"]["code"], "busy");
        assert_eq!(harness.session.launcher.started.len(), 1);
    }

    #[test]
    fn launch_failure_becomes_an_internal_error() {
        let mut harness = Harness::with(|launcher: &mut FakeLauncher| launcher.fail = true);
        harness.handle(&format!(r#"{{"id":1,"cmd":"capture.start","args":{}}}"#, serde_json::to_string(&start_args(&[10])).unwrap()));
        let responses = harness.responses();
        assert_eq!(responses[0]["error"]["code"], "internal");
    }

    #[test]
    fn pause_and_resume_reach_the_tracks() {
        let mut harness = Harness::new();
        harness.handle(&format!(r#"{{"id":1,"cmd":"capture.start","args":{}}}"#, serde_json::to_string(&start_args(&[10])).unwrap()));
        harness.drain();
        harness.handle(r#"{"id":2,"cmd":"capture.pause"}"#);
        assert!(harness.state.lock().unwrap().paused);
        harness.handle(r#"{"id":3,"cmd":"capture.resume"}"#);
        assert!(!harness.state.lock().unwrap().paused);
        let responses = harness.responses();
        assert_eq!(responses.len(), 2);
        for response in responses {
            assert_eq!(response["result"], serde_json::json!({}));
        }
    }

    #[test]
    fn stop_joins_the_tracks_then_answers_ok() {
        let mut harness = Harness::new();
        harness.handle(&format!(r#"{{"id":1,"cmd":"capture.start","args":{}}}"#, serde_json::to_string(&start_args(&[10])).unwrap()));
        harness.drain();
        harness.handle(r#"{"id":2,"cmd":"capture.stop"}"#);
        assert!(harness.state.lock().unwrap().joined);
        let all = harness.outgoing_json();
        // Los eventos `stopped` salen antes que la respuesta (mac: stop()
        // emite y el Dispatcher responde después).
        assert_eq!(all[0]["type"], "event");
        assert_eq!(all[0]["data"]["track"], "mic");
        assert_eq!(all[0]["data"]["state"], "stopped");
        assert_eq!(all[1]["data"]["track"], "system");
        assert_eq!(all.last().unwrap()["type"], "response");
        assert_eq!(all.last().unwrap()["result"], serde_json::json!({}));
        // Y se puede capturar otra vez.
        harness.drain();
        harness.handle(&format!(r#"{{"id":3,"cmd":"capture.start","args":{}}}"#, serde_json::to_string(&start_args(&[10])).unwrap()));
        assert_eq!(harness.responses()[0]["ok"], true);
    }

    #[test]
    fn pause_resume_and_stop_without_capture_are_harmless() {
        let mut harness = Harness::new();
        harness.handle(r#"{"id":1,"cmd":"capture.pause"}"#);
        harness.handle(r#"{"id":2,"cmd":"capture.resume"}"#);
        harness.handle(r#"{"id":3,"cmd":"capture.stop"}"#);
        for response in harness.responses() {
            assert_eq!(response["ok"], true);
        }
    }

    #[test]
    fn shutdown_stops_capture_answers_and_exits_zero() {
        let mut harness = Harness::new();
        harness.handle(&format!(r#"{{"id":1,"cmd":"capture.start","args":{}}}"#, serde_json::to_string(&start_args(&[10])).unwrap()));
        harness.drain();
        assert_eq!(harness.handle(r#"{"id":2,"cmd":"shutdown"}"#), Outcome::Exit(0));
        assert!(harness.state.lock().unwrap().joined);
        let all = harness.outgoing_json();
        assert_eq!(all.last().unwrap()["type"], "response");
        assert_eq!(all.last().unwrap()["result"], serde_json::json!({}));
        // Tras el shutdown todo lo demás queda cancelado (mac: shutting down).
        harness.drain();
        harness.handle(r#"{"id":3,"cmd":"capture.pause"}"#);
        assert_eq!(harness.responses()[0]["error"]["code"], "cancelled");
    }

    #[test]
    fn eof_stops_the_capture_without_answering() {
        let mut harness = Harness::new();
        harness.handle(&format!(r#"{{"id":1,"cmd":"capture.start","args":{}}}"#, serde_json::to_string(&start_args(&[10])).unwrap()));
        harness.drain();
        harness.session.on_exit();
        assert!(harness.state.lock().unwrap().joined);
        // Solo eventos; ninguna respuesta a mitad del cierre.
        let outgoing = harness.outgoing_json();
        assert!(!outgoing.is_empty());
        for line in outgoing {
            assert_eq!(line["type"], "event");
        }
    }
}
