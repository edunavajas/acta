//! Dónde está el binario `acta-win`.
//!
//! Orden (documentada por si cambia el empaquetado, M6):
//! 1. Variable de entorno `ACTA_WIN_SIDECAR`, para una ruta explícita.
//! 2. Junto al ejecutable de la app: `acta-win.exe` (y `acta-win-<triple>.exe`
//!    Tauri `externalBin` le añade el triple de destino al empaquetar).
//! 3. Desarrollo: `sidecars/acta-win/target/{debug,release}/acta-win.exe` del
//!    repo (`CARGO_MANIFEST_DIR` de este crate).
//!
//! Si no aparece en ninguno, el error dice qué se buscó y dónde: el lado de la
//! app lo convierte en el motivo de "no disponible" (nada de pánicos).

use acta_core::{PortError, PortResult};
use std::path::{Path, PathBuf};

pub const SIDECAR_ENV: &str = "ACTA_WIN_SIDECAR";
const NAMES: [&str; 2] = ["acta-win.exe", "acta-win-x86_64-pc-windows-msvc.exe"];

/// Resuelve la ruta del sidecar con el sistema de ficheros real.
pub fn locate_sidecar(explicit: Option<&Path>) -> PortResult<PathBuf> {
    let env = std::env::var_os(SIDECAR_ENV).map(PathBuf::from);
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sidecars/acta-win/target");
    let candidates = candidates(explicit, env.as_deref(), exe_dir.as_deref(), &repo);
    candidates
        .iter()
        .find(|p| p.is_file())
        .cloned()
        .ok_or_else(|| PortError::NotFound(format!("sidecar acta-win no encontrado; se buscó en {candidates:?}")))
}

/// Lista ordenada de candidatos (lógica pura, testeable).
pub(crate) fn candidates(
    explicit: Option<&Path>,
    env: Option<&Path>,
    exe_dir: Option<&Path>,
    repo_target: &Path,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    out.extend(explicit.map(Path::to_path_buf));
    out.extend(env.map(Path::to_path_buf));
    if let Some(dir) = exe_dir {
        out.extend(NAMES.iter().map(|n| dir.join(n)));
    }
    for profile in ["debug", "release"] {
        out.push(repo_target.join(profile).join("acta-win.exe"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Orden completo: explícita, variable de entorno, junto al ejecutable y
    /// los perfiles de desarrollo. Se compara por `Path`, no por cadenas.
    #[test]
    fn env_then_exe_dir_then_repo() {
        let repo_target = Path::new("/repo").join("sidecars/acta-win/target");
        let c = candidates(
            None,
            Some(Path::new("/env/acta-win.exe")),
            Some(Path::new("C:/app")),
            &repo_target,
        );
        let expected = vec![
            PathBuf::from("/env/acta-win.exe"),
            Path::new("C:/app").join("acta-win.exe"),
            Path::new("C:/app").join("acta-win-x86_64-pc-windows-msvc.exe"),
            repo_target.join("debug").join("acta-win.exe"),
            repo_target.join("release").join("acta-win.exe"),
        ];
        assert_eq!(c, expected);
    }

    #[test]
    fn explicit_goes_first() {
        let c = candidates(
            Some(Path::new("/x/acta-win.exe")),
            None,
            None,
            &PathBuf::from("/repo"),
        );
        assert_eq!(c, vec![PathBuf::from("/x/acta-win.exe"), PathBuf::from("/repo/debug/acta-win.exe"), PathBuf::from("/repo/release/acta-win.exe")]);
    }

    #[test]
    fn missing_everywhere_is_not_found() {
        let e = locate_sidecar(Some(Path::new("/definitely/not/here.exe"))).err();
        // Puede existir un build de desarrollo del repo; si no, NotFound.
        if let Some(e) = e {
            assert!(matches!(e, PortError::NotFound(_)));
        }
    }
}
