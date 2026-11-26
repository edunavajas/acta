//! Dónde está el binario `acta-mac`.
//!
//! Orden:
//! 1. `explicit` (Ajustes / WP6) o la variable `ACTA_MAC_SIDECAR`.
//! 2. Junto al ejecutable actual: `Contents/MacOS/acta-mac` en el bundle
//!    (Tauri `externalBin` quita el sufijo del triple al empaquetar) y
//!    `acta-mac-aarch64-apple-darwin` en `tauri dev`.
//! 3. Desarrollo: `sidecars/acta-mac/.build/{debug,release}/acta-mac` del repo.

use acta_core::{PortError, PortResult};
use std::path::{Path, PathBuf};

pub const SIDECAR_ENV: &str = "ACTA_MAC_SIDECAR";
const NAMES: [&str; 2] = ["acta-mac", "acta-mac-aarch64-apple-darwin"];

/// Resuelve la ruta del sidecar con el sistema de ficheros real.
pub fn locate_sidecar(explicit: Option<&Path>) -> PortResult<PathBuf> {
    let env = std::env::var_os(SIDECAR_ENV).map(PathBuf::from);
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sidecars/acta-mac/.build");
    let candidates = candidates(explicit, env.as_deref(), exe_dir.as_deref(), &repo);
    candidates
        .iter()
        .find(|p| p.is_file())
        .cloned()
        .ok_or_else(|| PortError::NotFound(format!("acta-mac sidecar not found; tried {candidates:?}")))
}

/// Lista ordenada de candidatos (lógica pura, testeable).
pub(crate) fn candidates(
    explicit: Option<&Path>,
    env: Option<&Path>,
    exe_dir: Option<&Path>,
    repo_build: &Path,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    out.extend(explicit.map(Path::to_path_buf));
    out.extend(env.map(Path::to_path_buf));
    if let Some(dir) = exe_dir {
        out.extend(NAMES.iter().map(|n| dir.join(n)));
    }
    for profile in ["debug", "release"] {
        out.push(repo_build.join(profile).join("acta-mac"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg_attr(not(target_os = "macos"), ignore = "compara rutas con separador POSIX")]
    fn explicit_then_env_then_exe_dir_then_repo() {
        let c = candidates(
            Some(Path::new("/x/acta-mac")),
            Some(Path::new("/env/acta-mac")),
            Some(Path::new("/App.app/Contents/MacOS")),
            Path::new("/repo/.build"),
        );
        let s: Vec<String> = c.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(
            s,
            vec![
                "/x/acta-mac",
                "/env/acta-mac",
                "/App.app/Contents/MacOS/acta-mac",
                "/App.app/Contents/MacOS/acta-mac-aarch64-apple-darwin",
                "/repo/.build/debug/acta-mac",
                "/repo/.build/release/acta-mac",
            ]
        );
    }

    #[test]
    fn missing_everywhere_is_not_found() {
        let e = locate_sidecar(Some(Path::new("/definitely/not/here"))).err();
        // Puede existir un build de desarrollo del repo; si no, NotFound.
        if let Some(e) = e {
            assert!(matches!(e, PortError::NotFound(_)));
        }
    }
}
