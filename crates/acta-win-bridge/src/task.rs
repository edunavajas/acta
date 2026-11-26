//! Copia de `crates/acta-mac-bridge/src/task.rs` (módulo privado allí): una
//! tarea de reenvío por stream, que se aborta al reemplazarla, al parar o al
//! soltar el adaptador. Extraerlo a un crate compartido es T13.

use acta_core::EventStream;
use futures::StreamExt;
use std::future::Future;
use std::sync::Mutex;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_stream::wrappers::ReceiverStream;

/// Capacidad del stream (≈20 frames/s de audio, margen para el consumidor).
const STREAM_CAPACITY: usize = 4096;

/// Lanza `f(tx)` y devuelve el stream de lo que envíe. El stream termina
/// cuando la tarea acaba o se aborta (se suelta `tx`).
pub(crate) fn spawn_stream<T, F, Fut>(f: F) -> (EventStream<T>, JoinHandle<()>)
where
    T: Send + 'static,
    F: FnOnce(mpsc::Sender<T>) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let (tx, rx) = mpsc::channel(STREAM_CAPACITY);
    let handle = tokio::spawn(f(tx));
    (ReceiverStream::new(rx).boxed(), handle)
}

/// Hueco para la tarea activa de un adaptador.
#[derive(Default)]
pub(crate) struct Slot(Mutex<Option<JoinHandle<()>>>);

impl Slot {
    pub(crate) fn set(&self, handle: JoinHandle<()>) {
        if let Some(old) = self.lock().replace(handle) {
            old.abort();
        }
    }

    pub(crate) fn take(&self) -> Option<JoinHandle<()>> {
        self.lock().take()
    }

    pub(crate) fn cancel(&self) {
        if let Some(old) = self.lock().take() {
            old.abort();
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<JoinHandle<()>>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.cancel();
    }
}
