//! Elección del proveedor LLM según Ajustes y resolución de claves del Llavero.

use acta_core::ids::ProviderId;
use acta_core::llm::{LlmProvider, LlmProviderFactory, ProviderConfig, ProviderKind, SecretRef};
use acta_core::settings::Settings;
use acta_core::{PortError, PortResult};
use acta_llm::providers::SecretResolver;
use acta_store::secrets::SecretStore;
use std::sync::Arc;

/// `SecretResolver` de acta-llm sobre el `SecretStore` (Llavero) de acta-store.
pub struct KeychainResolver(pub Arc<dyn SecretStore>);

impl SecretResolver for KeychainResolver {
    fn resolve(&self, secret: &SecretRef) -> PortResult<String> {
        self.0.get(secret)?.ok_or_else(|| {
            PortError::NotFound("falta la clave de API en el Llavero: guárdala en Ajustes › Proveedores".into())
        })
    }
}

/// Modelo configurado (para apuntarlo en el acta).
pub fn model_of(cfg: &ProviderConfig) -> Option<String> {
    match &cfg.kind {
        ProviderKind::ClaudeCli { model, .. } | ProviderKind::CodexCli { model, .. } | ProviderKind::CursorCli { model, .. } => {
            model.clone()
        }
        ProviderKind::OpenAiCompatible { model, .. } | ProviderKind::Anthropic { model, .. } => Some(model.clone()),
        ProviderKind::BuiltIn { model_id } => Some(model_id.clone()),
    }
}

/// El proveedor pedido si existe y está activo; si no, el primero activo.
pub fn pick_provider<'a>(settings: &'a Settings, wanted: Option<&ProviderId>) -> PortResult<&'a ProviderConfig> {
    let enabled = || settings.llm.providers.iter().filter(|p| p.enabled);
    wanted
        .and_then(|w| enabled().find(|p| &p.id == w))
        .or_else(|| enabled().next())
        .ok_or_else(|| {
            PortError::Unavailable(if settings.llm.providers.is_empty() {
                // Instalación nueva (o el usuario los quitó todos).
                "No hay ningún proveedor de IA configurado. Añade uno en Ajustes › Proveedores.".into()
            } else {
                "No hay ningún proveedor de IA activo. Actívalo en Ajustes › Proveedores.".into()
            })
        })
}

pub fn build_provider(
    settings: &Settings,
    factory: &dyn LlmProviderFactory,
    wanted: Option<&ProviderId>,
) -> PortResult<(ProviderConfig, Arc<dyn LlmProvider>)> {
    let cfg = pick_provider(settings, wanted)?.clone();
    let provider = factory.build(&cfg)?;
    Ok((cfg, provider))
}

#[cfg(test)]
mod tests {
    use super::*;
    use acta_store::secrets::{InMemorySecretStore, secret_ref_for_provider};

    fn provider(id: &str, enabled: bool) -> ProviderConfig {
        ProviderConfig {
            id: id.into(),
            label: id.into(),
            enabled,
            kind: ProviderKind::OpenAiCompatible { base_url: "http://127.0.0.1:9/v1".into(), model: "m".into(), api_key: None },
            timeout_s: 5,
        }
    }

    #[test]
    fn picks_wanted_enabled_then_first_enabled() {
        let mut s = Settings::default();
        s.llm.providers = vec![provider("uno", true), provider("dos", false)];
        assert_eq!(pick_provider(&s, Some(&"uno".into())).map(|p| p.id.0.as_str()).ok(), Some("uno"));
        // «dos» existe pero está desactivado → cae al primero activo.
        assert_eq!(pick_provider(&s, Some(&"dos".into())).map(|p| p.id.0.as_str()).ok(), Some("uno"));
        for p in &mut s.llm.providers {
            p.enabled = false;
        }
        assert!(matches!(pick_provider(&s, None), Err(PortError::Unavailable(_))));
    }

    /// Instalación nueva: no hay proveedores y se explica qué hacer.
    #[test]
    fn a_fresh_install_has_no_provider_and_says_where_to_add_one() {
        let e = pick_provider(&Settings::default(), None).map(|p| p.id.clone()).expect_err("sin proveedores");
        assert!(matches!(&e, PortError::Unavailable(m) if m.contains("Ajustes › Proveedores")), "{e:?}");
    }

    #[test]
    fn keychain_resolver_reads_the_secret_store() {
        let store = Arc::new(InMemorySecretStore::default());
        let r = secret_ref_for_provider("openrouter");
        let resolver = KeychainResolver(store.clone());
        assert!(matches!(resolver.resolve(&r), Err(PortError::NotFound(_))));
        store.set(&r, "sk-1").unwrap_or_default();
        assert_eq!(resolver.resolve(&r).ok().as_deref(), Some("sk-1"));
    }
}
