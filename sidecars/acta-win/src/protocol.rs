//! Espejo local del protocolo sidecar ⇄ Rust v1: solo el subconjunto que este
//! sidecar implementa (`hello`, `shutdown`, `capture.*`). Las formas exactas
//! (nombres de campos, etiquetas, códigos de error) vienen de
//! `crates/acta-mac-bridge/src/protocol.rs` y `docs/01-arquitectura.md` §4:
//! nada se inventa aquí. Duplicar el módulo es transitorio: dónde vive el
//! módulo compartido lo decide T13.
//!
//! Transporte: JSON por líneas (UTF-8, `\n`), una línea ≤ `MAX_LINE_BYTES`.
//! - stdin  (Rust → sidecar): `{"id":7,"cmd":"capture.start","args":{...}}`
//! - stdout (sidecar → Rust): respuestas y eventos, una línea por mensaje.
//! - stderr: logs en texto libre (nunca protocolo).

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

// ===========================================================================
// Rust → sidecar
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloArgs {
    pub protocol_version: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MicArgs {
    pub enabled: bool,
    /// `null` = dispositivo por defecto. La selección de dispositivo no está
    /// implementada en Windows todavía: si llega un valor se responde
    /// `unsupported` (queda dicho, nunca en silencio).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_uid: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemArgs {
    pub enabled: bool,
    pub pids: Vec<i32>,
    pub bundle_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureStartArgs {
    pub session_id: String,
    /// Siempre 16000.
    pub sample_rate: u32,
    /// Siempre 100.
    pub frame_ms: u32,
    pub mic: MicArgs,
    pub system: SystemArgs,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct RawRequest {
    pub id: u64,
    pub cmd: String,
    pub args: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ParsedRequest {
    Hello { id: u64, protocol_version: u32 },
    Shutdown { id: u64 },
    CaptureStart { id: u64, args: CaptureStartArgs },
    CapturePause { id: u64 },
    CaptureResume { id: u64 },
    CaptureStop { id: u64 },
    /// Comando que existe en el protocolo v1 pero el sidecar de Windows no
    /// implementa todavía.
    NotImplementedYet { id: u64, cmd: String },
    /// Comando que no existe en el protocolo v1.
    Unknown { id: u64, cmd: String },
    /// Línea corrupta o con argumentos que no encajan: se responde
    /// `invalid_args` si el JSON tenía `id` (como hace el Dispatcher de macOS).
    BadLine { id: Option<u64> },
}

/// Comandos del protocolo v1 que este sidecar no implementa (y por qué no hay
/// que inventarse otro mensaje): la lista se mantiene a mano con el
/// `Command` del contrato.
const NOT_IMPLEMENTED: &[&str] = &[
    "permissions.status",
    "permissions.request",
    "devices.list",
    "apps.list",
    "detect.start",
    "detect.stop",
    "capture.retarget",
    "speakers.watch",
    "speakers.unwatch",
    "screen.start",
    "screen.snapshot",
    "screen.stop",
    "clipboard.start",
    "clipboard.stop",
    "asr.transcribe",
    "asr.live.start",
    "asr.live.stop",
    "diarize.run",
    "voiceprint.embed",
    "models.status",
    "models.download",
    "job.cancel",
    "meeting.titles",
    "calendar.events",
    "system.profile",
    "apps.installed",
];

/// Parsea una línea de stdin. Los campos desconocidos se toleran (igual que
/// hace `JSONDecoder` en Swift, que ignora las claves que no pide).
pub fn parse_request(line: &str) -> ParsedRequest {
    let Ok(raw) = serde_json::from_str::<RawRequest>(line) else {
        #[derive(Deserialize)]
        struct IdOnly {
            id: u64,
        }
        return ParsedRequest::BadLine {
            id: serde_json::from_str::<IdOnly>(line).ok().map(|r| r.id),
        };
    };
    let id = raw.id;
    let args = raw.args.unwrap_or(serde_json::Value::Null);
    match raw.cmd.as_str() {
        "hello" => match serde_json::from_value::<HelloArgs>(args) {
            Ok(hello) => ParsedRequest::Hello {
                id,
                protocol_version: hello.protocol_version,
            },
            Err(_) => ParsedRequest::BadLine { id: Some(id) },
        },
        "shutdown" => ParsedRequest::Shutdown { id },
        "capture.start" => match serde_json::from_value::<CaptureStartArgs>(args) {
            Ok(start) => ParsedRequest::CaptureStart { id, args: start },
            Err(_) => ParsedRequest::BadLine { id: Some(id) },
        },
        "capture.pause" => ParsedRequest::CapturePause { id },
        "capture.resume" => ParsedRequest::CaptureResume { id },
        "capture.stop" => ParsedRequest::CaptureStop { id },
        cmd if NOT_IMPLEMENTED.contains(&cmd) => ParsedRequest::NotImplementedYet {
            id,
            cmd: raw.cmd,
        },
        cmd => ParsedRequest::Unknown { id, cmd: cmd.to_owned() },
    }
}

// ===========================================================================
// sidecar → Rust
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackName {
    Mic,
    System,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    PermissionDenied,
    NotFound,
    Unsupported,
    InvalidArgs,
    Busy,
    Cancelled,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtocolError {
    pub code: ErrorCode,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloResult {
    pub sidecar_version: String,
    pub protocol_version: u32,
    /// El nombre del campo lo fija el contrato (`macos_version`, sin guiones
    /// bajos); el contenido aquí es la versión de Windows. Se decidió así
    /// porque renombrar el campo rompería al cliente del contrato.
    pub macos_version: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureStartResult {
    /// Unix ms correspondiente a `pts_us = 0`.
    pub started_at_unix_ms: i64,
    /// Id del endpoint de captura del micrófono. Falta si no se pudo obtener.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mic_device: Option<String>,
    pub system_pids: Vec<i32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackStateName {
    Running,
    Stalled,
    Restarting,
    Stopped,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioFrameEvent {
    pub track: TrackName,
    /// Secuencia por pista desde 0.
    pub seq: u64,
    /// µs del primer sample del frame en la línea temporal del audio de la
    /// pista (reloj común: ambos tracks arrancan con `capture.start`).
    pub pts_us: u64,
    pub sample_rate: u32,
    pub channels: u16,
    /// Siempre "s16le".
    pub format: String,
    /// PCM base64 (estándar, con padding).
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioTrackStateEvent {
    pub track: TrackName,
    pub state: TrackStateName,
    /// Como en el contrato: `message` ausente cuando es None.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioOverrunEvent {
    pub track: TrackName,
    pub dropped_frames: u64,
}

// Espejo exacto de `Event`/`EventLine` del contrato (las tres variantes del
// protocolo de audio empiezan con `audio.`, como en él).
#[allow(clippy::enum_variant_names)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", content = "data")]
pub enum EventOut {
    #[serde(rename = "audio.frame")]
    AudioFrame(AudioFrameEvent),
    #[serde(rename = "audio.track_state")]
    AudioTrackState(AudioTrackStateEvent),
    #[serde(rename = "audio.overrun")]
    AudioOverrun(AudioOverrunEvent),
}

/// Línea de evento: `{"type":"event","ts":...,"event":"...","data":{...}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventLine {
    #[serde(rename = "type")]
    pub line_type: String,
    /// Reloj de pared (unix ms) al emitir.
    pub ts: i64,
    #[serde(flatten)]
    pub event: EventOut,
}

/// Línea de respuesta: `{"type":"response","id":..,"ok":..,"result"?:..,"error"?:..}`.
/// Espejo de `ResponseLine<R>` (Protocol.swift): `result`/`error` se omiten.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResponseLine {
    #[serde(rename = "type")]
    pub line_type: String,
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProtocolError>,
}

/// Todo lo que sale por stdout: una variante por tipo de línea.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Outgoing {
    Response(ResponseLine),
    Event(EventLine),
}

impl Outgoing {
    pub fn to_line(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

/// Unix ms del reloj de pared (como `Clock.unixMs()` en el sidecar de macOS).
pub fn now_unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

pub fn ok_response(id: u64, result: serde_json::Value) -> Outgoing {
    Outgoing::Response(ResponseLine {
        line_type: "response".into(),
        id,
        ok: true,
        result: Some(result),
        error: None,
    })
}

pub fn empty_result() -> serde_json::Value {
    serde_json::json!({})
}

pub fn error_response(id: u64, code: ErrorCode, message: impl Into<String>) -> Outgoing {
    Outgoing::Response(ResponseLine {
        line_type: "response".into(),
        id,
        ok: false,
        result: None,
        error: Some(ProtocolError { code, message: message.into() }),
    })
}

pub fn event(event: EventOut) -> Outgoing {
    Outgoing::Event(EventLine { line_type: "event".into(), ts: now_unix_ms(), event })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn protocol_version_and_line_cap_match_the_contract() {
        assert_eq!(PROTOCOL_VERSION, 1);
        assert_eq!(MAX_LINE_BYTES, 1024 * 1024);
    }

    #[test]
    fn parses_hello_request() {
        let line = r#"{"id":7,"cmd":"hello","args":{"protocol_version":1}}"#;
        match parse_request(line) {
            ParsedRequest::Hello { id, protocol_version } => {
                assert_eq!(id, 7);
                assert_eq!(protocol_version, 1);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn parses_unit_command_without_args() {
        let line = r#"{"id":1,"cmd":"capture.stop"}"#;
        match parse_request(line) {
            ParsedRequest::CaptureStop { id } => assert_eq!(id, 1),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn parses_capture_start_with_the_contract_field_names() {
        let line = r#"{"id":2,"cmd":"capture.start","args":{"session_id":"s","sample_rate":16000,"frame_ms":100,"mic":{"enabled":true},"system":{"enabled":true,"pids":[10,11],"bundle_ids":["com.microsoft.teams2"]}}}"#;
        match parse_request(line) {
            ParsedRequest::CaptureStart { id, args } => {
                assert_eq!(id, 2);
                assert_eq!(args.session_id, "s");
                assert_eq!(args.sample_rate, 16000);
                assert_eq!(args.frame_ms, 100);
                assert!(args.mic.enabled);
                assert_eq!(args.mic.device_uid, None);
                assert_eq!(args.system.pids, vec![10, 11]);
                assert_eq!(args.system.bundle_ids, vec!["com.microsoft.teams2"]);
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn unknown_command_is_reported_for_the_id() {
        let line = r#"{"id":9,"cmd":"nope"}"#;
        match parse_request(line) {
            ParsedRequest::Unknown { id, cmd } => {
                assert_eq!(id, 9);
                assert_eq!(cmd, "nope");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn known_but_unimplemented_command_is_flagged() {
        let line = r#"{"id":5,"cmd":"devices.list"}"#;
        match parse_request(line) {
            ParsedRequest::NotImplementedYet { id, cmd } => {
                assert_eq!(id, 5);
                assert_eq!(cmd, "devices.list");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn bad_json_line_keeps_the_id_when_present() {
        match parse_request("{not json}") {
            ParsedRequest::BadLine { id } => assert_eq!(id, None),
            other => panic!("unexpected {other:?}"),
        }
        match parse_request(r#"{"id":3,"cmd":"capture.start","args":{"sample_rate":1}}"#) {
            ParsedRequest::BadLine { id } => assert_eq!(id, Some(3)),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn audio_frame_fixture_from_the_contract_roundtrips() {
        // Línea tomada del test `parses_audio_frame_event_line` del contrato.
        let line = r#"{"type":"event","ts":1731750000123,"event":"audio.frame","data":{"track":"system","seq":42,"pts_us":4200000,"sample_rate":16000,"channels":1,"format":"s16le","data":"AAA="}}"#;
        let parsed: EventLine = serde_json::from_str(line).expect("de");
        assert_eq!(parsed.ts, 1_731_750_000_123);
        match parsed.event {
            EventOut::AudioFrame(frame) => {
                assert_eq!(frame.track, TrackName::System);
                assert_eq!(frame.seq, 42);
                assert_eq!(frame.pts_us, 4_200_000);
                assert_eq!(frame.sample_rate, 16_000);
                assert_eq!(frame.channels, 1);
                assert_eq!(frame.format, "s16le");
                assert_eq!(frame.data, "AAA=");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn our_audio_frame_serializes_to_the_contract_shape() {
        let line = event(EventOut::AudioFrame(AudioFrameEvent {
            track: TrackName::Mic,
            seq: 0,
            pts_us: 0,
            sample_rate: 16_000,
            channels: 1,
            format: "s16le".into(),
            data: "AAA=".into(),
        }));
        let value = serde_json::to_value(&line).expect("ser");
        assert_eq!(
            value,
            json!({"type":"event","ts":value["ts"],"event":"audio.frame","data":{
                "track":"mic","seq":0,"pts_us":0,"sample_rate":16000,"channels":1,"format":"s16le","data":"AAA="
            }})
        );
    }

    #[test]
    fn response_lines_match_the_contract_fixtures() {
        let ok = ok_response(7, json!({"sidecar_version":"0.1.2","started_at_unix_ms":1}));
        let ok_value = serde_json::to_value(&ok).expect("ser");
        assert_eq!(
            ok_value,
            json!({"type":"response","id":7,"ok":true,"result":{"sidecar_version":"0.1.2","started_at_unix_ms":1}})
        );
        let error = error_response(4, ErrorCode::PermissionDenied, "tcc");
        let error_value = serde_json::to_value(&error).expect("ser");
        assert_eq!(
            error_value,
            json!({"type":"response","id":4,"ok":false,"error":{"code":"permission_denied","message":"tcc"}})
        );
    }

    #[test]
    fn empty_result_stays_an_object() {
        let value = serde_json::to_value(ok_response(1, empty_result())).expect("ser");
        assert_eq!(value, json!({"type":"response","id":1,"ok":true,"result":{}}));
    }

    #[test]
    fn track_state_and_overrun_use_the_contract_names() {
        let state = serde_json::to_value(event(EventOut::AudioTrackState(AudioTrackStateEvent {
            track: TrackName::Mic,
            state: TrackStateName::Stopped,
            message: None,
        })))
        .expect("ser");
        let data = state["data"].as_object().expect("object");
        assert_eq!(state["event"], "audio.track_state");
        assert!(!data.contains_key("message"), "message ausente cuando es None");
        assert_eq!(data["state"], "stopped");
        assert_eq!(data["track"], "mic");

        let overrun =
            serde_json::to_value(event(EventOut::AudioOverrun(AudioOverrunEvent { track: TrackName::System, dropped_frames: 3 })))
                .expect("ser");
        assert_eq!(overrun["event"], "audio.overrun");
        assert_eq!(overrun["data"]["dropped_frames"], 3);
    }
}
