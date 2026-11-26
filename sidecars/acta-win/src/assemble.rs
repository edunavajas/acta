//! Ensamblado de frames de 100 ms: junta muestras mono de 16 kHz en frames
//! fijos con `seq` monotónico por pista y `pts` en la línea temporal del
//! audio emitido (µs desde el primer sample tras `capture.start`), que es lo
//! que el writer de Rust usa para alinear pistas (docs §5.2). Espejo de
//! `FrameAssembler` (AudioMath.swift) sin el re-anclaje por deriva: aquí el
//! `pts` sale del mismo contador que llena los frames, así que no puede
//! derivar del reloj de pared.

use base64::Engine as _;

pub const OUTPUT_SAMPLE_RATE: u32 = 16_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PcmFrame {
    pub seq: u64,
    pub pts_us: u64,
    pub samples: Vec<i16>,
}

impl PcmFrame {
    /// PCM s16le en base64 estándar con padding (contrato `audio.frame`).
    pub fn base64(&self) -> String {
        let mut bytes = Vec::with_capacity(self.samples.len() * 2);
        for sample in &self.samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }
}

#[derive(Debug)]
pub struct FrameAssembler {
    frame_samples: usize,
    seq: u64,
    /// `pts` del primer sample pendiente (y de lo que salga por `flush`).
    pts_us: u64,
    pending: Vec<i16>,
}

impl FrameAssembler {
    pub fn new(sample_rate: u32, frame_ms: u32) -> Self {
        Self {
            frame_samples: (sample_rate.saturating_mul(frame_ms) as usize / 1_000).max(1),
            seq: 0,
            pts_us: 0,
            pending: Vec::new(),
        }
    }

    /// Encola muestras y devuelve los frames completos que se cierran.
    pub fn push(&mut self, samples: &[i16]) -> Vec<PcmFrame> {
        self.pending.extend_from_slice(samples);
        let mut frames = Vec::new();
        while self.pending.len() >= self.frame_samples {
            frames.push(self.take_frame(self.frame_samples));
        }
        frames
    }

    /// Cierra el frame parcial (si lo hay) para la parada, como `flushPartial`:
    /// sale corto y consume su hueco en la línea temporal.
    pub fn flush(&mut self) -> Option<PcmFrame> {
        (!self.pending.is_empty()).then(|| self.take_frame(self.pending.len()))
    }

    /// Olvida lo pendiente sin emitir (pausa). La línea temporal NO avanza:
    /// `pts` solo cuenta audio emitido, así que lo tirado no crea hueco.
    pub fn discard(&mut self) {
        self.pending.clear();
    }

    fn take_frame(&mut self, count: usize) -> PcmFrame {
        let frame = PcmFrame {
            seq: self.seq,
            pts_us: self.pts_us,
            samples: self.pending.drain(..count).collect(),
        };
        // El pts avanza por lo EMITIDO (no por el tamaño nominal del frame):
        // un frame corto del `flush` consume solo su parte y el siguiente
        // push re-ancla justo después.
        self.pts_us += u64::from(count as u32) * 1_000_000 / u64::from(OUTPUT_SAMPLE_RATE);
        self.seq += 1;
        frame
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const FRAME_SAMPLES: usize = 1_600; // 100 ms a 16 kHz

    fn samples(count: usize, first: i16) -> Vec<i16> {
        (0..count).map(|i| first + i as i16).collect()
    }

    fn s16_bytes(frame: &PcmFrame) -> Vec<u8> {
        frame.samples.iter().flat_map(|s| s.to_le_bytes()).collect()
    }

    #[test]
    fn full_frame_exactly_100_ms() {
        let mut assembler = FrameAssembler::new(16_000, 100);
        let frames = assembler.push(&samples(FRAME_SAMPLES, 7));
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].seq, 0);
        assert_eq!(frames[0].pts_us, 0);
        assert_eq!(frames[0].samples.len(), FRAME_SAMPLES);
        assert_eq!(frames[0].samples[0], 7);
        assert_eq!(frames[0].samples[FRAME_SAMPLES - 1], 7 + FRAME_SAMPLES as i16 - 1);
    }

    #[test]
    fn chunks_smaller_than_a_frame_accumulate() {
        let mut assembler = FrameAssembler::new(16_000, 100);
        assert!(assembler.push(&samples(160, 0)).is_empty());
        assert!(assembler.push(&samples(160, 0)).is_empty());
        assert!(assembler.push(&samples(479, 0)).is_empty());
        // 160 + 160 + 479 + 801 = 1600: aquí se cierra el primer frame.
        let frames = assembler.push(&samples(801, 0));
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].samples.len(), FRAME_SAMPLES);
        assert!(assembler.push(&samples(500, 0)).is_empty());
    }

    #[test]
    fn frames_keep_points_and_seq_monotonic_across_pushes() {
        let mut assembler = FrameAssembler::new(16_000, 100);
        let first = assembler.push(&samples(FRAME_SAMPLES, 0));
        let second = assembler.push(&samples(FRAME_SAMPLES, 0));
        assert_eq!(first[0].seq, 0);
        assert_eq!(second[0].seq, 1);
        assert_eq!(second[0].pts_us, 100_000);
    }

    #[test]
    fn a_frame_spanning_two_pushes_starts_at_the_right_point() {
        let mut assembler = FrameAssembler::new(16_000, 100);
        assert!(assembler.push(&samples(1_000, 0)).is_empty()); // frame parcial
        let frames = assembler.push(&samples(600, 0)); // cierra el frame 0
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].seq, 0);
        assert_eq!(frames[0].pts_us, 0);
        assert_eq!(frames[0].samples.len(), FRAME_SAMPLES);
        // Y el sobre restante no abre ningún frame nuevo.
        assert!(assembler.push(&samples(0, 0)).is_empty());
    }

    #[test]
    fn discard_on_pause_keeps_the_audio_timeline_going() {
        let mut assembler = FrameAssembler::new(16_000, 100);
        let first = assembler.push(&samples(FRAME_SAMPLES, 0));
        assert_eq!(first.len(), 1);
        assert!(assembler.push(&samples(300, 0)).is_empty());
        assembler.discard();
        // Tras la pausa, la línea temporal sigue donde la dejó el audio
        // emitido: frame completo siguiente con pts 100_000 y seq 1.
        let after = assembler.push(&samples(FRAME_SAMPLES, 0));
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].seq, 1);
        assert_eq!(after[0].pts_us, 100_000);
    }

    #[test]
    fn flush_on_stop_emits_the_partial_frame_short() {
        let mut assembler = FrameAssembler::new(16_000, 100);
        // 1600 + 120: un frame completo sale ya por push, el resto queda el parcial.
        assert_eq!(assembler.push(&samples(FRAME_SAMPLES + 120, 0)).len(), 1);
        let partial = assembler.flush().expect("frame parcial");
        assert_eq!(partial.seq, 1);
        assert_eq!(partial.pts_us, 100_000);
        assert_eq!(partial.samples.len(), 120);
        // Y flush sin pendientes devuelve None.
        assert!(assembler.flush().is_none());
    }

    #[test]
    fn after_a_flush_a_new_push_reanchors() {
        let mut assembler = FrameAssembler::new(16_000, 100);
        assert!(assembler.push(&samples(200, 0)).is_empty());
        let partial = assembler.flush().expect("parcial");
        assert_eq!(partial.seq, 0);
        assert_eq!(partial.pts_us, 0);
        let next = assembler.push(&samples(FRAME_SAMPLES, 0));
        // El frame siguiente arranca después del parcial emitido.
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].seq, 1);
        // 200 muestras emitidas como parcial + frame completo.
        assert_eq!(next[0].pts_us, 12_500);
    }

    #[test]
    fn base64_decodes_to_s16le_bytes() {
        let frame = PcmFrame { seq: 0, pts_us: 0, samples: vec![1, -1, 32_767, -32_768] };
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(frame.base64())
            .expect("base64");
        assert_eq!(decoded, s16_bytes(&frame));
        // Un frame completo de 100 ms son 3.200 bytes (1600 muestras × 2).
        let mut assembler = FrameAssembler::new(16_000, 100);
        let full = assembler.push(&samples(FRAME_SAMPLES, 0));
        assert_eq!(s16_bytes(&full[0]).len(), 3_200);
        assert_eq!(full[0].base64().len(), 3_200usize.div_ceil(3) * 4);
    }

    #[test]
    fn frame_smaller_than_100_ms_is_supported_like_the_contract_allows() {
        // El contrato admite frame_ms ≤ 1000 (macOS: "bad frame_ms" solo en 0
        // o > 1000); el cliente real siempre usa 100.
        let mut assembler = FrameAssembler::new(16_000, 20);
        let frames = assembler.push(&samples(320, 0));
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].samples.len(), 320);
        assert_eq!(frames[0].pts_us, 0);
    }
}
