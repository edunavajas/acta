//! Filtro de capturas automáticas. El sidecar manda una `slide_change` por
//! cada cambio estable de la ventana y una candidata cada N s, aunque nadie
//! comparta pantalla (vídeo de Teams, pantalla entera en grabación manual):
//! decenas de imágenes sin valor. Aquí solo pasa lo que aporta algo.
//!
//! - Las automáticas (cambio de diapositiva, candidata) exigen que alguien
//!   esté compartiendo.
//! - Y que el hash perceptual difiera lo bastante de la última conservada
//!   (descarta casi duplicadas).
//! - Y, para el cambio de diapositiva, un intervalo mínimo.
//! - Las que pide una persona o una regla (manual, palabra clave, LLM) pasan
//!   siempre.

use acta_core::screen::CaptureReason;
use acta_core::time::UnixMs;

/// Separación mínima entre dos cambios de diapositiva conservados.
pub const MIN_SLIDE_GAP_MS: i64 = 8_000;

#[derive(Debug, Clone)]
pub struct CaptureGate {
    threshold: u32,
    last: Option<(u64, UnixMs)>,
}

impl CaptureGate {
    pub fn new(phash_threshold: u32) -> Self {
        Self { threshold: phash_threshold.max(1), last: None }
    }

    /// Nueva compartición: lo anterior ya no sirve de referencia.
    pub fn reset(&mut self) {
        self.last = None;
    }

    /// ¿Se conserva esta captura?
    pub fn admit(&mut self, reason: CaptureReason, at: UnixMs, phash: &str, sharing: bool) -> bool {
        let hash = u64::from_str_radix(phash, 16).ok();
        let automatic = matches!(reason, CaptureReason::SlideChange | CaptureReason::Candidate);
        if automatic {
            if !sharing {
                return false;
            }
            if let (Some((last_hash, last_at)), Some(h)) = (self.last, hash) {
                if (last_hash ^ h).count_ones() < self.threshold {
                    return false;
                }
                if reason == CaptureReason::SlideChange && at.0 - last_at.0 < MIN_SLIDE_GAP_MS {
                    return false;
                }
            }
        }
        if let Some(h) = hash {
            self.last = Some((h, at));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use CaptureReason::*;

    const A: &str = "0000000000000000";
    const FAR: &str = "ffffffffffffffff";
    const NEAR_A: &str = "0000000000000003";

    #[test]
    fn automatic_captures_need_someone_sharing() {
        let mut g = CaptureGate::new(10);
        assert!(!g.admit(SlideChange, UnixMs(1_000), A, false));
        assert!(!g.admit(Candidate, UnixMs(2_000), FAR, false));
        assert!(g.admit(SlideChange, UnixMs(3_000), A, true));
    }

    #[test]
    fn manual_keyword_and_llm_always_pass() {
        let mut g = CaptureGate::new(10);
        for r in [Manual, Keyword, LlmPick] {
            assert!(g.admit(r, UnixMs(1_000), A, false));
        }
    }

    #[test]
    fn near_duplicates_of_the_last_kept_are_dropped() {
        let mut g = CaptureGate::new(10);
        assert!(g.admit(SlideChange, UnixMs(0), A, true));
        assert!(!g.admit(SlideChange, UnixMs(60_000), NEAR_A, true));
        assert!(!g.admit(Candidate, UnixMs(61_000), A, true));
        assert!(g.admit(SlideChange, UnixMs(62_000), FAR, true));
    }

    #[test]
    fn slides_closer_than_the_minimum_gap_are_dropped() {
        let mut g = CaptureGate::new(10);
        assert!(g.admit(SlideChange, UnixMs(0), A, true));
        assert!(!g.admit(SlideChange, UnixMs(MIN_SLIDE_GAP_MS - 1), FAR, true));
        assert!(g.admit(SlideChange, UnixMs(MIN_SLIDE_GAP_MS), FAR, true));
    }

    #[test]
    fn a_manual_capture_counts_as_reference_and_reset_forgets_it() {
        let mut g = CaptureGate::new(10);
        assert!(g.admit(Manual, UnixMs(0), A, true));
        assert!(!g.admit(SlideChange, UnixMs(20_000), A, true));
        g.reset();
        assert!(g.admit(SlideChange, UnixMs(21_000), A, true));
    }

    #[test]
    fn an_unreadable_hash_is_kept() {
        let mut g = CaptureGate::new(10);
        assert!(g.admit(SlideChange, UnixMs(0), "zz", true));
        assert!(g.admit(SlideChange, UnixMs(1), "zz", true));
    }
}
