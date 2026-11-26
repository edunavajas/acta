//! # acta-win-bridge (M2)
//!
//! Adaptador Windows del puerto `AudioSource` sobre el sidecar `acta-win`.
//!
//! El sidecar habla el protocolo v1 (`hello`, `capture.*`, `shutdown`) y emite
//! `audio.frame` (PCM s16le 100 ms en base64), `audio.track_state` y
//! `audio.overrun`. Aquí no se lanza ningún proceso: el lanzamiento y la
//! supervisión (`tokio::process` + `kill_on_drop` + `wait`) los hace
//! `acta_mac_bridge::client::SidecarClient`.
//!
//! | puerto (acta-core) | adaptador |
//! |---|---|
//! | `AudioSource` | `audio::WinAudioSource` |
//!
//! Detector, hablantes, pantalla, transcripción y permisos siguen sin
//! adaptador en Windows (M3-M5); en la app quedan como "no disponible".
#![deny(clippy::zombie_processes)]

pub mod audio;
pub mod locate;
mod task;
