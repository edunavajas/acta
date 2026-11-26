//! Pequeñas llamadas al sistema: espacio libre y URLs de Ajustes del Sistema.

use acta_core::permissions::PermissionKind;
use std::path::Path;

/// Bytes libres para el usuario en el volumen de `path` (`statvfs`).
#[cfg(not(windows))]
#[allow(unsafe_code)]
pub fn disk_free(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st = std::mem::MaybeUninit::<libc::statvfs>::zeroed();
    // SAFETY: `c` es un C string válido terminado en NUL y `st` apunta a
    // memoria del tamaño de `statvfs`; solo se lee si la llamada devuelve 0.
    let rc = unsafe { libc::statvfs(c.as_ptr(), st.as_mut_ptr()) };
    if rc != 0 {
        return None;
    }
    // SAFETY: statvfs devolvió 0, así que la estructura está inicializada.
    let st = unsafe { st.assume_init() };
    #[allow(clippy::unnecessary_cast)]
    Some((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
}

#[cfg(windows)]
/// Bytes libres para el usuario en el volumen de `path` (fs4: `GetDiskFreeSpaceExW`).
pub fn disk_free(path: &Path) -> Option<u64> {
    // statvfs falla si `path` no existe; el `GetVolumePathNameW` de fs4 resuelve
    // el volumen igualmente, así que se replica aquí el mismo contrato.
    if !path.exists() {
        return None;
    }
    fs4::free_space(path).ok()
}

/// Accesibilidad y Grabación de pantalla vistas desde ESTE proceso (acta.app,
/// el cliente TCC que aparece en Ajustes del Sistema). El sidecar las veía
/// «sin decidir» aunque estuvieran concedidas: su caché TCC es del momento en
/// que arrancó, y la Grabación de pantalla solo se reevalúa en un proceso nuevo.
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub fn app_level_permissions() -> AppLevel {
    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn AXIsProcessTrusted() -> u8;
    }
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
    }
    // SAFETY: funciones C sin argumentos que solo consultan TCC.
    unsafe { AppLevel { accessibility: AXIsProcessTrusted() != 0, screen_recording: CGPreflightScreenCaptureAccess() } }
}

#[cfg(not(target_os = "macos"))]
pub fn app_level_permissions() -> AppLevel {
    AppLevel::default()
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AppLevel {
    pub accessibility: bool,
    pub screen_recording: bool,
}

/// Lo que dice la app manda sobre lo que dice el sidecar para Accesibilidad
/// y Pantalla (si la app los tiene, están concedidos).
pub fn merge_app_level(mut states: Vec<acta_core::permissions::PermissionState>, app: AppLevel) -> Vec<acta_core::permissions::PermissionState> {
    use acta_core::permissions::{PermissionState, PermissionStatus};
    for (kind, granted) in [(PermissionKind::Accessibility, app.accessibility), (PermissionKind::ScreenRecording, app.screen_recording)] {
        match states.iter_mut().find(|s| s.kind == kind) {
            Some(s) if granted => s.status = PermissionStatus::Granted,
            Some(_) => {}
            None => states.push(PermissionState { kind, status: if granted { PermissionStatus::Granted } else { PermissionStatus::NotDetermined }, target: None }),
        }
    }
    states
}

/// Permisos que la captura real acaba de rechazar (avisos activos de la
/// grabación en curso): se muestran denegados aunque la consulta diga otra
/// cosa (la consulta de TCC puede estar en caché o ver un permiso viejo).
pub fn apply_runtime_denials(mut states: Vec<acta_core::permissions::PermissionState>, denied: &[PermissionKind]) -> Vec<acta_core::permissions::PermissionState> {
    use acta_core::permissions::{PermissionState, PermissionStatus};
    for &kind in denied {
        match states.iter_mut().find(|s| s.kind == kind) {
            Some(s) => s.status = PermissionStatus::Denied,
            None => states.push(PermissionState { kind, status: PermissionStatus::Denied, target: None }),
        }
    }
    states
}

/// Bytes que ocupa una carpeta (recursivo, sin seguir enlaces). 0 si no existe.
pub fn dir_size_sync(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else { return 0 };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size_sync(&e.path()),
            Ok(t) if t.is_file() => e.metadata().map_or(0, |m| m.len()),
            _ => 0,
        })
        .sum()
}

pub async fn dir_size(path: std::path::PathBuf) -> u64 {
    tokio::task::spawn_blocking(move || dir_size_sync(&path)).await.unwrap_or(0)
}

/// Memoria, chip y disco libre de este Mac, para recomendar un modelo local.
/// `sysctl` se lanza supervisado; si falla, se devuelve un Mac "desconocido"
/// de 8 GB (la opción prudente: solo se recomienda el modelo pequeño).
pub async fn hardware_info(data_root: &Path) -> acta_core::llm::local::HardwareInfo {
    use acta_core::llm::local::{GIB, HardwareInfo, parse_sysctl};
    let free = disk_free(data_root);
    let mut cmd = tokio::process::Command::new("/usr/sbin/sysctl");
    cmd.args(["-n", "hw.memsize", "machdep.cpu.brand_string", "hw.optional.arm64"]);
    let out = acta_llm::process::run_supervised(cmd, None, std::time::Duration::from_secs(5)).await;
    out.ok()
        .and_then(|o| parse_sysctl(&String::from_utf8_lossy(&o.stdout), free))
        .unwrap_or(HardwareInfo { ram_bytes: 8 * GIB, chip: "Mac".into(), apple_silicon: cfg!(target_arch = "aarch64"), free_disk_bytes: free })
}

/// Variable para usar otra carpeta de datos (pruebas, sin tocar la real).
pub const DATA_DIR_ENV: &str = "ACTA_DATA_DIR";

/// Carpeta de datos: `ACTA_DATA_DIR` si está y no es vacía; si no,
/// `<Application Support>/acta`.
pub fn data_root(env: Option<std::ffi::OsString>, support_dir: Option<std::path::PathBuf>) -> Option<std::path::PathBuf> {
    match env.filter(|v| !v.is_empty()) {
        Some(v) => Some(v.into()),
        None => support_dir.map(|d| d.join(acta_core::layout::APP_DIR_NAME)),
    }
}

/// Panel de Privacidad y seguridad de cada permiso.
pub fn settings_url(kind: PermissionKind) -> &'static str {
    match kind {
        PermissionKind::Microphone => "x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone",
        PermissionKind::SystemAudio | PermissionKind::ScreenRecording => {
            "x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"
        }
        PermissionKind::Accessibility => "x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility",
        PermissionKind::Automation => "x-apple.systempreferences:com.apple.preference.security?Privacy_Automation",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_dir_env_override_wins_and_empty_is_ignored() {
        let support = Some(std::path::PathBuf::from("/Users/e/Library/Application Support"));
        assert_eq!(data_root(Some("/tmp/acta-x".into()), support.clone()).unwrap(), Path::new("/tmp/acta-x"));
        assert_eq!(data_root(Some("".into()), support.clone()).unwrap(), Path::new("/Users/e/Library/Application Support/acta"));
        assert_eq!(data_root(None, support).unwrap(), Path::new("/Users/e/Library/Application Support/acta"));
        assert_eq!(data_root(None, None), None);
    }

    #[test]
    fn dir_size_adds_nested_files_and_missing_is_zero() {
        let d = tempfile::tempdir().expect("tmp");
        std::fs::create_dir_all(d.path().join("a/b")).expect("dirs");
        std::fs::write(d.path().join("x"), [0u8; 10]).expect("x");
        std::fs::write(d.path().join("a/b/y"), [0u8; 32]).expect("y");
        assert_eq!(dir_size_sync(d.path()), 42);
        assert_eq!(dir_size_sync(&d.path().join("nope")), 0);
    }

    #[test]
    fn disk_free_of_root_is_known() {
        #[cfg(windows)]
        let (root, miss) = (Path::new(r"C:\"), Path::new(r"C:\definitely\not\here"));
        #[cfg(not(windows))]
        let (root, miss) = (Path::new("/"), Path::new("/definitely/not/here"));
        assert!(disk_free(root).is_some_and(|b| b > 0));
        assert_eq!(disk_free(miss), None);
    }

    #[test]
    fn every_permission_has_a_privacy_pane() {
        for k in [
            PermissionKind::Microphone,
            PermissionKind::SystemAudio,
            PermissionKind::ScreenRecording,
            PermissionKind::Accessibility,
            PermissionKind::Automation,
        ] {
            assert!(settings_url(k).contains("Privacy_"));
        }
    }
}

#[cfg(test)]
mod permission_tests {
    use super::*;
    use acta_core::permissions::{PermissionState, PermissionStatus};

    fn st(kind: PermissionKind, status: PermissionStatus) -> PermissionState {
        PermissionState { kind, status, target: None }
    }

    #[test]
    fn the_app_process_wins_for_accessibility_and_screen_recording() {
        let sidecar = vec![
            st(PermissionKind::Microphone, PermissionStatus::Granted),
            st(PermissionKind::ScreenRecording, PermissionStatus::NotDetermined),
            st(PermissionKind::Accessibility, PermissionStatus::NotDetermined),
        ];
        let out = merge_app_level(sidecar.clone(), AppLevel { accessibility: true, screen_recording: true });
        assert!(out.iter().all(|s| s.status == PermissionStatus::Granted), "{out:?}");
        let out = merge_app_level(sidecar.clone(), AppLevel::default());
        assert_eq!(out, sidecar, "si la app tampoco los tiene, manda el sidecar");
        // Sin sidecar (no arrancó), la app los sigue enseñando.
        let out = merge_app_level(vec![], AppLevel { accessibility: true, screen_recording: false });
        assert_eq!(out, vec![st(PermissionKind::Accessibility, PermissionStatus::Granted), st(PermissionKind::ScreenRecording, PermissionStatus::NotDetermined)]);
    }

    /// Real (19-nov-2026): Ajustes del Sistema enseñaba acta activada en
    /// Grabación de pantalla, pero el permiso era de una versión anterior
    /// (cdhash distinto, app sin firmar reinstalada) y ScreenCaptureKit lo
    /// rechazaba. Lo que pasó de verdad al capturar manda sobre la consulta.
    #[test]
    fn a_permission_refused_while_capturing_wins_over_the_preflight() {
        let states = vec![st(PermissionKind::Microphone, PermissionStatus::Granted), st(PermissionKind::ScreenRecording, PermissionStatus::Granted)];
        let out = apply_runtime_denials(states.clone(), &[PermissionKind::ScreenRecording]);
        assert_eq!(out, vec![st(PermissionKind::Microphone, PermissionStatus::Granted), st(PermissionKind::ScreenRecording, PermissionStatus::Denied)]);
        assert_eq!(apply_runtime_denials(states.clone(), &[]), states);
        let out = apply_runtime_denials(vec![], &[PermissionKind::ScreenRecording]);
        assert_eq!(out, vec![st(PermissionKind::ScreenRecording, PermissionStatus::Denied)]);
    }
}
