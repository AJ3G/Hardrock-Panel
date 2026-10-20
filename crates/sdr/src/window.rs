// Ported from SDRoxide (https://github.com/dividebysandwich/sdroxide),
// crates/sdroxide-dsp/src/window.rs, GPL-3.0-or-later. See NOTICE.md.

use std::f32::consts::TAU;

/// Hann window: a 2-bin main lobe with -31 dB first sidelobes.
///
/// Chosen over [`blackman_harris`] wherever two close tones have to be told
/// apart rather than one weak tone dug out of leakage — Blackman-Harris buys its
/// -92 dB sidelobes with a main lobe twice as wide, which is the wrong trade
/// when the pair in question is 67.0 and 69.3 Hz.
pub fn hann(n: usize) -> Vec<f32> {
    (0..n).map(|i| 0.5 - 0.5 * (TAU * i as f32 / (n as f32 - 1.0)).cos()).collect()
}

/// 4-term Blackman-Harris window (-92 dB sidelobes).
pub fn blackman_harris(n: usize) -> Vec<f32> {
    const A: [f32; 4] = [0.35875, 0.48829, 0.14128, 0.01168];
    (0..n)
        .map(|i| {
            let x = TAU * i as f32 / (n as f32 - 1.0);
            A[0] - A[1] * x.cos() + A[2] * (2.0 * x).cos() - A[3] * (3.0 * x).cos()
        })
        .collect()
}

/// Rectangular (no) window: the narrowest main lobe and the most leakage
/// (-13 dB sidelobes). For the finest resolution of clean, equal signals.
pub fn rectangular(n: usize) -> Vec<f32> {
    vec![1.0; n]
}

/// 5-term flat-top window (the HFT/SRS coefficients): a main lobe five bins
/// wide but under 0.01 dB of scalloping, so a carrier reads its true level
/// wherever it falls between bins. For measuring, not for resolving.
pub fn flat_top(n: usize) -> Vec<f32> {
    const A: [f64; 5] = [0.21557895, 0.41663158, 0.277263158, 0.083578947, 0.006947368];
    (0..n)
        .map(|i| {
            let x = std::f64::consts::TAU * i as f64 / (n as f64 - 1.0);
            (A[0] - A[1] * x.cos() + A[2] * (2.0 * x).cos() - A[3] * (3.0 * x).cos()
                + A[4] * (4.0 * x).cos()) as f32
        })
        .collect()
}

/// A window's equivalent noise bandwidth in bins, `N·Σw² / (Σw)²`: how many
/// bins' worth of noise one bin collects. Hann 1.5, Blackman-Harris 2.0,
/// flat-top 3.8, rectangular 1.
pub fn enbw_bins(w: &[f32]) -> f32 {
    let s: f64 = w.iter().map(|&x| x as f64).sum();
    let s2: f64 = w.iter().map(|&x| (x as f64) * (x as f64)).sum();
    if s == 0.0 { 1.0 } else { (w.len() as f64 * s2 / (s * s)) as f32 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noise_bandwidths_are_the_textbook_ones() {
        let n = 4096;
        assert!((enbw_bins(&rectangular(n)) - 1.0).abs() < 1e-4);
        assert!((enbw_bins(&hann(n)) - 1.5).abs() < 0.01);
        assert!((enbw_bins(&blackman_harris(n)) - 2.0).abs() < 0.01);
        assert!((enbw_bins(&flat_top(n)) - 3.77).abs() < 0.03);
    }
}
