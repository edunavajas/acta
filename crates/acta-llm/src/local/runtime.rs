//! El `llama-server` (llama.cpp) que ejecuta el modelo local integrado.
//!
//! acta no compila llama.cpp: descarga el binario oficial para macOS arm64 de
//! una versión FIJADA de <https://github.com/ggml-org/llama.cpp/releases>
//! (17 MB, con Metal) y comprueba su SHA-256. Del `.tar.gz` solo se extraen
//! `llama-server`, sus `.dylib` y la licencia. No se firma nada: los binarios
//! oficiales ya vienen firmados ad hoc por el enlazador.
//!
//! Orden de búsqueda: `ACTA_LLAMA_SERVER` → junto al ejecutable de la app →
//! el descargado → Homebrew (`brew install llama.cpp`), que sirve de respaldo
//! si la red de la empresa bloquea GitHub.
//!
//! La extracción es portable (fuera de unix no crea enlaces ni fija el bit de
//! ejecución), pero el binario fijado es de macOS arm64: en otras plataformas
//! `pinned()` sigue apuntando a ese artefacto y el servidor no arrancará. Dar
//! soporte real al runtime integrado en Windows es trabajo aparte.

use super::download::{CancelFlag, DownloadSpec, Progress, download};
use acta_core::llm::local::RuntimeSource;
use acta_core::{PortError, PortResult};
use std::path::{Path, PathBuf};

pub const LLAMA_SERVER_ENV: &str = "ACTA_LLAMA_SERVER";
pub const SERVER_BINARY: &str = "llama-server";

/// Una versión publicada de llama.cpp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeRelease {
    /// Etiqueta de la versión ("b12444"): también es el nombre de su carpeta.
    pub tag: String,
    pub url: String,
    pub sha256: String,
    pub size_bytes: u64,
}

impl RuntimeRelease {
    /// La versión con la que se ha probado acta (16-nov-2026, macOS arm64).
    pub fn pinned() -> Self {
        Self {
            tag: "b12444".into(),
            url: "https://github.com/ggml-org/llama.cpp/releases/download/b12444/llama-b12444-bin-macos-arm64.tar.gz".into(),
            sha256: "e1c00b152caa8c76680a0fbf3029b28b679f904b8fc2d8be9401f74cf41c46e1".into(),
            size_bytes: 17_011_663,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeLocation {
    pub path: PathBuf,
    pub source: RuntimeSource,
}

/// Dónde buscar. Todo inyectable para probarlo sin tocar el sistema.
#[derive(Debug, Clone, Default)]
pub struct RuntimeSearch {
    /// Valor de `ACTA_LLAMA_SERVER`.
    pub env_override: Option<PathBuf>,
    /// Carpeta del ejecutable de la app (`Contents/MacOS`).
    pub exe_dir: Option<PathBuf>,
    /// `<datos>/runtime/llama`.
    pub runtime_dir: PathBuf,
    pub release_tag: String,
    /// Rutas de Homebrew a probar.
    pub homebrew: Vec<PathBuf>,
}

impl RuntimeSearch {
    pub fn system(runtime_dir: PathBuf, release_tag: &str) -> Self {
        Self {
            env_override: std::env::var_os(LLAMA_SERVER_ENV).filter(|v| !v.is_empty()).map(PathBuf::from),
            exe_dir: std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf)),
            runtime_dir,
            release_tag: release_tag.to_owned(),
            homebrew: vec!["/opt/homebrew/bin/llama-server".into(), "/usr/local/bin/llama-server".into()],
        }
    }

    /// Carpeta de la versión descargada.
    pub fn installed_dir(&self) -> PathBuf {
        self.runtime_dir.join(&self.release_tag)
    }

    pub fn locate(&self) -> Option<RuntimeLocation> {
        let found = |path: PathBuf, source| path.is_file().then_some(RuntimeLocation { path, source });
        self.env_override
            .clone()
            .and_then(|p| found(p, RuntimeSource::Env))
            .or_else(|| self.exe_dir.as_ref().and_then(|d| found(d.join(SERVER_BINARY), RuntimeSource::Bundled)))
            .or_else(|| found(self.installed_dir().join(SERVER_BINARY), RuntimeSource::Downloaded))
            .or_else(|| self.homebrew.iter().find_map(|p| found(p.clone(), RuntimeSource::Homebrew)))
    }
}

/// ¿Hace falta este fichero del `.tar.gz`?
fn wanted(name: &str) -> bool {
    name == SERVER_BINARY || name == "LICENSE" || name.ends_with(".dylib")
}

/// Extrae del `.tar.gz` lo necesario en `dest` (plano, sin la carpeta raíz).
/// Los nombres se reducen a su último componente: una entrada nunca escribe
/// fuera de `dest`.
pub fn extract_runtime(tarball: &Path, dest: &Path) -> PortResult<()> {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(dest)?;
    let file = std::fs::File::open(tarball)?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let bad = |e: std::io::Error| PortError::InvalidResponse(format!("el paquete de llama.cpp no se puede leer: {e}"));
    for entry in archive.entries().map_err(bad)? {
        let mut entry = entry.map_err(bad)?;
        let path = entry.path().map_err(bad)?.into_owned();
        let Some(name) = path.file_name().and_then(|n| n.to_str()).map(str::to_owned) else { continue };
        if !wanted(&name) {
            continue;
        }
        let out = dest.join(&name);
        let kind = entry.header().entry_type();
        if kind.is_symlink() {
            // `libllama.dylib -> libllama.1.dylib`: solo enlaces a un vecino.
            // Fuera de unix no se crean: los paquetes de Windows no los traen y
            // crearlos requiere privilegios que la app no pide.
            #[cfg(unix)]
            {
                let Some(target) = entry.link_name().map_err(bad)?.and_then(|t| t.file_name().map(|n| n.to_os_string())) else { continue };
                let _ = std::fs::remove_file(&out);
                std::os::unix::fs::symlink(target, &out)?;
            }
        } else if kind.is_file() {
            let mut f = std::fs::File::create(&out)?;
            std::io::copy(&mut entry, &mut f).map_err(bad)?;
            // En Windows no hay bit de ejecución que fijar.
            #[cfg(unix)]
            {
                let mode = if name == SERVER_BINARY { 0o755 } else { 0o644 };
                f.set_permissions(std::fs::Permissions::from_mode(mode))?;
            }
            f.sync_all()?;
        }
    }
    if !dest.join(SERVER_BINARY).is_file() {
        return Err(PortError::InvalidResponse("el paquete de llama.cpp no trae llama-server".into()));
    }
    Ok(())
}

/// Descarga (reanudable, con SHA-256) e instala la versión en
/// `<runtime_dir>/<tag>/`. La carpeta final aparece de golpe (rename), así
/// que una instalación a medias nunca se confunde con una buena.
pub async fn install_runtime(
    client: &reqwest::Client,
    release: &RuntimeRelease,
    runtime_dir: &Path,
    cancel: &CancelFlag,
    progress: &(dyn Fn(Progress) + Send + Sync),
) -> PortResult<PathBuf> {
    let final_dir = runtime_dir.join(&release.tag);
    let binary = final_dir.join(SERVER_BINARY);
    if binary.is_file() {
        return Ok(binary);
    }
    let tarball = runtime_dir.join(format!("llama-{}.tar.gz", release.tag));
    let spec = DownloadSpec {
        url: release.url.clone(),
        dest: tarball.clone(),
        sha256: release.sha256.clone(),
        size_bytes: (release.size_bytes > 0).then_some(release.size_bytes),
    };
    download(client, &spec, cancel, progress).await?;
    let staging = runtime_dir.join(format!("{}.installing", release.tag));
    let (tar_in, staging_in, final_in) = (tarball.clone(), staging.clone(), final_dir.clone());
    let installed = tokio::task::spawn_blocking(move || -> PortResult<()> {
        let _ = std::fs::remove_dir_all(&staging_in);
        extract_runtime(&tar_in, &staging_in)?;
        let _ = std::fs::remove_dir_all(&final_in);
        std::fs::rename(&staging_in, &final_in)?;
        Ok(())
    })
    .await
    .map_err(|e| PortError::Internal(format!("join: {e}")))?;
    let _ = tokio::fs::remove_file(&tarball).await;
    if let Err(e) = installed {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(e);
    }
    Ok(binary)
}

#[cfg(test)]
pub(crate) mod testing {
    use std::io::Write;
    use std::path::Path;

    /// `.tar.gz` con la forma de los paquetes oficiales: carpeta raíz, el
    /// binario, dylibs, un enlace y herramientas que no hacen falta.
    pub fn fake_tarball(server_script: &str) -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        let mut add = |name: &str, body: &[u8], mode: u32| {
            let mut h = tar::Header::new_gnu();
            h.set_size(body.len() as u64);
            h.set_mode(mode);
            h.set_cksum();
            builder.append_data(&mut h, format!("llama-b1/{name}"), body).expect("append");
        };
        add("llama-server", server_script.as_bytes(), 0o755);
        add("libllama.1.dylib", b"dylib", 0o644);
        add("llama-cli", b"no hace falta", 0o755);
        add("LICENSE", b"MIT", 0o644);
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        h.set_cksum();
        builder.append_link(&mut h, "llama-b1/libllama.dylib", "libllama.1.dylib").expect("link");
        let mut gz = builder.into_inner().expect("tar");
        gz.flush().expect("flush");
        gz.finish().expect("gz")
    }

    /// Tar con una entrada que intenta salirse de la carpeta.
    pub fn evil_tarball() -> Vec<u8> {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast()));
        for (name, body) in [("x/llama-server", &b"#!/bin/sh\n"[..]), ("x/sub/../../evil.dylib", &b"malo"[..])] {
            let mut h = tar::Header::new_gnu();
            h.set_size(body.len() as u64);
            h.set_mode(0o644);
            // `set_path` rechaza los `..`: se escribe el nombre a mano.
            let bytes = name.as_bytes();
            h.as_old_mut().name[..bytes.len()].copy_from_slice(bytes);
            h.set_cksum();
            builder.append(&h, body).expect("append");
        }
        builder.into_inner().expect("tar").finish().expect("gz")
    }

    pub fn touch_executable(path: &Path) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, b"#!/bin/sh\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::*;
    use super::*;
    use crate::local::download::testing::{FileServer, FileServerOptions, sha256_hex};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn search(root: &Path) -> RuntimeSearch {
        RuntimeSearch {
            env_override: None,
            exe_dir: Some(root.join("app")),
            runtime_dir: root.join("runtime"),
            release_tag: "b1".into(),
            homebrew: vec![root.join("brew/llama-server")],
        }
    }

    #[test]
    fn pinned_release_points_at_the_official_macos_arm64_build() {
        let r = RuntimeRelease::pinned();
        assert!(r.url.starts_with("https://github.com/ggml-org/llama.cpp/releases/download/"));
        assert!(r.url.contains(&r.tag) && r.url.ends_with("bin-macos-arm64.tar.gz"));
        assert_eq!(r.sha256.len(), 64);
        assert!(r.size_bytes > 1_000_000);
    }

    #[test]
    fn nothing_installed_means_no_runtime() {
        let d = tempfile::tempdir().expect("tmp");
        assert_eq!(search(d.path()).locate(), None);
    }

    #[test]
    fn search_order_is_env_then_bundled_then_downloaded_then_homebrew() {
        let d = tempfile::tempdir().expect("tmp");
        let mut s = search(d.path());
        let brew = d.path().join("brew/llama-server");
        touch_executable(&brew);
        assert_eq!(s.locate(), Some(RuntimeLocation { path: brew, source: RuntimeSource::Homebrew }));
        let downloaded = d.path().join("runtime/b1/llama-server");
        touch_executable(&downloaded);
        assert_eq!(s.locate().map(|l| l.source), Some(RuntimeSource::Downloaded));
        touch_executable(&d.path().join("app/llama-server"));
        assert_eq!(s.locate().map(|l| l.source), Some(RuntimeSource::Bundled));
        let custom = d.path().join("custom/llama-server");
        touch_executable(&custom);
        s.env_override = Some(custom.clone());
        assert_eq!(s.locate(), Some(RuntimeLocation { path: custom, source: RuntimeSource::Env }));
        // Una ruta de entorno que no existe no tapa a las demás.
        s.env_override = Some(d.path().join("nope"));
        assert_eq!(s.locate().map(|l| l.source), Some(RuntimeSource::Bundled));
    }

    #[test]
    fn another_version_on_disk_is_not_used() {
        let d = tempfile::tempdir().expect("tmp");
        touch_executable(&d.path().join("runtime/b0/llama-server"));
        assert_eq!(search(d.path()).locate(), None);
    }

    #[test]
    fn extracts_only_the_server_its_libraries_and_the_licence() {
        let d = tempfile::tempdir().expect("tmp");
        let tarball = d.path().join("rt.tar.gz");
        std::fs::write(&tarball, fake_tarball("#!/bin/sh\necho hi\n")).expect("write");
        let dest = d.path().join("out");
        extract_runtime(&tarball, &dest).expect("extract");
        let mut names: Vec<String> =
            std::fs::read_dir(&dest).expect("ls").flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        // Fuera de unix no se crean enlaces: falta el nombre sin versión.
        #[cfg(unix)]
        assert_eq!(names, ["LICENSE", "libllama.1.dylib", "libllama.dylib", "llama-server"]);
        #[cfg(not(unix))]
        assert_eq!(names, ["LICENSE", "libllama.1.dylib", "llama-server"]);
        #[cfg(unix)]
        {
            let mode = std::fs::metadata(dest.join("llama-server")).expect("meta").permissions().mode();
            assert_eq!(mode & 0o111, 0o111, "ejecutable");
            assert_eq!(std::fs::read_link(dest.join("libllama.dylib")).expect("link"), Path::new("libllama.1.dylib"));
        }
    }

    #[test]
    fn entries_can_never_escape_the_destination() {
        let d = tempfile::tempdir().expect("tmp");
        let tarball = d.path().join("evil.tar.gz");
        std::fs::write(&tarball, evil_tarball()).expect("write");
        let dest = d.path().join("deep/out");
        extract_runtime(&tarball, &dest).expect("extract");
        assert!(dest.join("evil.dylib").is_file(), "aplanado dentro del destino");
        assert!(!d.path().join("deep/evil.dylib").exists() && !d.path().join("evil.dylib").exists());
    }

    #[test]
    fn a_package_without_the_server_is_rejected() {
        let d = tempfile::tempdir().expect("tmp");
        let tarball = d.path().join("x.tar.gz");
        std::fs::write(&tarball, b"esto no es un tar.gz").expect("write");
        assert!(matches!(extract_runtime(&tarball, &d.path().join("o")), Err(PortError::InvalidResponse(_))));
    }

    #[tokio::test]
    async fn installs_from_a_verified_download_and_cleans_up() {
        let body = fake_tarball("#!/bin/sh\nexit 0\n");
        let srv = FileServer::start(body.clone(), FileServerOptions::default()).await;
        let d = tempfile::tempdir().expect("tmp");
        let release = RuntimeRelease { tag: "b1".into(), url: srv.url.clone(), sha256: sha256_hex(&body), size_bytes: body.len() as u64 };
        let runtime_dir = d.path().join("runtime");
        let client = reqwest::Client::new();
        let bin = install_runtime(&client, &release, &runtime_dir, &CancelFlag::default(), &|_| {}).await.expect("install");
        assert_eq!(bin, runtime_dir.join("b1/llama-server"));
        assert!(bin.is_file());
        assert!(!runtime_dir.join("llama-b1.tar.gz").exists() && !runtime_dir.join("b1.installing").exists());
        // Ya instalado: ni una petición más.
        install_runtime(&client, &release, &runtime_dir, &CancelFlag::default(), &|_| {}).await.expect("again");
        assert_eq!(srv.ranges().len(), 1);
    }

    #[tokio::test]
    async fn a_tampered_package_is_never_installed() {
        let body = fake_tarball("#!/bin/sh\nexit 0\n");
        let srv = FileServer::start(body.clone(), FileServerOptions::default()).await;
        let d = tempfile::tempdir().expect("tmp");
        let release = RuntimeRelease { tag: "b1".into(), url: srv.url.clone(), sha256: "f".repeat(64), size_bytes: body.len() as u64 };
        let err = install_runtime(&reqwest::Client::new(), &release, d.path(), &CancelFlag::default(), &|_| {}).await.expect_err("sha");
        assert!(matches!(err, PortError::InvalidResponse(_)));
        assert!(!d.path().join("b1").exists());
    }
}
