//! Sidecar de audio de Windows (acta-win): protocolo JSON por líneas por
//! stdin/stdout, el mismo transporte que acta-mac (docs/01-arquitectura.md §4).
//! - stdin  (Rust → sidecar): peticiones `{"id":..,"cmd":..,"args":{..}}`
//! - stdout (sidecar → Rust): respuestas y eventos `audio.frame`, una por línea
//! - stderr: logs en texto libre (nunca protocolo)
//! - EOF en stdin: para la captura y sale con código 0

mod assemble;
mod capture;
mod protocol;
mod session;

use std::io::{BufRead, Write};
use std::process::ExitCode;
use std::sync::mpsc::{Receiver, sync_channel};

use crate::protocol::Outgoing;

/// Bucle de escritura: una línea por mensaje, con flush por línea para que el
/// cliente nunca espere. Muere (silenciosamente) si stdout se cierra.
fn write_loop(rx: Receiver<Outgoing>) {
    let stdout = std::io::stdout();
    let mut lock = stdout.lock();
    for outgoing in rx {
        match outgoing.to_line() {
            Ok(line) => {
                if writeln!(lock, "{line}").is_err() {
                    break;
                }
                if lock.flush().is_err() {
                    break;
                }
            }
            Err(error) => eprintln!("acta-win: línea no serializable: {error}"),
        }
    }
}

fn main() -> ExitCode {
    let (tx, rx) = sync_channel(session::CHANNEL_CAPACITY);
    let writer = std::thread::Builder::new()
        .name("acta-win-stdout".into())
        .spawn(|| write_loop(rx))
        .map_err(|error| eprintln!("acta-win: spawn del escritor: {error}"));

    let mut session = session::Session::new(tx, capture::WinLauncher);
    let mut outcome = session::Outcome::Continue;

    // Lector de stdin: una petición por línea. Las líneas mayores que el tope
    // del contrato se descartan SIN responder (no son fatals).
    let mut aborted = false;
    for line in std::io::stdin().lock().split(b'\n') {
        match line {
            Ok(bytes) => {
                if bytes.len() > protocol::MAX_LINE_BYTES {
                    eprintln!("acta-win: línea de {} bytes supera el tope, descartada", bytes.len());
                    continue;
                }
                // \r de CRLF y líneas vacías: se toleran silenciosamente.
                let text = String::from_utf8_lossy(&bytes);
                let text = text.trim_end_matches('\r');
                if text.trim().is_empty() {
                    continue;
                }
                outcome = session.handle(text);
                if outcome != session::Outcome::Continue {
                    break;
                }
            }
            Err(error) => {
                eprintln!("acta-win: error leyendo stdin: {error}");
                aborted = true;
                break;
            }
        }
    }

    // EOF en stdin o `shutdown`: parar la captura (join de pistas, y los
    // `stopped` que correspondan) antes de morir. Tras un `shutdown` es no-op.
    session.on_exit();
    drop(session);
    if let Ok(writer) = writer {
        let _ = writer.join();
    }
    let code = if aborted {
        1
    } else {
        outcome.code().unwrap_or(0)
    };
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}
