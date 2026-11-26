//! Modelo local integrado (opcional): acta descarga un modelo GGUF y lo
//! ejecuta con su propio `llama-server`, sin Ollama ni nada instalado.
//!
//! - `download`: descargas reanudables con SHA-256.
//! - `runtime`: localizar o instalar `llama-server` (versión fijada).
//! - `server`: supervisor del proceso (arranque perezoso, parada por inactividad).
//! - `provider`: `LlmProvider` sobre la API compatible con OpenAI del servidor.
//! - [`LocalLlm`]: lo junta todo y es lo que usa la app.
//!
//! El catálogo y la recomendación por hardware son lógica pura y viven en
//! `acta_core::llm::local`.

pub mod download;
pub mod provider;
pub mod runtime;
pub mod server;

pub use provider::{BuiltInProvider, LocalEndpoint, LocalRuntime};

use acta_core::layout::DataLayout;
use acta_core::llm::local::*;
use acta_core::{PortError, PortResult};
use async_trait::async_trait;
use download::{CancelFlag, DownloadSpec, Progress, part_path};
use runtime::{RuntimeRelease, RuntimeSearch, install_runtime};
use server::{LlamaSupervisor, ServerSpec, SupervisorTimings};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

/// Tras este tiempo sin peticiones el modelo se descarga de memoria.
pub const IDLE_STOP: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone)]
pub struct LocalLlmConfig {
    /// `<datos>/models/llm`.
    pub models_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub catalog: Vec<LocalModel>,
    pub release: RuntimeRelease,
    pub search: RuntimeSearch,
    pub timings: SupervisorTimings,
}

impl LocalLlmConfig {
    /// Configuración real: catálogo de acta, versión fijada de llama.cpp y
    /// las rutas de `DataLayout`.
    pub fn for_layout(layout: &DataLayout) -> Self {
        let release = RuntimeRelease::pinned();
        Self {
            models_dir: layout.llm_models_dir(),
            logs_dir: layout.logs_dir(),
            catalog: catalog(),
            search: RuntimeSearch::system(layout.llama_runtime_dir(), &release.tag),
            release,
            timings: SupervisorTimings::default(),
        }
    }
}

pub struct LocalLlm {
    cfg: LocalLlmConfig,
    supervisor: LlamaSupervisor,
    client: reqwest::Client,
    /// Descargas en curso: modelo → bandera para cancelarla.
    downloads: Mutex<HashMap<String, CancelFlag>>,
}

fn fraction(received: u64, total: u64) -> f32 {
    if total == 0 { 0.0 } else { (received as f64 / total as f64).clamp(0.0, 1.0) as f32 }
}

impl LocalLlm {
    pub fn new(cfg: LocalLlmConfig) -> Self {
        let supervisor = LlamaSupervisor::new(cfg.timings);
        Self { cfg, supervisor, client: reqwest::Client::new(), downloads: Mutex::default() }
    }

    fn model(&self, id: &str) -> PortResult<&LocalModel> {
        self.cfg
            .catalog
            .iter()
            .find(|m| m.id == id)
            .ok_or_else(|| PortError::NotFound(format!("No existe el modelo local «{id}».")))
    }

    pub fn model_path(&self, model: &LocalModel) -> PathBuf {
        self.cfg.models_dir.join(&model.file)
    }

    fn pid_file(&self) -> PathBuf {
        self.cfg.search.runtime_dir.join("llama-server.pid")
    }

    fn downloading(&self, id: &str) -> bool {
        self.downloads.lock().unwrap_or_else(|e| e.into_inner()).contains_key(id)
    }

    /// Estado para la UI: qué cabe en este Mac, qué hay descargado y qué se recomienda.
    pub async fn status(&self, hardware: HardwareInfo) -> LocalLlmStatus {
        let recommended = recommend(&self.cfg.catalog, &hardware);
        let mut models = Vec::with_capacity(self.cfg.catalog.len());
        for m in &self.cfg.catalog {
            let path = self.model_path(m);
            let downloaded = tokio::fs::metadata(&path).await.is_ok_and(|md| md.is_file());
            let partial_bytes = if downloaded { 0 } else { tokio::fs::metadata(part_path(&path)).await.map_or(0, |md| md.len()) };
            let on_disk = if downloaded { m.size_bytes } else { partial_bytes };
            models.push(LocalModelStatus {
                fit: fit(m, &hardware, on_disk),
                recommended: recommended.as_deref() == Some(m.id.as_str()),
                downloaded,
                partial_bytes,
                downloading: self.downloading(&m.id),
                expected_tokens_per_s: expected_tokens_per_s(m, &hardware),
                model: m.clone(),
            });
        }
        let located = self.cfg.search.locate();
        LocalLlmStatus {
            hardware,
            runtime: LocalRuntimeStatus {
                available: located.is_some(),
                source: located.as_ref().map(|l| l.source),
                path: located.map(|l| l.path.to_string_lossy().into_owned()),
                pinned_version: self.cfg.release.tag.clone(),
                download_bytes: self.cfg.release.size_bytes,
            },
            models,
            recommended,
            running: self.supervisor.running().await.map(|(model, _)| model),
        }
    }

    /// Descarga el modelo (y antes llama.cpp si acta aún no tiene el suyo).
    /// Reanuda lo que hubiera a medias. Cada avance llega a `on_event`; el
    /// último evento lleva `done: true` y, si falló, `error`.
    pub async fn download(&self, model_id: &str, on_event: &(dyn Fn(LocalLlmDownload) + Send + Sync)) -> PortResult<()> {
        let model = self.model(model_id)?.clone();
        let cancel = CancelFlag::default();
        {
            let mut downloads = self.downloads.lock().unwrap_or_else(|e| e.into_inner());
            if downloads.contains_key(model_id) {
                return Err(PortError::InvalidInput("Ese modelo ya se está descargando.".into()));
            }
            downloads.insert(model_id.to_owned(), cancel.clone());
        }
        let result = self.download_inner(&model, &cancel, on_event).await;
        self.downloads.lock().unwrap_or_else(|e| e.into_inner()).remove(model_id);
        let error = match &result {
            Ok(()) => None,
            Err(PortError::Cancelled) => Some("Descarga en pausa. Puedes reanudarla cuando quieras.".to_owned()),
            Err(e) => Some(e.to_string()),
        };
        on_event(LocalLlmDownload {
            model_id: model.id.clone(),
            stage: LocalDownloadStage::Done,
            received_bytes: if result.is_ok() { model.size_bytes } else { 0 },
            total_bytes: model.size_bytes,
            fraction: if result.is_ok() { 1.0 } else { 0.0 },
            done: true,
            error,
        });
        result
    }

    async fn download_inner(&self, model: &LocalModel, cancel: &CancelFlag, on_event: &(dyn Fn(LocalLlmDownload) + Send + Sync)) -> PortResult<()> {
        let event = |stage: LocalDownloadStage, p: Progress| LocalLlmDownload {
            model_id: model.id.clone(),
            stage: if p.verifying { LocalDownloadStage::Verifying } else { stage },
            received_bytes: p.received,
            total_bytes: p.total,
            fraction: fraction(p.received, p.total),
            done: false,
            error: None,
        };

        // 1) llama.cpp. Lo que ponga el usuario (entorno) o venga con la app
        //    manda; si no, la versión fijada. Homebrew queda de respaldo si
        //    GitHub no es accesible.
        let own = self.cfg.search.locate().is_some_and(|l| l.source != RuntimeSource::Homebrew);
        if !own {
            let installed = install_runtime(&self.client, &self.cfg.release, &self.cfg.search.runtime_dir, cancel, &|p| {
                // El paso de verificación del runtime es instantáneo: no se anuncia.
                if !p.verifying {
                    on_event(event(LocalDownloadStage::Runtime, p));
                }
            })
            .await;
            match installed {
                Ok(_) => {}
                Err(PortError::Cancelled) => return Err(PortError::Cancelled),
                Err(e) if self.cfg.search.locate().is_some() => {
                    tracing::warn!("no se pudo descargar llama.cpp ({e}); se usa el de Homebrew");
                }
                Err(e) => {
                    return Err(PortError::Unavailable(format!(
                        "No se pudo descargar llama.cpp: {e}. Si tu red bloquea GitHub, instala `brew install llama.cpp` y vuelve a intentarlo."
                    )));
                }
            }
        }

        // 2) el modelo.
        let spec = DownloadSpec {
            url: model.url.clone(),
            dest: self.model_path(model),
            sha256: model.sha256.clone(),
            size_bytes: Some(model.size_bytes),
        };
        download::download(&self.client, &spec, cancel, &|p| on_event(event(LocalDownloadStage::Model, p))).await
    }

    /// Pausa la descarga (lo bajado queda para reanudar).
    pub fn cancel(&self, model_id: &str) -> bool {
        match self.downloads.lock().unwrap_or_else(|e| e.into_inner()).get(model_id) {
            Some(flag) => {
                flag.cancel();
                true
            }
            None => false,
        }
    }

    /// Borra el modelo (y su descarga a medias). Devuelve los bytes liberados.
    pub async fn delete(&self, model_id: &str) -> PortResult<u64> {
        let model = self.model(model_id)?.clone();
        self.cancel(model_id);
        if self.supervisor.running().await.is_some_and(|(running, _)| running == model.id) {
            self.supervisor.stop().await;
        }
        let path = self.model_path(&model);
        let mut freed = 0;
        for p in [part_path(&path), path] {
            if let Ok(md) = tokio::fs::metadata(&p).await {
                tokio::fs::remove_file(&p).await?;
                freed += md.len();
            }
        }
        Ok(freed)
    }

    /// Para el servidor si lleva [`IDLE_STOP`] sin uso.
    pub async fn stop_if_idle(&self) -> bool {
        self.supervisor.stop_if_idle(IDLE_STOP).await
    }

    /// Al arrancar la app: mata un servidor huérfano de un cierre brusco.
    pub async fn kill_stale(&self) -> bool {
        server::kill_stale(&self.pid_file()).await
    }

    /// Al cerrar la app.
    pub async fn shutdown(&self) {
        for flag in self.downloads.lock().unwrap_or_else(|e| e.into_inner()).values() {
            flag.cancel();
        }
        self.supervisor.stop().await;
    }
}

#[async_trait]
impl LocalRuntime for LocalLlm {
    async fn acquire(&self, model_id: &str) -> PortResult<LocalEndpoint> {
        let model = self.model(model_id)?;
        let model_path = self.model_path(model);
        if !tokio::fs::metadata(&model_path).await.is_ok_and(|md| md.is_file()) {
            return Err(PortError::Unavailable(format!(
                "El modelo local {} no está descargado. Descárgalo en Ajustes › Proveedores de IA.",
                model.name
            )));
        }
        let Some(located) = self.cfg.search.locate() else {
            return Err(PortError::Unavailable(
                "Falta llama.cpp para ejecutar el modelo local. Vuelve a pulsar «Descargar» en Ajustes › Proveedores de IA.".into(),
            ));
        };
        let spec = ServerSpec {
            binary: located.path,
            model_path,
            model_id: model.id.clone(),
            context_tokens: model.context_tokens,
            log_path: Some(self.cfg.logs_dir.join("llama-server.log")),
            pid_file: Some(self.pid_file()),
        };
        let (base_url, lease) = self.supervisor.ensure(&spec).await?;
        Ok(LocalEndpoint { base_url, lease: Some(lease) })
    }

    fn context_tokens(&self, model_id: &str) -> u32 {
        self.model(model_id).map_or(8_192, |m| m.context_tokens)
    }
}

#[cfg(test)]
mod tests {
    use super::download::testing::{FileServer, FileServerOptions, payload, sha256_hex};
    use super::runtime::testing::{fake_tarball, touch_executable};
    use super::server::testing::{FAKE_SERVER, alive};
    use super::*;
    use acta_core::llm::{LlmMessage, LlmProvider, LlmRequest, Role};
    use std::path::Path;
    use std::sync::Arc;

    struct Rig {
        dir: tempfile::TempDir,
        llm: Arc<LocalLlm>,
        model_srv: FileServer,
        runtime_srv: FileServer,
        events: Arc<Mutex<Vec<LocalLlmDownload>>>,
        body: Vec<u8>,
    }

    const MODEL: &str = "qwen3.5-2b-q4_k_m";

    fn hw() -> HardwareInfo {
        HardwareInfo { ram_bytes: 32 * GIB, chip: "Apple M2 Pro".into(), apple_silicon: true, free_disk_bytes: Some(100 * GIB) }
    }

    async fn rig_with(model_opts: FileServerOptions, homebrew: bool) -> Rig {
        let dir = tempfile::tempdir().expect("tmp");
        let body = payload(350_000);
        let tarball = fake_tarball(FAKE_SERVER);
        let model_srv = FileServer::start(body.clone(), model_opts).await;
        let runtime_srv = FileServer::start(tarball.clone(), FileServerOptions::default()).await;
        let mut models = catalog();
        models[0].url = model_srv.url.clone();
        models[0].sha256 = sha256_hex(&body);
        models[0].size_bytes = body.len() as u64;
        let brew = dir.path().join("brew/llama-server");
        if homebrew {
            super::server::testing::write_executable(&brew, FAKE_SERVER);
        }
        let cfg = LocalLlmConfig {
            models_dir: dir.path().join("models/llm"),
            logs_dir: dir.path().join("logs"),
            catalog: models,
            release: RuntimeRelease { tag: "b1".into(), url: runtime_srv.url.clone(), sha256: sha256_hex(&tarball), size_bytes: tarball.len() as u64 },
            search: RuntimeSearch {
                env_override: None,
                exe_dir: None,
                runtime_dir: dir.path().join("runtime/llama"),
                release_tag: "b1".into(),
                homebrew: vec![brew],
            },
            timings: SupervisorTimings { start_timeout: Duration::from_secs(10), poll: Duration::from_millis(30) },
        };
        Rig { dir, llm: Arc::new(LocalLlm::new(cfg)), model_srv, runtime_srv, events: Arc::default(), body }
    }

    async fn rig() -> Rig {
        rig_with(FileServerOptions::default(), false).await
    }

    async fn download(r: &Rig) -> PortResult<()> {
        let events = r.events.clone();
        r.llm.download(MODEL, &move |e| events.lock().expect("lock").push(e)).await
    }

    fn stages(r: &Rig) -> Vec<LocalDownloadStage> {
        let mut out: Vec<LocalDownloadStage> = Vec::new();
        for e in r.events.lock().expect("lock").iter() {
            if out.last() != Some(&e.stage) {
                out.push(e.stage);
            }
        }
        out
    }

    fn model_file(r: &Rig) -> PathBuf {
        r.dir.path().join("models/llm/Qwen3.5-2B-Q4_K_M.gguf")
    }

    #[tokio::test]
    async fn nothing_downloaded_reports_the_catalog_with_a_recommendation_for_this_mac() {
        let r = rig().await;
        let s = r.llm.status(hw()).await;
        assert!(!s.runtime.available && s.runtime.source.is_none());
        assert_eq!(s.runtime.pinned_version, "b1");
        assert_eq!(s.models.len(), 3);
        assert!(s.models.iter().all(|m| !m.downloaded && !m.downloading && m.partial_bytes == 0));
        assert_eq!(s.recommended.as_deref(), Some("qwen3.5-9b-q4_k_m"));
        assert_eq!(s.models.iter().filter(|m| m.recommended).count(), 1);
        assert!(s.models[0].expected_tokens_per_s > s.models[2].expected_tokens_per_s);
        assert_eq!(s.running, None);
    }

    #[tokio::test]
    async fn download_installs_llama_cpp_then_the_model_and_reports_each_stage() {
        let r = rig().await;
        download(&r).await.expect("download");
        assert_eq!(stages(&r), [LocalDownloadStage::Runtime, LocalDownloadStage::Model, LocalDownloadStage::Verifying, LocalDownloadStage::Done]);
        let last = r.events.lock().expect("lock").last().cloned().expect("last");
        assert!(last.done && last.error.is_none() && (last.fraction - 1.0).abs() < f32::EPSILON);
        assert_eq!(std::fs::read(model_file(&r)).expect("model"), r.body);
        assert!(r.dir.path().join("runtime/llama/b1/llama-server").is_file());

        let s = r.llm.status(hw()).await;
        assert_eq!((s.runtime.available, s.runtime.source), (true, Some(RuntimeSource::Downloaded)));
        assert!(s.models[0].downloaded && !s.models[1].downloaded);

        // Descargar otra vez no baja nada.
        download(&r).await.expect("again");
        assert_eq!((r.model_srv.ranges().len(), r.runtime_srv.ranges().len()), (1, 1));
    }

    #[tokio::test]
    async fn an_interrupted_download_shows_as_partial_and_resumes() {
        let r = rig_with(FileServerOptions { cut_first_after: Some(120_000), ..Default::default() }, false).await;
        let err = download(&r).await.expect_err("cut");
        assert!(matches!(err, PortError::Unavailable(_)));
        let last = r.events.lock().expect("lock").last().cloned().expect("last");
        assert!(last.done && last.error.is_some());
        let s = r.llm.status(hw()).await;
        assert!(!s.models[0].downloaded && s.models[0].partial_bytes > 0 && !s.models[0].downloading);

        download(&r).await.expect("resume");
        assert_eq!(std::fs::read(model_file(&r)).expect("model"), r.body);
        assert!(r.model_srv.ranges()[1].as_deref().is_some_and(|v| v.starts_with("bytes=")));
        assert_eq!(r.runtime_srv.ranges().len(), 1, "llama.cpp no se baja dos veces");
    }

    #[tokio::test]
    async fn cancelling_pauses_the_download_and_says_so() {
        let r = rig_with(FileServerOptions { chunk_delay_ms: 20, ..Default::default() }, false).await;
        let llm = r.llm.clone();
        tokio::spawn(async move {
            for _ in 0..200 {
                if llm.cancel(MODEL) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        });
        assert_eq!(download(&r).await, Err(PortError::Cancelled));
        let last = r.events.lock().expect("lock").last().cloned().expect("last");
        assert!(last.error.as_deref().is_some_and(|m| m.contains("en pausa")));
        assert!(!r.llm.cancel(MODEL), "ya no hay descarga que cancelar");
    }

    #[tokio::test]
    async fn the_same_model_cannot_be_downloaded_twice_at_once_and_unknown_models_are_rejected() {
        let r = rig_with(FileServerOptions { chunk_delay_ms: 20, ..Default::default() }, false).await;
        let (llm, events) = (r.llm.clone(), r.events.clone());
        let first = tokio::spawn(async move { llm.download(MODEL, &move |e| events.lock().expect("lock").push(e)).await });
        for _ in 0..200 {
            if r.llm.status(hw()).await.models[0].downloading {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(matches!(r.llm.download(MODEL, &|_| {}).await, Err(PortError::InvalidInput(_))));
        assert!(matches!(r.llm.download("nope", &|_| {}).await, Err(PortError::NotFound(_))));
        first.await.expect("join").expect("first download");
    }

    #[tokio::test]
    async fn a_corrupt_model_is_rejected_and_never_used() {
        let r = rig().await;
        // El servidor entrega otros bytes que los del catálogo.
        let mut cfg = r.llm.cfg.clone();
        cfg.catalog[0].sha256 = "a".repeat(64);
        let llm = LocalLlm::new(cfg);
        let err = llm.download(MODEL, &|_| {}).await.expect_err("sha");
        assert!(matches!(&err, PortError::InvalidResponse(m) if m.contains("SHA-256")));
        assert!(!model_file(&r).exists());
        assert!(matches!(llm.acquire(MODEL).await, Err(PortError::Unavailable(m)) if m.contains("no está descargado")));
    }

    #[tokio::test]
    #[cfg_attr(not(unix), ignore = "Homebrew no existe en Windows")]
    async fn when_github_is_blocked_the_homebrew_llama_server_is_the_fallback() {
        let mut r = rig_with(FileServerOptions::default(), true).await;
        r.runtime_srv = FileServer::start(vec![], FileServerOptions { status: Some(503), ..Default::default() }).await;
        let mut cfg = r.llm.cfg.clone();
        cfg.release.url = r.runtime_srv.url.clone();
        let llm = Arc::new(LocalLlm::new(cfg));
        llm.download(MODEL, &|_| {}).await.expect("download with fallback");
        assert_eq!(llm.status(hw()).await.runtime.source, Some(RuntimeSource::Homebrew));
        let endpoint = llm.acquire(MODEL).await.expect("server");
        assert!(endpoint.base_url.starts_with("http://127.0.0.1:"));
        llm.shutdown().await;
    }

    #[tokio::test]
    async fn without_any_llama_cpp_the_error_says_what_to_do() {
        let mut r = rig().await;
        r.runtime_srv = FileServer::start(vec![], FileServerOptions { status: Some(503), ..Default::default() }).await;
        let mut cfg = r.llm.cfg.clone();
        cfg.release.url = r.runtime_srv.url.clone();
        let llm = LocalLlm::new(cfg);
        let err = llm.download(MODEL, &|_| {}).await.expect_err("no runtime");
        assert!(matches!(&err, PortError::Unavailable(m) if m.contains("brew install llama.cpp")), "{err}");
        assert_eq!(r.model_srv.ranges().len(), 0, "sin runtime no se bajan 1,3 GB");
    }

    #[tokio::test]
    #[cfg_attr(not(unix), ignore = "el runtime integrado fijado es un binario de macOS")]
    async fn the_provider_runs_the_downloaded_model_end_to_end_and_delete_frees_everything() {
        let r = rig().await;
        download(&r).await.expect("download");
        let provider = BuiltInProvider::new("builtin".into(), MODEL.into(), r.llm.clone(), Duration::from_secs(10));
        assert_eq!(provider.capabilities().context_tokens, 16_384);
        assert_eq!(r.llm.status(hw()).await.running, None, "no arranca hasta la primera petición");

        let resp = provider
            .complete(LlmRequest {
                system: None,
                messages: vec![LlmMessage { role: Role::User, content: "hola".into() }],
                json_schema: Some(serde_json::json!({"type": "object"})),
                max_tokens: Some(8),
                temperature: None,
            })
            .await
            .expect("complete");
        assert_eq!(resp.json.expect("json")["a"], 1);
        assert_eq!(r.llm.status(hw()).await.running.as_deref(), Some(MODEL));
        let args = std::fs::read_to_string(r.dir.path().join("runtime/llama/b1/llama-server.args")).expect("args");
        assert!(args.contains("Qwen3.5-2B-Q4_K_M.gguf") && args.contains("16384"));
        let pid: u32 = std::fs::read_to_string(r.dir.path().join("runtime/llama/llama-server.pid")).expect("pid").parse().expect("pid");

        let freed = r.llm.delete(MODEL).await.expect("delete");
        assert_eq!(freed, r.body.len() as u64);
        assert!(!model_file(&r).exists());
        for _ in 0..100 {
            if !alive(pid).await {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!alive(pid).await, "borrar el modelo para su servidor");
        assert_eq!(r.llm.status(hw()).await.running, None);
        assert_eq!(r.llm.delete(MODEL).await.expect("idempotente"), 0);
    }

    #[tokio::test]
    async fn the_real_layout_keeps_models_and_runtime_inside_the_data_dir() {
        let cfg = LocalLlmConfig::for_layout(&DataLayout::new("/data"));
        assert_eq!(cfg.models_dir, Path::new("/data/models/llm"));
        assert_eq!(cfg.search.runtime_dir, Path::new("/data/runtime/llama"));
        assert_eq!(cfg.search.release_tag, cfg.release.tag);
        assert_eq!(cfg.catalog.len(), 3);
        let llm = LocalLlm::new(cfg);
        assert_eq!(llm.pid_file(), Path::new("/data/runtime/llama/llama-server.pid"));
        let _ = touch_executable; // usado por otros módulos de test
    }
}
