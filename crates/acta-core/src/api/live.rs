//! Contrato de la ola 4: transcripción y preguntas EN VIVO, reintento con otro
//! proveedor, búsqueda dentro del transcript y título de grupos.
//!
//! | comando | args (JS, camelCase) | resultado |
//! |---|---|---|
//! | `meeting_retry` | `{ meetingId, steps?, providerId?, model? }` | `null` (el proveedor/modelo solo vale para ESTE reintento: no cambia los ajustes) |
//! | `llm_providers_available` | — | `AvailableProvider[]` (solo los activados, con sus modelos elegibles) |
//! | `live_transcript_get` | `{ sessionId }` | `LiveTranscript` (todos los segmentos hasta ahora) |
//! | `live_ask` | `{ sessionId, question, providerId?, model? }` | `string` (`requestId`; la respuesta llega por `live:answer`) |
//! | `meeting_search_transcript` | `{ meetingId, query }` | `TranscriptMatch[]` (sin distinguir acentos ni mayúsculas) |
//! | `groups_suggest_title` | `{ groupId }` | `GroupTitleSuggestion` (no lo aplica: la UI decide con `group_update`) |
//! | `chat_cancel` | `{ requestId }` (= `assistantMessageId` de `chat_send`) | `null` (corta la generación: mata el CLI o cierra el stream HTTP; guarda lo recibido) |
//! | `live_cancel` | `{ requestId }` (de `live_ask`) | `null` (corta la respuesta; llega un `live:answer` con `done: true`) |
//!
//! | evento | payload | frecuencia |
//! |---|---|---|
//! | `live:transcript` | `LiveTranscript` con los segmentos NUEVOS o CAMBIADOS (la UI los fusiona por `id`) | cada pocos segundos mientras graba |
//! | `live:answer` | `LiveAnswer` | streaming de `live_ask` (el último lleva `done: true`) |
//! | `permissions:changed` | `PermissionChanged` | un permiso cambió (se comprueba al activar la app) |
//!
//! `sessionId` es el `MeetingId` de la grabación en curso
//! (`SessionState::Recording.meetingId`).

use crate::ids::{MeetingId, ProviderId};
use crate::permissions::{PermissionKind, PermissionStatus};
use serde::{Deserialize, Serialize};

/// Quién habla en un segmento en vivo: `"yo"` (micro), `"otros"` (sistema,
/// aún sin diarizar) o un nombre si Teams dice quién habla.
pub const LIVE_SPEAKER_ME: &str = "yo";
pub const LIVE_SPEAKER_OTHERS: &str = "otros";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveSegment {
    /// Estable: un segmento provisional y su versión final comparten id.
    pub id: String,
    pub speaker: String,
    pub text: String,
    /// Tiempo media (como las utterances), desde el inicio de la grabación.
    pub start_ms: u64,
    pub end_ms: u64,
    /// `false` mientras la ventana de audio puede cambiar.
    #[serde(rename = "final")]
    pub is_final: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveTranscript {
    pub session_id: MeetingId,
    pub segments: Vec<LiveSegment>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveCitation {
    pub start_ms: u64,
}

/// Trozo de la respuesta de `live_ask`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LiveAnswer {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delta: Option<String>,
    pub done: bool,
    /// Solo en el último (`done: true`): momentos citados con `[mm:ss]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub citations: Option<Vec<LiveCitation>>,
    /// En castellano, apto para mostrar. Llega con `done: true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Proveedor activado y los modelos que se pueden elegir para «Reintentar con…».
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AvailableProvider {
    pub provider_id: ProviderId,
    pub label: String,
    /// `claudeCli` | `codexCli` | `cursorCli` | `openAiCompatible` | `anthropic` | `builtIn`.
    pub kind: String,
    /// Modelos elegibles. Puede estar vacío (el CLI usa el suyo por defecto).
    pub models: Vec<String>,
    /// El modelo configurado en el proveedor, si hay.
    pub default_model: Option<String>,
    /// Es el proveedor por defecto del acta (`minutesProvider`).
    pub is_default: bool,
    /// Para qué es el proveedor por defecto: `minutes`, `chat`, `naming`.
    pub default_for: Vec<String>,
}

/// Coincidencia de `meeting_search_transcript`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptMatch {
    /// `Utterance.idx`.
    pub utterance_id: u32,
    pub start_ms: u64,
    /// El texto entero de la utterance.
    pub snippet: String,
    /// `[inicio, fin)` en unidades UTF-16 de `snippet` (como `String.slice` en JS).
    pub ranges: Vec<[u32; 2]>,
}

/// `permissions:changed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionChanged {
    pub kind: PermissionKind,
    pub status: PermissionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupTitleSuggestion {
    pub name: String,
    pub description: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_segment_uses_final_on_the_wire() {
        let s = LiveSegment { id: "mic-1".into(), speaker: LIVE_SPEAKER_ME.into(), text: "hola".into(), start_ms: 0, end_ms: 900, is_final: true };
        let v = serde_json::to_value(&s).unwrap_or_default();
        assert_eq!(v["final"], true);
        assert_eq!(v["startMs"], 0);
        assert!(v.get("isFinal").is_none());
    }

    #[test]
    fn live_answer_omits_empty_fields() {
        let a = LiveAnswer { request_id: "r1".into(), delta: Some("Sí".into()), done: false, citations: None, error: None };
        let v = serde_json::to_value(&a).unwrap_or_default();
        assert_eq!(v, serde_json::json!({"requestId": "r1", "delta": "Sí", "done": false}));
    }

    #[test]
    fn transcript_match_ranges_are_pairs() {
        let m = TranscriptMatch { utterance_id: 3, start_ms: 1000, snippet: "Ámbito".into(), ranges: vec![[0, 6]] };
        assert_eq!(serde_json::to_value(&m).unwrap_or_default()["ranges"], serde_json::json!([[0, 6]]));
    }
}

#[cfg(test)]
mod names {
    use crate::api::{command_names as c, event_names as e};

    #[test]
    fn wave4_names_match_the_typescript_contract() {
        assert_eq!(
            [c::MEETING_RETRY, c::LLM_PROVIDERS_AVAILABLE, c::LIVE_TRANSCRIPT_GET, c::LIVE_ASK, c::MEETING_SEARCH_TRANSCRIPT, c::GROUPS_SUGGEST_TITLE],
            ["meeting_retry", "llm_providers_available", "live_transcript_get", "live_ask", "meeting_search_transcript", "groups_suggest_title"]
        );
        assert_eq!([c::CHAT_CANCEL, c::LIVE_CANCEL], ["chat_cancel", "live_cancel"]);
        assert_eq!([e::LIVE_TRANSCRIPT, e::LIVE_ANSWER, e::PERMISSIONS_CHANGED], ["live:transcript", "live:answer", "permissions:changed"]);
    }
}
