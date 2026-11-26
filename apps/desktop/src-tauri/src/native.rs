//! Adaptadores nativos (sidecar Swift en macOS) y su alternativa "no disponible".

use crate::unavailable::Unavailable;
use acta_core::api::{AudioInput, ModelDownload, ModelStatus};
use acta_core::detection::{MeetingDetector, SpeakerNameSource};
use acta_core::permissions::PermissionsProbe;
use acta_core::recording::AudioSource;
use acta_core::screen::{ClipboardImporter, ScreenCapturer};
use acta_core::transcription::{Diarizer, LiveTranscriber, Transcriber};
use acta_core::{EventStream, PortResult};
use async_trait::async_trait;
use std::sync::Arc;

/// Modelos locales de FluidAudio (sin puerto en acta-core: es cosa de la app).
#[async_trait]
pub trait ModelManager: Send + Sync {
    async fn status(&self) -> PortResult<Vec<ModelStatus>>;
    async fn download(&self, name: &str) -> PortResult<EventStream<ModelDownload>>;
}

/// De dónde sacar el título de una reunión: ventanas de su app y Calendario.
#[async_trait]
pub trait MeetingInfo: Send + Sync {
    async fn window_titles(&self, pid: i32, bundle_id: &str) -> PortResult<Vec<String>>;
    /// Eventos que tocan `[from, to)`. Con `request_access`, pide el permiso
    /// de Calendario si está sin decidir.
    async fn calendar_events(
        &self,
        from: acta_core::time::UnixMs,
        to: acta_core::time::UnixMs,
        request_access: bool,
    ) -> PortResult<Vec<acta_core::library::titles::CalendarEntry>>;
}

/// Lo que macOS sabe de quien usa el Mac y de sus apps (bienvenida y selector de apps).
#[async_trait]
pub trait SystemInfo: Send + Sync {
    /// Nombre completo de la cuenta e idiomas preferidos, en orden.
    async fn profile(&self) -> PortResult<(Option<String>, Vec<String>)>;
    /// Apps instaladas y en marcha. Deja el icono PNG de cada una en `icons_dir`.
    async fn installed_apps(&self, icons_dir: &std::path::Path) -> PortResult<Vec<acta_core::api::InstalledApp>>;
}

/// Micrófonos del sistema (para elegir uno en Ajustes).
#[async_trait]
pub trait InputDevices: Send + Sync {
    async fn inputs(&self) -> PortResult<Vec<AudioInput>>;
}

/// Lado de los iconos de `apps_list`: 64 pt a @2x.
pub const ICON_PX: u32 = 128;

#[derive(Clone)]
pub struct NativePorts {
    pub audio: Arc<dyn AudioSource>,
    pub detector: Arc<dyn MeetingDetector>,
    pub speakers: Arc<dyn SpeakerNameSource>,
    pub screen: Option<Arc<dyn ScreenCapturer>>,
    pub clipboard: Option<Arc<dyn ClipboardImporter>>,
    pub live: Option<Arc<dyn LiveTranscriber>>,
    pub transcriber: Arc<dyn Transcriber>,
    pub diarizer: Arc<dyn Diarizer>,
    pub permissions: Arc<dyn PermissionsProbe>,
    pub models: Arc<dyn ModelManager>,
    pub devices: Arc<dyn InputDevices>,
    pub meeting_info: Arc<dyn MeetingInfo>,
    pub system: Arc<dyn SystemInfo>,
    pub sidecar_version: Option<String>,
    pub macos_version: Option<String>,
    /// Por qué no hay sidecar (para el log y Ajustes).
    pub error: Option<String>,
    #[cfg(target_os = "macos")]
    pub client: Option<acta_mac_bridge::client::SidecarClient>,
}

impl NativePorts {
    pub fn unavailable(reason: impl Into<String>) -> Self {
        let reason = reason.into();
        let u = Arc::new(Unavailable(format!("El componente nativo de acta no está disponible: {reason}")));
        Self {
            audio: u.clone(),
            detector: u.clone(),
            speakers: u.clone(),
            screen: None,
            clipboard: None,
            live: None,
            transcriber: u.clone(),
            diarizer: u.clone(),
            permissions: u.clone(),
            models: u.clone(),
            meeting_info: u.clone(),
            system: u.clone(),
            devices: u,
            sidecar_version: None,
            macos_version: None,
            error: Some(reason),
            #[cfg(target_os = "macos")]
            client: None,
        }
    }
}

#[cfg(target_os = "macos")]
mod mac {
    use super::*;
    use acta_core::layout::DataLayout;
    use acta_mac_bridge::audio::MacAudioSource;
    use acta_mac_bridge::client::{SidecarClient, SidecarConfig, locate_sidecar};
    use acta_mac_bridge::detection::MacMeetingDetector;
    use acta_mac_bridge::permissions::MacPermissions;
    use acta_mac_bridge::screen::{MacClipboardImporter, MacScreenCapturer};
    use acta_mac_bridge::speakers::MacSpeakerNames;
    use acta_mac_bridge::transcription::{MacDiarizer, MacLiveTranscriber, MacModels, MacTranscriber};

    struct Models(MacModels);

    struct Devices(SidecarClient);

    struct Info(SidecarClient);

    #[async_trait]
    impl MeetingInfo for Info {
        async fn window_titles(&self, pid: i32, bundle_id: &str) -> PortResult<Vec<String>> {
            use acta_mac_bridge::protocol::{Command, MeetingTitlesArgs, MeetingTitlesResult};
            let args = MeetingTitlesArgs { pid, bundle_id: bundle_id.to_owned() };
            let r: MeetingTitlesResult = self.0.call_typed(Command::MeetingTitles(args), None).await?;
            Ok(r.titles)
        }
        async fn calendar_events(
            &self,
            from: acta_core::time::UnixMs,
            to: acta_core::time::UnixMs,
            request_access: bool,
        ) -> PortResult<Vec<acta_core::library::titles::CalendarEntry>> {
            use acta_mac_bridge::protocol::{CalendarEventsArgs, CalendarEventsResult, Command};
            let args = CalendarEventsArgs { from_unix_ms: from.0, to_unix_ms: to.0, request_access };
            // Pedir el permiso espera a que el usuario conteste.
            let r: CalendarEventsResult =
                self.0.call_typed(Command::CalendarEvents(args), Some(std::time::Duration::from_secs(120))).await?;
            if r.access != "granted" {
                return Err(acta_core::PortError::PermissionDenied(format!("Calendario: {}", r.access)));
            }
            Ok(r.events
                .into_iter()
                .map(|e| acta_core::library::titles::CalendarEntry { title: e.title, start_ms: e.start_unix_ms, end_ms: e.end_unix_ms, all_day: e.all_day })
                .collect())
        }
    }

    struct System(SidecarClient);

    #[async_trait]
    impl SystemInfo for System {
        async fn profile(&self) -> PortResult<(Option<String>, Vec<String>)> {
            use acta_mac_bridge::protocol::{Command, SystemProfileResult};
            let r: SystemProfileResult = self.0.call_typed(Command::SystemProfile, Some(std::time::Duration::from_secs(5))).await?;
            Ok((r.full_name, r.preferred_languages))
        }
        async fn installed_apps(&self, icons_dir: &std::path::Path) -> PortResult<Vec<acta_core::api::InstalledApp>> {
            use acta_mac_bridge::protocol::{AppsInstalledArgs, AppsInstalledResult, Command};
            let args = AppsInstalledArgs { icons_dir: icons_dir.to_string_lossy().into_owned(), icon_px: ICON_PX };
            // La primera vez dibuja un icono por app (cientos); después salen de la caché.
            let r: AppsInstalledResult = self.0.call_typed(Command::AppsInstalled(args), Some(std::time::Duration::from_secs(60))).await?;
            Ok(r.apps
                .into_iter()
                .map(|a| acta_core::api::InstalledApp { bundle_id: a.bundle_id, name: a.name, path: a.path, running: a.running, icon_path: a.icon_path })
                .collect())
        }
    }

    #[async_trait]
    impl InputDevices for Devices {
        async fn inputs(&self) -> PortResult<Vec<AudioInput>> {
            use acta_mac_bridge::protocol::{Command, DevicesListResult};
            let r: DevicesListResult = self.0.call_typed(Command::DevicesList, None).await?;
            Ok(r.inputs
                .into_iter()
                .map(|d| AudioInput { uid: d.uid, name: d.name, is_default: d.is_default, transport: d.transport })
                .collect())
        }
    }

    #[async_trait]
    impl ModelManager for Models {
        async fn status(&self) -> PortResult<Vec<ModelStatus>> {
            Ok(self
                .0
                .status()
                .await?
                .into_iter()
                .map(|m| ModelStatus { name: m.name, downloaded: m.downloaded, bytes: m.bytes })
                .collect())
        }
        async fn download(&self, name: &str) -> PortResult<EventStream<ModelDownload>> {
            self.0.download(name).await
        }
    }

    /// Lanza el sidecar (`Contents/MacOS/acta-mac` en el bundle) y monta los adaptadores.
    pub async fn start(layout: &DataLayout) -> NativePorts {
        let binary = match locate_sidecar(None) {
            Ok(b) => b,
            Err(e) => return NativePorts::unavailable(e.to_string()),
        };
        let config = SidecarConfig {
            binary,
            log_file: Some(layout.logs_dir().join("acta-mac.log")),
            ..SidecarConfig::default()
        };
        let client = match SidecarClient::start(config).await {
            Ok(c) => c,
            Err(e) => return NativePorts::unavailable(e.to_string()),
        };
        let hello = client.hello_info();
        NativePorts {
            audio: Arc::new(MacAudioSource::new(client.clone())),
            detector: Arc::new(MacMeetingDetector::new(client.clone())),
            speakers: Arc::new(MacSpeakerNames::new(client.clone())),
            screen: Some(Arc::new(MacScreenCapturer::new(client.clone()))),
            clipboard: Some(Arc::new(MacClipboardImporter::new(client.clone()))),
            live: Some(Arc::new(MacLiveTranscriber::new(client.clone()))),
            transcriber: Arc::new(MacTranscriber::new(client.clone())),
            diarizer: Arc::new(MacDiarizer::new(client.clone())),
            permissions: Arc::new(MacPermissions::new(client.clone())),
            models: Arc::new(Models(MacModels::new(client.clone()))),
            devices: Arc::new(Devices(client.clone())),
            meeting_info: Arc::new(Info(client.clone())),
            system: Arc::new(System(client.clone())),
            sidecar_version: hello.as_ref().map(|h| h.sidecar_version.clone()),
            macos_version: hello.map(|h| h.macos_version),
            error: None,
            client: Some(client),
        }
    }
}

#[cfg(target_os = "macos")]
pub use mac::start;

#[cfg(target_os = "windows")]
mod win {
    use super::*;
    use acta_core::layout::DataLayout;
    use acta_mac_bridge::client::{SidecarClient, SidecarConfig};
    use acta_win_bridge::audio::WinAudioSource;
    use acta_win_bridge::locate::locate_sidecar;

    /// Lanza el sidecar de Windows y monta SOLO el audio (M2): el sidecar usa
    /// `hello` + `capture.*` del protocolo v1. Detección, hablantes, pantalla,
    /// transcripción y permisos siguen sin adaptador (M3-M5) y quedan como
    /// "no disponible" con su motivo (nunca en silencio).
    ///
    /// Si el sidecar no está o no arranca, no hay pánico: la app abre con el
    /// componente nativo "no disponible" y el motivo dice qué faltó y dónde se
    /// buscó.
    pub async fn start(layout: &DataLayout) -> NativePorts {
        let binary = match locate_sidecar(None) {
            Ok(b) => b,
            Err(e) => return NativePorts::unavailable(e.to_string()),
        };
        let config = SidecarConfig {
            binary,
            log_file: Some(layout.logs_dir().join("acta-win.log")),
            ..SidecarConfig::default()
        };
        let client = match SidecarClient::start(config).await {
            Ok(c) => c,
            Err(e) => return NativePorts::unavailable(e.to_string()),
        };
        let hello = client.hello_info();
        let sin_adaptador = Arc::new(Unavailable("sin adaptador todavía en Windows (M3-M5)".to_owned()));
        NativePorts {
            audio: Arc::new(WinAudioSource::new(client.clone())),
            detector: sin_adaptador.clone(),
            speakers: sin_adaptador.clone(),
            screen: None,
            clipboard: None,
            live: None,
            transcriber: sin_adaptador.clone(),
            diarizer: sin_adaptador.clone(),
            permissions: sin_adaptador.clone(),
            models: sin_adaptador.clone(),
            meeting_info: sin_adaptador.clone(),
            system: sin_adaptador.clone(),
            devices: sin_adaptador.clone(),
            sidecar_version: hello.as_ref().map(|h| h.sidecar_version.clone()),
            macos_version: hello.map(|h| h.macos_version),
            error: None,
        }
    }
}

#[cfg(target_os = "windows")]
pub use win::start;

/// Sistema sin adaptadores nativos todavía (Linux).
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub async fn start(_layout: &acta_core::layout::DataLayout) -> NativePorts {
    NativePorts::unavailable("plataforma no soportada todavía")
}
