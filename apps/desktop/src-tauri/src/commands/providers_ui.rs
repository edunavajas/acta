//! Comandos que solo sirven a la pantalla de proveedores y a la bienvenida:
//! qué modelos ofrece un servidor, qué servidores locales hay en marcha y
//! abrir un enlace en el navegador del usuario.

use super::Core;
use acta_core::api::{ApiError, ApiErrorCode, ApiResult};
use acta_core::ids::ProviderId;
use acta_llm::providers::models::{self, LocalDetection};
use acta_store::secrets::secret_ref_for_provider;
use std::time::Duration;
use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;

/// Listar modelos es una consulta corta: si tarda más, el servidor no está.
const LIST_TIMEOUT: Duration = Duration::from_secs(12);

/// Modelos de texto de un servidor compatible con OpenAI o del CLI de OpenCode
/// (`base_url` con prefijo `opencode:<ruta>`). Si `provider_id` tiene una clave
/// en el Llavero, se usa; la clave nunca pasa por la UI.
#[tauri::command]
pub async fn llm_list_models(
    state: State<'_, Core>,
    base_url: String,
    provider_id: Option<ProviderId>,
) -> ApiResult<Vec<String>> {
    if let Some(bin_path) = base_url.strip_prefix("opencode:") {
        let bin = bin_path.trim();
        if bin.is_empty() {
            return Err(ApiError {
                code: ApiErrorCode::InvalidInput,
                message: "Indica primero la ruta de OpenCode en «Avanzado» o pulsa «Detectar CLI instalados».".into(),
            });
        }
        let extra_env = provider_id
            .as_ref()
            .and_then(|id| {
                state.settings().llm.providers.into_iter().find_map(|p| match p.kind {
                    acta_core::llm::ProviderKind::OpenCodeCli { extra_env, .. } if &p.id == id => Some(extra_env),
                    _ => None,
                })
            })
            .unwrap_or_default();
        return acta_llm::providers::opencode::list_models(std::path::Path::new(bin), &extra_env, LIST_TIMEOUT)
            .await
            .map_err(ApiError::from);
    }
    let key = match &provider_id {
        Some(id) => state.secrets.get(&secret_ref_for_provider(&id.0)).map_err(ApiError::from)?,
        None => None,
    };
    models::list_models(&base_url, key.as_deref(), LIST_TIMEOUT).await.map_err(ApiError::from)
}

/// Ollama y LM Studio en sus puertos por defecto, con sus modelos.
#[tauri::command]
pub async fn llm_detect_local() -> ApiResult<LocalDetection> {
    Ok(models::detect_local().await)
}

/// Solo enlaces web: nada de `file:`, esquemas de apps ni rutas locales.
pub fn is_web_url(url: &str) -> bool {
    let url = url.trim();
    let rest = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"));
    rest.is_some_and(|host| !host.is_empty() && !host.starts_with('/') && !url.contains(char::is_whitespace))
}

/// Abre un enlace en el navegador por defecto (nunca en una ventana de acta).
#[tauri::command]
pub async fn open_external_url(app: AppHandle, url: String) -> ApiResult<()> {
    if !is_web_url(&url) {
        return Err(ApiError { code: ApiErrorCode::InvalidInput, message: format!("No es un enlace web: {url}") });
    }
    app.opener()
        .open_url(url.trim(), None::<&str>)
        .map_err(|e| ApiError { code: ApiErrorCode::Internal, message: e.to_string() })
}

#[cfg(test)]
mod tests {
    use super::is_web_url;

    #[test]
    fn only_web_links_can_be_opened() {
        assert!(is_web_url("https://cloud.nan.builders/r/YZ524NPK"));
        assert!(is_web_url(" http://127.0.0.1:11434 "));
        for bad in ["", "https://", "file:///etc/passwd", "x-apple.systempreferences:foo", "javascript:alert(1)", "/Applications", "https:///x", "https://a b"] {
            assert!(!is_web_url(bad), "{bad}");
        }
    }
}
