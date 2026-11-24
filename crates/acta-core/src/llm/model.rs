use crate::ids::ProviderId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// Referencia a un secreto en el Llavero de macOS (nunca el valor en claro).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretRef {
    /// Servicio del Llavero, p. ej. "acta".
    pub service: String,
    /// Cuenta, p. ej. "provider:openrouter".
    pub account: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ProviderKind {
    /// `claude -p` con el binario que el usuario ya tiene instalado. Nunca se
    /// leen ni copian sus credenciales: se lanza tal cual con su entorno.
    #[serde(rename_all = "camelCase")]
    ClaudeCli {
        /// Ruta absoluta (las apps GUI no heredan el PATH del shell).
        binary_path: PathBuf,
        model: Option<String>,
        /// `--effort <low|medium|high|xhigh|max>`. `None` = el del CLI.
        #[serde(default)]
        effort: Option<String>,
        /// Variables extra (p. ej. `CLAUDE_CONFIG_DIR`).
        extra_env: BTreeMap<String, String>,
    },
    /// `codex exec`.
    #[serde(rename_all = "camelCase")]
    CodexCli {
        binary_path: PathBuf,
        model: Option<String>,
        extra_env: BTreeMap<String, String>,
    },
    /// `opencode run --archived --format json`.
    #[serde(rename = "opencodeCli", rename_all = "camelCase")]
    OpenCodeCli {
        binary_path: PathBuf,
        model: Option<String>,
        extra_env: BTreeMap<String, String>,
    },
    /// Cualquier API compatible con OpenAI (Ollama, LM Studio, OpenRouter).
    #[serde(rename_all = "camelCase")]
    OpenAiCompatible {
        base_url: String,
        model: String,
        api_key: Option<SecretRef>,
    },
    #[serde(rename_all = "camelCase")]
    Anthropic {
        model: String,
        api_key: SecretRef,
    },
    /// Modelo local integrado: acta lo descarga y lo ejecuta con su propio
    /// `llama-server` (sin Ollama). `model_id` es un id de `llm::local::catalog`.
    #[serde(rename_all = "camelCase")]
    BuiltIn { model_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    pub id: ProviderId,
    /// Nombre visible ("Claude Code (CLI)").
    pub label: String,
    pub enabled: bool,
    pub kind: ProviderKind,
    /// Tiempo máximo por llamada (s).
    pub timeout_s: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmMessage {
    pub role: Role,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmRequest {
    pub system: Option<String>,
    pub messages: Vec<LlmMessage>,
    /// JSON Schema de la salida estructurada. El proveedor lo usa de forma
    /// nativa si puede (`claude --json-schema`, `codex exec --output-schema`,
    /// `response_format` OpenAI, tool-use Anthropic) y si no, lo incluye en
    /// el prompt. El llamador SIEMPRE valida la respuesta.
    pub json_schema: Option<serde_json::Value>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmResponse {
    pub text: String,
    /// Si se pidió `json_schema` y el proveedor ya devolvió JSON parseado.
    pub json: Option<serde_json::Value>,
    pub model: Option<String>,
    pub usage: Usage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapabilities {
    pub streaming: bool,
    pub native_json_schema: bool,
    /// Contexto aproximado en tokens (para decidir si trocear).
    pub context_tokens: u32,
}

/// Resultado de "probar proveedor" en Ajustes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTestResult {
    pub ok: bool,
    pub message: String,
    pub latency_ms: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opencode_cli_wire_tag_matches_typescript_contract() {
        let json = r#"{"type":"opencodeCli","binaryPath":"/Users/a/.opencode/bin/opencode","model":"github-copilot/gemini-4-neon","extraEnv":{}}"#;
        let kind: ProviderKind = serde_json::from_str(json).expect("deserialize opencodeCli");
        assert_eq!(
            kind,
            ProviderKind::OpenCodeCli {
                binary_path: PathBuf::from("/Users/a/.opencode/bin/opencode"),
                model: Some("github-copilot/gemini-4-neon".into()),
                extra_env: BTreeMap::new(),
            }
        );
        let serialized = serde_json::to_value(&kind).expect("serialize");
        assert_eq!(serialized["type"], "opencodeCli");
    }
}
