//! Panadapter and software-receiver DSP for hardrock-panel.
//!
//! Pure Rust, no UI. Most of the signal processing — the NCO, the half-band
//! and FIR decimators, the band-pass filters, the demodulators and the AGC — is
//! ported from SDRoxide (<https://github.com/dividebysandwich/sdroxide>,
//! GPL-3.0-or-later); see NOTICE.md and each file's header.
//!
//! * [`Spectrum`] turns interleaved I/Q into calibrated dB frames for the
//!   spectrum, 3D and waterfall views.
//! * [`Receiver`] is one software receiver anywhere inside the span, with its
//!   own offset, mode, passband and AGC, producing 48 kHz mono audio.
//! * [`AudioOut`] plays that audio on a sound card.
//! * [`ZoomSpectrum`] is a second spectrum of only the region on screen, at
//!   full resolution (the zoom FFT).
//! * [`Pipeline`] is the thread that ties an [`IqSource`] to both and publishes
//!   the result for the UI.

pub mod agc;
pub mod audio;
pub mod ddc;
pub mod decim;
pub mod demod;
pub mod design;
pub mod fir;
pub mod nco;
pub mod pipeline;
pub mod receiver;
pub(crate) mod simd;
pub mod source;
pub mod spectrum;
pub mod types;
pub mod window;
pub mod zoom;

pub use audio::{AudioFeed, AudioOut, output_devices};
pub use pipeline::{AudioTap, Command, IqTap, Pipeline, PipelineConfig, Snapshot};
pub use receiver::{AUDIO_RATE, Receiver, SUPPORTED_RATES, decimation_plan};
pub use source::{EladSource, IqSource, SynthSignal, SyntheticSource};
pub use spectrum::{
    Averaging, MAX_FFT_SIZE, MIN_FFT_SIZE, Overlap, Peak, Spectrum, SpectrumConfig, SpectrumFrame,
    Window, interpolate_peak, pool_max, pool_max_range,
};
pub use types::{AgcMode, Complex32, Mode};
pub use zoom::{ZoomRequest, ZoomSpectrum, zoom_factor};
