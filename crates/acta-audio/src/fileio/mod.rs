//! Utilidades de fichero compartidas: escritura atómica, fsync de directorio,
//! cabecera WAV y lectura de segmentos (`.wav` o `.raw` de emergencia).
//! Nada aquí lanza procesos.

use acta_core::{PortError, PortResult};
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// Cabecera RIFF/WAVE de hound para PCM s16 mono: siempre 44 bytes.
pub const WAV_HEADER_LEN: u64 = 44;

/// Sincroniza el directorio para que el rename sobreviva a un corte.
///
/// En Windows no existe una forma portable de sincronizar un directorio
/// (`File::open` sobre una carpeta devuelve `ERROR_ACCESS_DENIED`), y el
/// renombrado ya es atómico: el fichero se sincroniza antes de moverlo.
#[cfg(windows)]
pub fn fsync_dir(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(not(windows))]
pub fn fsync_dir(dir: &Path) -> std::io::Result<()> {
    File::open(dir)?.sync_all()
}

/// `path.tmp` (se trunca: es nuestro) → fsync → rename → fsync del directorio.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    {
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    if let Some(parent) = path.parent() {
        fsync_dir(parent)?;
    }
    Ok(())
}

/// Cabecera WAV PCM s16 mono para `samples` muestras.
pub fn wav_header(samples: u64, sample_rate: u32) -> [u8; 44] {
    let data_len = (samples * 2).min(u64::from(u32::MAX - 36)) as u32;
    let mut h = [0u8; 44];
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data_len).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes());
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&1u16.to_le_bytes()); // mono
    h[24..28].copy_from_slice(&sample_rate.to_le_bytes());
    h[28..32].copy_from_slice(&(sample_rate * 2).to_le_bytes());
    h[32..34].copy_from_slice(&2u16.to_le_bytes());
    h[34..36].copy_from_slice(&16u16.to_le_bytes());
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_len.to_le_bytes());
    h
}

pub fn le_bytes_to_samples(bytes: &[u8]) -> Vec<i16> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| i16::from_le_bytes(*c))
        .collect()
}

pub fn samples_to_le_bytes(samples: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

/// Lee un segmento finalizado: `.wav` (hound) o `.raw` (PCM s16le de emergencia).
pub fn read_segment_samples(path: &Path) -> PortResult<Vec<i16>> {
    if path.extension().and_then(|e| e.to_str()) == Some("raw") {
        let mut bytes = Vec::new();
        File::open(path)?.read_to_end(&mut bytes)?;
        return Ok(le_bytes_to_samples(&bytes));
    }
    let mut reader = hound::WavReader::open(path)
        .map_err(|e| PortError::Io(format!("{}: {e}", path.display())))?;
    reader
        .samples::<i16>()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| PortError::Io(format!("{}: {e}", path.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_matches_hound_layout() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.wav");
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut w = hound::WavWriter::create(&p, spec).unwrap();
        for i in 0..100i16 {
            w.write_sample(i).unwrap();
        }
        w.finalize().unwrap();
        let bytes = std::fs::read(&p).unwrap();
        assert_eq!(bytes.len() as u64, WAV_HEADER_LEN + 200);
        assert_eq!(&bytes[..44], &wav_header(100, 16_000));
    }

    #[test]
    fn write_atomic_replaces_and_leaves_no_tmp() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("m.json");
        write_atomic(&p, b"one").unwrap();
        write_atomic(&p, b"two").unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"two");
        assert!(!dir.path().join("m.json.tmp").exists());
    }
}
