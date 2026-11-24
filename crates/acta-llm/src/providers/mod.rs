//! Proveedores LLM (WP3). Ver docs/01-arquitectura.md §7.2 para las líneas de
//! comando exactas de `claude` y `codex`.
//!
//! Los CLI se lanzan SOLO con `process::run_supervised`, con el binario
//! instalado por el usuario (ruta absoluta + `extra_env`). No se leen ni se
//! copian credenciales de Claude.ai / ChatGPT. Las claves de API se piden a un
//! [`SecretResolver`] que implementa la app (Llavero).

pub mod anthropic;
pub mod claude;
pub mod codex;
pub mod common;
pub mod detect;
pub(crate) mod http;
pub mod models;
pub mod openai;
pub mod opencode;
pub mod presets;
pub mod sse;

pub use anthropic::AnthropicProvider;
pub use claude::ClaudeCliProvider;
pub use codex::CodexCliProvider;
pub use openai::OpenAiCompatibleProvider;
pub use opencode::OpenCodeCliProvider;

use acta_core::llm::*;
use acta_core::{PortError, PortResult};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Resuelve una referencia al Llavero a su valor. Impl: app (keyring).
pub trait SecretResolver: Send + Sync {
    fn resolve(&self, secret: &SecretRef) -> PortResult<String>;
}

/// Resolver para tests y para quien no use Llavero: mapa `service/account` → valor.
#[derive(Debug, Default, Clone)]
pub struct StaticSecrets(pub HashMap<String, String>);

impl StaticSecrets {
    pub fn key(secret: &SecretRef) -> String {
        format!("{}/{}", secret.service, secret.account)
    }
}

impl SecretResolver for StaticSecrets {
    fn resolve(&self, secret: &SecretRef) -> PortResult<String> {
        self.0
            .get(&Self::key(secret))
            .cloned()
            .ok_or_else(|| PortError::NotFound(format!("secreto {} no encontrado en el Llavero", Self::key(secret))))
    }
}

pub struct DefaultProviderFactory {
    secrets: Arc<dyn SecretResolver>,
    /// Quien ejecuta el modelo local integrado (`ProviderKind::BuiltIn`).
    local: Option<Arc<dyn crate::local::LocalRuntime>>,
}

impl DefaultProviderFactory {
    pub fn new(secrets: Arc<dyn SecretResolver>) -> Self {
        Self { secrets, local: None }
    }

    pub fn with_local(mut self, local: Arc<dyn crate::local::LocalRuntime>) -> Self {
        self.local = Some(local);
        self
    }
}

impl LlmProviderFactory for DefaultProviderFactory {
    fn build(&self, config: &ProviderConfig) -> PortResult<Arc<dyn LlmProvider>> {
        let timeout = Duration::from_secs(u64::from(config.timeout_s.max(1)));
        let id = config.id.clone();
        Ok(match &config.kind {
            ProviderKind::ClaudeCli { binary_path, model, effort, extra_env } => Arc::new(
                ClaudeCliProvider::new(id, binary_path.clone(), model.clone(), extra_env.clone(), timeout)
                    .with_effort(effort.clone()),
            ),
            ProviderKind::CodexCli { binary_path, model, extra_env } => Arc::new(CodexCliProvider::new(
                id,
                binary_path.clone(),
                model.clone(),
                extra_env.clone(),
                timeout,
            )),
            ProviderKind::OpenCodeCli { binary_path, model, extra_env } => Arc::new(OpenCodeCliProvider::new(
                id,
                binary_path.clone(),
                model.clone(),
                extra_env.clone(),
                timeout,
            )),
            ProviderKind::OpenAiCompatible { base_url, model, api_key } => {
                if base_url.trim().is_empty() {
                    return Err(PortError::InvalidInput("la URL base del proveedor está vacía".into()));
                }
                let key = api_key.as_ref().map(|r| self.secrets.resolve(r)).transpose()?;
                Arc::new(OpenAiCompatibleProvider::new(id, base_url, model.clone(), key, timeout))
            }
            ProviderKind::Anthropic { model, api_key } => {
                let key = self.secrets.resolve(api_key)?;
                Arc::new(AnthropicProvider::new(id, model.clone(), key, timeout))
            }
            ProviderKind::BuiltIn { model_id } => {
                let local = self.local.clone().ok_or_else(|| {
                    PortError::Unavailable("El modelo local integrado no está disponible en esta instalación.".into())
                })?;
                Arc::new(crate::local::BuiltInProvider::new(id, model_id.clone(), local, timeout))
            }
        })
    }
}

/// "Probar proveedor" de Ajustes: una petición mínima y latencia.
pub async fn test_provider(provider: &dyn LlmProvider) -> ProviderTestResult {
    let started = Instant::now();
    let request = LlmRequest {
        system: None,
        messages: vec![LlmMessage {
            role: Role::User,
            content: "Responde solo con la palabra: ok".into(),
        }],
        json_schema: None,
        max_tokens: Some(16),
        temperature: Some(0.0),
    };
    let latency = || Some(started.elapsed().as_millis() as u64);
    match provider.complete(request).await {
        Ok(r) if !r.text.trim().is_empty() => ProviderTestResult {
            ok: true,
            message: format!("Conexión correcta ({})", r.model.unwrap_or_else(|| provider.id().to_string())),
            latency_ms: latency(),
        },
        Ok(_) => ProviderTestResult { ok: false, message: "El proveedor respondió vacío".into(), latency_ms: latency() },
        Err(e) => ProviderTestResult { ok: false, message: e.to_string(), latency_ms: latency() },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::structured::testing::FakeProvider;
    use acta_core::ids::ProviderId;
    use std::collections::BTreeMap;

    fn cfg(kind: ProviderKind) -> ProviderConfig {
        ProviderConfig { id: ProviderId("p".into()), label: "P".into(), enabled: true, kind, timeout_s: 30 }
    }

    fn secret() -> SecretRef {
        SecretRef { service: "acta".into(), account: "provider:x".into() }
    }

    fn factory(with_secret: bool) -> DefaultProviderFactory {
        let mut m = HashMap::new();
        if with_secret {
            m.insert("acta/provider:x".to_string(), "sk-1".to_string());
        }
        DefaultProviderFactory::new(Arc::new(StaticSecrets(m)))
    }

    #[test]
    fn builds_all_four_kinds() {
        let f = factory(true);
        let bin = std::path::PathBuf::from("/bin/claude");
        let kinds = vec![
            ProviderKind::ClaudeCli { binary_path: bin.clone(), model: None, effort: Some("high".into()), extra_env: BTreeMap::new() },
            ProviderKind::CodexCli { binary_path: bin.clone(), model: None, extra_env: BTreeMap::new() },
            ProviderKind::OpenCodeCli { binary_path: bin, model: None, extra_env: BTreeMap::new() },
            ProviderKind::OpenAiCompatible { base_url: "http://localhost:11434/v1".into(), model: "m".into(), api_key: None },
            ProviderKind::Anthropic { model: "m".into(), api_key: secret() },
        ];
        for k in kinds {
            assert!(f.build(&cfg(k)).is_ok());
        }
    }

    #[test]
    fn missing_secret_and_empty_url_are_errors() {
        let e = factory(false)
            .build(&cfg(ProviderKind::Anthropic { model: "m".into(), api_key: secret() }))
            .err()
            .expect("err");
        assert!(matches!(e, PortError::NotFound(_)));
        let e = factory(true)
            .build(&cfg(ProviderKind::OpenAiCompatible { base_url: " ".into(), model: "m".into(), api_key: None }))
            .err()
            .expect("err");
        assert!(matches!(e, PortError::InvalidInput(_)));
    }

    #[test]
    fn built_in_needs_a_local_runtime() {
        use crate::local::{LocalEndpoint, LocalRuntime};
        struct Rt;
        #[async_trait::async_trait]
        impl LocalRuntime for Rt {
            async fn acquire(&self, _m: &str) -> PortResult<LocalEndpoint> {
                Err(PortError::Unavailable("x".into()))
            }
            fn context_tokens(&self, _m: &str) -> u32 {
                4_096
            }
        }
        let kind = ProviderKind::BuiltIn { model_id: "qwen3.5-2b-q4_k_m".into() };
        let e = factory(true).build(&cfg(kind.clone())).err().expect("err");
        assert!(matches!(e, PortError::Unavailable(m) if m.contains("modelo local integrado")));
        let p = factory(true).with_local(Arc::new(Rt)).build(&cfg(kind)).expect("provider");
        assert_eq!(p.capabilities().context_tokens, 4_096);
        assert!(p.capabilities().native_json_schema);
    }

    #[tokio::test]
    async fn test_provider_reports_ok_and_failure() {
        let ok = FakeProvider::new(vec!["ok"]);
        let r = test_provider(&ok).await;
        assert!(r.ok && r.latency_ms.is_some());
        let bad = FakeProvider::with_results(vec![Err(PortError::Unavailable("claude no encontrado en /x".into()))]);
        let r = test_provider(&bad).await;
        assert!(!r.ok && r.message.contains("claude no encontrado"));
    }
}
