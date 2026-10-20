//! What the scope keeps from one run to the next.
//!
//! [`ScopeSettings`] is plain serde data, saved by the panel alongside its own
//! settings (`Saved` in main.rs) with eframe's storage. Every field has a
//! default, so a settings file from before a field existed still loads.

use serde::{Deserialize, Serialize};

use super::palette::Palette;

/// What the upper part of the scope shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TopView {
    /// The flat spectrum trace.
    #[default]
    Spectrum,
    /// SDRoxide's 3D surface: the newest spectrum in front, older ones
    /// receding behind it.
    Surface,
    /// Both: the 3D surface above the flat trace, on one frequency axis.
    Both,
    /// The spectral correlation function's (f, α) plane ([`super::scf`]).
    Scf,
}

impl TopView {
    pub const ALL: [TopView; 4] =
        [TopView::Spectrum, TopView::Surface, TopView::Both, TopView::Scf];

    pub fn label(self) -> &'static str {
        match self {
            TopView::Spectrum => "Spectrum",
            TopView::Surface => "3D FFT",
            TopView::Both => "Both",
            TopView::Scf => "SCF",
        }
    }

    pub fn has_surface(self) -> bool {
        !matches!(self, TopView::Spectrum | TopView::Scf)
    }

    pub fn has_trace(self) -> bool {
        self != TopView::Surface
    }
}

/// How levels are labelled: dBm, S-units (S9 = −73 dBm, 6 dB a unit), or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Scale {
    Dbm,
    SUnits,
    #[default]
    Both,
}

impl Scale {
    pub const ALL: [Scale; 3] = [Scale::Dbm, Scale::SUnits, Scale::Both];

    pub fn label(self) -> &'static str {
        match self {
            Scale::Dbm => "dBm",
            Scale::SUnits => "S-units",
            Scale::Both => "Both",
        }
    }

    /// The short form on the SCALE soft key.
    pub fn key_label(self) -> &'static str {
        match self {
            Scale::Dbm => "dBm",
            Scale::SUnits => "S",
            Scale::Both => "BOTH",
        }
    }

    pub fn next(self) -> Scale {
        match self {
            Scale::Dbm => Scale::SUnits,
            Scale::SUnits => Scale::Both,
            Scale::Both => Scale::Dbm,
        }
    }
}

/// The FFT window, mirrored from [`sdr::Window`] so it can be saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FftWindow {
    Hann,
    #[default]
    BlackmanHarris,
    Rectangular,
    FlatTop,
}

impl FftWindow {
    pub const ALL: [FftWindow; 4] =
        [FftWindow::Hann, FftWindow::BlackmanHarris, FftWindow::Rectangular, FftWindow::FlatTop];

    pub fn to_sdr(self) -> sdr::Window {
        match self {
            FftWindow::Hann => sdr::Window::Hann,
            FftWindow::BlackmanHarris => sdr::Window::BlackmanHarris,
            FftWindow::Rectangular => sdr::Window::Rectangular,
            FftWindow::FlatTop => sdr::Window::FlatTop,
        }
    }

    pub fn label(self) -> &'static str {
        self.to_sdr().label()
    }
}

/// FFT overlap, mirrored from [`sdr::Overlap`] so it can be saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum FftOverlap {
    Off,
    #[default]
    Half,
    ThreeQuarters,
}

impl FftOverlap {
    pub const ALL: [FftOverlap; 3] = [FftOverlap::Off, FftOverlap::Half, FftOverlap::ThreeQuarters];

    pub fn to_sdr(self) -> sdr::Overlap {
        match self {
            FftOverlap::Off => sdr::Overlap::Latest,
            FftOverlap::Half => sdr::Overlap::Half,
            FftOverlap::ThreeQuarters => sdr::Overlap::ThreeQuarters,
        }
    }

    pub fn label(self) -> &'static str {
        self.to_sdr().label()
    }
}

/// How fast the waterfall runs: frames gathered into each row (the strongest
/// of each bin over them, so nothing short is lost) and pixels each row is
/// drawn tall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum WaterfallSpeed {
    VeryFast,
    #[default]
    Fast,
    Medium,
    Slow,
    VerySlow,
}

impl WaterfallSpeed {
    pub const ALL: [WaterfallSpeed; 5] = [
        WaterfallSpeed::VeryFast,
        WaterfallSpeed::Fast,
        WaterfallSpeed::Medium,
        WaterfallSpeed::Slow,
        WaterfallSpeed::VerySlow,
    ];

    pub fn frames_per_row(self) -> u32 {
        match self {
            WaterfallSpeed::VeryFast | WaterfallSpeed::Fast => 1,
            WaterfallSpeed::Medium => 2,
            WaterfallSpeed::Slow => 4,
            WaterfallSpeed::VerySlow => 8,
        }
    }

    /// Physical pixels each row is drawn tall.
    pub fn px_per_row(self) -> f32 {
        if self == WaterfallSpeed::VeryFast { 2.0 } else { 1.0 }
    }

    pub fn label(self) -> &'static str {
        match self {
            WaterfallSpeed::VeryFast => "Very fast",
            WaterfallSpeed::Fast => "Fast",
            WaterfallSpeed::Medium => "Medium",
            WaterfallSpeed::Slow => "Slow",
            WaterfallSpeed::VerySlow => "Very slow",
        }
    }
}

/// The FFT sizes offered, up to 2^20 (a million points: 5.9 Hz bins at
/// 6144 kHz, 0.18 Hz at 192 kHz). CPU is plentiful and the spectrum has a
/// thread of its own, so the choice is resolution against time smear.
pub const FFT_SIZES: [usize; 9] =
    [4096, 8192, 16_384, 32_768, 65_536, 131_072, 262_144, 524_288, 1_048_576];

/// The frame rates offered.
pub const FPS_CHOICES: [f32; 4] = [15.0, 30.0, 45.0, 60.0];

/// The tuning steps offered (SDR Console's Step/Snap), Hz.
pub const STEPS_HZ: [u32; 6] = [1, 10, 100, 500, 1000, 5000];

/// The lowest and highest the floor and ceiling can be set to, dBm.
pub const LEVEL_MIN: f32 = -170.0;
pub const LEVEL_MAX: f32 = 10.0;
/// The ceiling stays at least this far over the floor.
pub const MIN_RANGE_DB: f32 = 20.0;

/// The scope's whole height, points, and the share the spectrum gets.
pub const HEIGHT_MIN: f32 = 220.0;
pub const HEIGHT_MAX: f32 = 1400.0;
pub const SPLIT_MIN: f32 = 0.15;
pub const SPLIT_MAX: f32 = 0.85;

/// Everything about the scope that is kept between runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScopeSettings {
    /// Bottom and top of the level scale, dBm. The waterfall's colours run
    /// over the same range.
    pub floor_dbm: f32,
    pub ceil_dbm: f32,
    /// FIT: keep the floor just under the noise and the ceiling over the
    /// strongest signal. Setting a level by hand turns it off.
    pub fit: bool,
    pub scale: Scale,
    pub palette: Palette,
    pub top: TopView,
    /// 3D drawn as a solid coloured surface (true) or as lines.
    pub solid_3d: bool,
    /// The solid 3D surface lit and shaded, SDR Console's look (true), or
    /// flat-coloured as SDRoxide draws it. Needs a GPU with compute.
    pub lit_3d: bool,
    /// The spectrum computed on the GPU from the raw I/Q (true), or on the
    /// CPU by the pipeline's spectrum thread. The GPU's needs compute
    /// shaders; without them the CPU's is used whatever this says.
    pub fft_gpu: bool,
    /// 3D rows laid down a second: how deep in time the surface reaches.
    pub surface_rows_per_sec: f32,
    /// The whole scope's height, points.
    pub height: f32,
    /// The share of it the spectrum (and 3D) gets; the waterfall has the rest.
    pub split: f32,
    pub step_hz: u32,
    /// Averaging four frames (AVG).
    pub avg: bool,
    pub peak: bool,
    pub fft_size: usize,
    pub fps: f32,
    pub window: FftWindow,
    pub overlap: FftOverlap,
    /// Re-resolve a zoomed view with its own FFT (the zoom FFT) rather than
    /// stretching the whole span's bins.
    pub zoom_fft: bool,
    /// The phosphor (persistence) trace, and how long its glow lasts, s.
    pub persist: bool,
    pub persist_secs: f32,
    /// The noise floor line on the trace.
    pub noise_line: bool,
    pub wf_speed: WaterfallSpeed,
    /// Waterfall drawn smooth between columns (true) or as blocks.
    pub wf_smooth: bool,
    /// The waterfall's own levels follow the noise (true), or use the
    /// spectrum's floor and ceiling.
    pub wf_auto: bool,
    /// Waterfall contrast (1 = as the levels say) and brightness (−0.5 to
    /// 0.5 of the scale).
    pub wf_contrast: f32,
    pub wf_brightness: f32,
    /// UTC time ticks down the waterfall's edge.
    pub wf_time: bool,
    /// The band plan strip under the spectrum.
    pub band_plan: bool,
    /// A transverter's (up/down converter's) local oscillator, Hz, signed:
    /// every frequency shown is the radio's own (IF) plus this. Zero for none.
    /// What the scope hands back (`tune_to`, `recentre`) is always the radio's
    /// own frequency.
    pub transverter_hz: f64,
    /// Frequency calibration, parts per million: what the radio's reference
    /// is off by. 0 for a GPS-disciplined reference.
    pub ppm: f64,
    /// The SCF view's own settings.
    pub scf: super::scf::ScfSettings,
}

impl Default for ScopeSettings {
    fn default() -> Self {
        ScopeSettings {
            floor_dbm: -130.0,
            ceil_dbm: -40.0,
            fit: true,
            scale: Scale::Both,
            palette: Palette::Elad,
            top: TopView::Spectrum,
            solid_3d: true,
            lit_3d: true,
            fft_gpu: true,
            surface_rows_per_sec: 16.0,
            height: 420.0,
            split: 0.42,
            step_hz: 100,
            avg: true,
            peak: false,
            fft_size: 65_536,
            fps: 60.0,
            window: FftWindow::BlackmanHarris,
            overlap: FftOverlap::Half,
            zoom_fft: true,
            persist: false,
            persist_secs: 1.5,
            noise_line: true,
            wf_speed: WaterfallSpeed::Fast,
            wf_smooth: true,
            wf_auto: false,
            wf_contrast: 1.0,
            wf_brightness: 0.0,
            wf_time: true,
            band_plan: true,
            transverter_hz: 0.0,
            ppm: 0.0,
            scf: Default::default(),
        }
    }
}

impl ScopeSettings {
    /// Set the floor by hand: FIT goes off, as in SDRoxide, or it would pull
    /// the level straight back. The ceiling is kept [`MIN_RANGE_DB`] above.
    pub fn set_floor(&mut self, v: f32) {
        self.fit = false;
        self.floor_dbm = v.clamp(LEVEL_MIN, self.ceil_dbm - MIN_RANGE_DB);
    }

    /// Set the ceiling by hand; as [`Self::set_floor`].
    pub fn set_ceil(&mut self, v: f32) {
        self.fit = false;
        self.ceil_dbm = v.clamp(self.floor_dbm + MIN_RANGE_DB, LEVEL_MAX);
    }

    pub fn set_height(&mut self, h: f32) {
        self.height = h.clamp(HEIGHT_MIN, HEIGHT_MAX);
    }

    pub fn set_split(&mut self, s: f32) {
        self.split = s.clamp(SPLIT_MIN, SPLIT_MAX);
    }

    /// The pipeline's averaging for AVG.
    pub fn averaging(&self) -> sdr::Averaging {
        if self.avg { sdr::Averaging::X4 } else { sdr::Averaging::Off }
    }

    /// Bring anything out of range (a hand-edited file, an older version)
    /// back in.
    pub fn sanitise(&mut self) {
        self.scf.sanitise();
        if !self.floor_dbm.is_finite() || !self.ceil_dbm.is_finite() {
            let d = ScopeSettings::default();
            self.floor_dbm = d.floor_dbm;
            self.ceil_dbm = d.ceil_dbm;
        }
        self.floor_dbm = self.floor_dbm.clamp(LEVEL_MIN, LEVEL_MAX - MIN_RANGE_DB);
        self.ceil_dbm = self.ceil_dbm.clamp(self.floor_dbm + MIN_RANGE_DB, LEVEL_MAX);
        self.set_height(if self.height.is_finite() { self.height } else { 420.0 });
        self.set_split(if self.split.is_finite() { self.split } else { 0.42 });
        if !STEPS_HZ.contains(&self.step_hz) {
            self.step_hz = 100;
        }
        if !FFT_SIZES.contains(&self.fft_size) {
            self.fft_size = 65_536;
        }
        self.persist_secs =
            if self.persist_secs.is_finite() { self.persist_secs.clamp(0.2, 10.0) } else { 1.5 };
        self.wf_contrast =
            if self.wf_contrast.is_finite() { self.wf_contrast.clamp(0.25, 4.0) } else { 1.0 };
        self.wf_brightness =
            if self.wf_brightness.is_finite() { self.wf_brightness.clamp(-0.5, 0.5) } else { 0.0 };
        if !self.ppm.is_finite() || self.ppm.abs() > 100.0 {
            self.ppm = 0.0;
        }
        if !self.fps.is_finite() {
            self.fps = 60.0;
        }
        self.fps = self.fps.clamp(5.0, sdr::spectrum::MAX_FPS);
        if !self.surface_rows_per_sec.is_finite() {
            self.surface_rows_per_sec = 16.0;
        }
        self.surface_rows_per_sec = self.surface_rows_per_sec.clamp(2.0, 60.0);
        if !self.transverter_hz.is_finite() {
            self.transverter_hz = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_round_trip_and_old_files_load() {
        let s = ScopeSettings {
            palette: Palette::Icom,
            transverter_hz: 116e6,
            top: TopView::Both,
            ..Default::default()
        };
        let j = serde_json::to_string(&s).unwrap();
        let back: ScopeSettings = serde_json::from_str(&j).unwrap();
        assert_eq!(back, s);
        // A file written before most fields existed.
        let old: ScopeSettings = serde_json::from_str(r#"{"floor_dbm": -120.0}"#).unwrap();
        assert_eq!(old.floor_dbm, -120.0);
        assert_eq!(old.palette, Palette::Elad);
        assert_eq!(old.transverter_hz, 0.0);
    }

    #[test]
    fn a_level_set_by_hand_turns_fit_off_and_keeps_the_range_open() {
        let mut s = ScopeSettings::default();
        assert!(s.fit);
        s.set_floor(-30.0);
        assert!(!s.fit);
        assert_eq!(s.floor_dbm, s.ceil_dbm - MIN_RANGE_DB);
        s.set_ceil(-200.0);
        assert_eq!(s.ceil_dbm, s.floor_dbm + MIN_RANGE_DB);
    }

    #[test]
    fn nonsense_is_brought_back_in_range() {
        let mut s = ScopeSettings {
            floor_dbm: f32::NAN,
            height: 5.0,
            split: 2.0,
            step_hz: 7,
            fft_size: 1000,
            fps: 1000.0,
            ..Default::default()
        };
        s.sanitise();
        assert_eq!(s.floor_dbm, -130.0);
        assert_eq!(s.height, HEIGHT_MIN);
        assert_eq!(s.split, SPLIT_MAX);
        assert_eq!(s.step_hz, 100);
        assert_eq!(s.fft_size, 65_536);
        assert_eq!(s.fps, 60.0);
    }
}
