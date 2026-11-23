//! Una sola acta por carpeta de datos. Sin guardia, un segundo proceso (la
//! app instalada y una de desarrollo, o un relanzamiento tras actualizar)
//! crea SU icono en la barra de menús: dos iconos de acta a la vez.

use std::fs::File;
use std::os::fd::AsRawFd;
use std::path::Path;

/// Candado exclusivo sobre `acta.lock`; el sistema lo libera al morir el proceso.
#[derive(Debug)]
pub struct InstanceLock {
    _file: File,
}

impl InstanceLock {
    /// `None` si otra acta ya tiene el candado.
    pub fn acquire(root: &Path) -> Option<Self> {
        let file = File::options().create(true).truncate(false).write(true).open(root.join("acta.lock")).ok()?;
        // SAFETY: `flock` sobre un descriptor abierto que vive tanto como `file`.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        (rc == 0).then_some(Self { _file: file })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_instance_cannot_take_the_lock_until_the_first_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let first = InstanceLock::acquire(dir.path());
        assert!(first.is_some());
        assert!(InstanceLock::acquire(dir.path()).is_none(), "segunda instancia");
        drop(first);
        assert!(InstanceLock::acquire(dir.path()).is_some());
    }
}
