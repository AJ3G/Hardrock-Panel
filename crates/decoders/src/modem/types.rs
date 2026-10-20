// Ported from SDRoxide (https://github.com/dividebysandwich/sdroxide),
// crates/sdroxide-types/src/sstv.rs and digi.rs, GPL-3.0-or-later.

//! The SSTV modes and Hellschreiber variants the modems are configured with.


/// One SSTV transmission mode. `Mode::Sstv` is the radio mode; this picks the
/// specific line format used for encode/decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SstvMode {
    Scottie1,
    Scottie2,
    ScottieDx,
    Martin1,
    Martin2,
    Robot72,
    Robot36,
    /// Wraase SC-2 180 — 320×256 RGB, 711 ms a line. Very widely used on
    /// 3.730 MHz and 14.230 MHz, and the first of the modes added for issue
    /// #421: until then a station sending one met a receiver that showed a
    /// perfect signal on the waterfall and said "waiting for a signal".
    WraaseSc2_180,
    /// Wraase SC-2 120 — the same format at 476 ms a line.
    WraaseSc2_120,
    // The PD family (Martin Bruchanov OK2MNM's, after the "PD" in the original
    // program). Four scans between syncs, carrying *two* image lines: luma for
    // each of them either side of one shared pair of chroma scans. That is why
    // they are cheap for the bandwidth — half the chroma — and why they need a
    // line structure none of the older modes do.
    Pd50,
    Pd90,
    Pd120,
    Pd160,
    Pd180,
    Pd240,
    Pd290,
}

impl Default for SstvMode {
    fn default() -> Self {
        SstvMode::Scottie1
    }
}

impl SstvMode {
    /// All modes, in a sensible menu order.
    pub const ALL: [SstvMode; 16] = [
        SstvMode::Scottie1,
        SstvMode::Scottie2,
        SstvMode::ScottieDx,
        SstvMode::Martin1,
        SstvMode::Martin2,
        SstvMode::Robot72,
        SstvMode::Robot36,
        SstvMode::WraaseSc2_180,
        SstvMode::WraaseSc2_120,
        SstvMode::Pd50,
        SstvMode::Pd90,
        SstvMode::Pd120,
        SstvMode::Pd160,
        SstvMode::Pd180,
        SstvMode::Pd240,
        SstvMode::Pd290,
    ];

    /// Short human label for buttons/menus.
    pub fn label(self) -> &'static str {
        match self {
            SstvMode::Scottie1 => "Scottie 1",
            SstvMode::Scottie2 => "Scottie 2",
            SstvMode::ScottieDx => "Scottie DX",
            SstvMode::Martin1 => "Martin 1",
            SstvMode::Martin2 => "Martin 2",
            SstvMode::Robot72 => "Robot 72",
            SstvMode::Robot36 => "Robot 36",
            SstvMode::WraaseSc2_180 => "SC2-180",
            SstvMode::WraaseSc2_120 => "SC2-120",
            SstvMode::Pd50 => "PD50",
            SstvMode::Pd90 => "PD90",
            SstvMode::Pd120 => "PD120",
            SstvMode::Pd160 => "PD160",
            SstvMode::Pd180 => "PD180",
            SstvMode::Pd240 => "PD240",
            SstvMode::Pd290 => "PD290",
        }
    }

    /// Transmitted image size in pixels, `(width, height)`.
    pub fn dimensions(self) -> (u16, u16) {
        match self {
            // Scottie/Martin/Wraase are 320×256.
            SstvMode::Scottie1
            | SstvMode::Scottie2
            | SstvMode::ScottieDx
            | SstvMode::Martin1
            | SstvMode::Martin2
            | SstvMode::WraaseSc2_180
            | SstvMode::WraaseSc2_120 => (320, 256),
            // Robot modes are 320×240.
            SstvMode::Robot72 | SstvMode::Robot36 => (320, 240),
            // The PD family carries a different size per mode — this is the
            // one family where the picture really does get bigger with the
            // air time, which is most of the point of it.
            SstvMode::Pd50 | SstvMode::Pd90 => (320, 256),
            SstvMode::Pd160 => (512, 400),
            SstvMode::Pd120 | SstvMode::Pd180 | SstvMode::Pd240 => (640, 496),
            SstvMode::Pd290 => (800, 616),
        }
    }

    /// How many *image* lines one transmitted line carries.
    ///
    /// One for every mode but the PD family, which sends luma for two lines
    /// either side of a single pair of chroma scans and so covers two rows per
    /// sync pulse.
    pub fn rows_per_line(self) -> u16 {
        match self {
            SstvMode::Pd50
            | SstvMode::Pd90
            | SstvMode::Pd120
            | SstvMode::Pd160
            | SstvMode::Pd180
            | SstvMode::Pd240
            | SstvMode::Pd290 => 2,
            _ => 1,
        }
    }

    /// The 7-bit VIS code identifying this mode in the calibration header.
    ///
    /// The codes are the published ones — JL Barber N7CXI's 2000 mode
    /// specification, as implemented by every decoder on the air. Checked
    /// against `windytan/slowrx`'s `modespec.c` VIS map.
    pub fn vis_code(self) -> u8 {
        match self {
            SstvMode::Robot36 => 0x08,
            SstvMode::Robot72 => 0x0C,
            SstvMode::Martin2 => 0x28,
            SstvMode::Martin1 => 0x2C,
            SstvMode::WraaseSc2_180 => 0x37,
            SstvMode::Scottie2 => 0x38,
            SstvMode::Scottie1 => 0x3C,
            SstvMode::WraaseSc2_120 => 0x3F,
            SstvMode::ScottieDx => 0x4C,
            SstvMode::Pd50 => 0x5D,
            SstvMode::Pd290 => 0x5E,
            SstvMode::Pd120 => 0x5F,
            SstvMode::Pd180 => 0x60,
            SstvMode::Pd240 => 0x61,
            SstvMode::Pd160 => 0x62,
            SstvMode::Pd90 => 0x63,
        }
    }

    /// Map a decoded VIS code back to a mode, if recognised.
    pub fn from_vis(code: u8) -> Option<SstvMode> {
        SstvMode::ALL.into_iter().find(|m| m.vis_code() == code)
    }

    /// The name of a mode this build does **not** decode, for a VIS code that
    /// came through with good parity.
    ///
    /// A receiver that has read a valid header knows exactly what it is being
    /// sent and exactly why it is about to draw nothing. Saying so is the
    /// difference between "the SSTV decoder is broken" and "that station is
    /// using Pasokon P3" — issue #421, where a picture arrived in a mode this
    /// decoder did not have and the panel went on saying *waiting for a
    /// signal* with a textbook signal on the waterfall.
    ///
    /// Only the codes that are actually assigned; an unassigned one is a
    /// misread header rather than a mode, and claiming otherwise would turn
    /// noise into a confident wrong answer.
    pub fn unsupported_name(code: u8) -> Option<&'static str> {
        Some(match code {
            0x02 => "Robot 8 B/W",
            0x04 => "Robot 24",
            0x06 => "Robot 12 B/W",
            0x0A => "Robot 24 B/W",
            0x20 => "Martin 4",
            0x24 => "Martin 3",
            0x71 => "Pasokon P3",
            0x72 => "Pasokon P5",
            0x73 => "Pasokon P7",
            _ => return None,
        })
    }
}

/// Hellschreiber variant. Feld Hell is the classic on/off-keyed facsimile mode;
/// X5 and X9 speed it up, Slow Hell crawls for weak signals, and the FSK
/// variants keep the carrier up and shift it instead of keying it.
///
/// Rates follow fldigi's `feldcolumnrate` (character columns per second); every
/// variant scans 14 dot rows per column and 7 columns per character cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HellVariant {
    #[default]
    Feld,
    Slow,
    X5,
    X9,
    Fsk245,
    Fsk105,
    Hell80,
}

impl HellVariant {
    /// Dot rows scanned per character column.
    pub const ROWS: usize = 14;
    /// Columns per character cell; the last is the inter-character gap.
    pub const CELL_COLS: usize = 7;

    pub const ALL: [HellVariant; 7] = [
        HellVariant::Feld,
        HellVariant::Slow,
        HellVariant::X5,
        HellVariant::X9,
        HellVariant::Fsk245,
        HellVariant::Fsk105,
        HellVariant::Hell80,
    ];

    pub fn label(self) -> &'static str {
        match self {
            HellVariant::Feld => "FELD",
            HellVariant::Slow => "SLOW",
            HellVariant::X5 => "X5",
            HellVariant::X9 => "X9",
            HellVariant::Fsk245 => "FSK245",
            HellVariant::Fsk105 => "FSK105",
            HellVariant::Hell80 => "HELL80",
        }
    }

    /// Character columns per second (fldigi's `feldcolumnrate`).
    pub fn column_rate(self) -> f64 {
        match self {
            HellVariant::Feld => 17.5,
            HellVariant::Slow => 2.1875,
            HellVariant::X5 => 87.5,
            HellVariant::X9 => 157.5,
            HellVariant::Fsk245 => 17.5,
            HellVariant::Fsk105 => 17.5,
            HellVariant::Hell80 => 35.0,
        }
    }

    /// Dots (transmitted pixels) per second — 14 rows in every column.
    pub fn pixel_rate(self) -> f64 {
        self.column_rate() * Self::ROWS as f64
    }

    /// Characters per second — 2.5 for classic Feld Hell.
    pub fn chars_per_sec(self) -> f64 {
        self.column_rate() / Self::CELL_COLS as f64
    }

    /// **Peak-to-peak** FSK shift in Hz; 0 for the on/off-keyed variants.
    ///
    /// This is fldigi's `hell_bandwidth`, which it applies as `tone ± value/2` —
    /// so it is the full shift, not the deviation.
    pub fn shift_hz(self) -> f64 {
        match self {
            HellVariant::Fsk245 => 122.5,
            HellVariant::Fsk105 => 55.0,
            HellVariant::Hell80 => 300.0,
            _ => 0.0,
        }
    }

    /// True for the frequency-shifted variants (continuous carrier); false for
    /// the on/off-keyed ones.
    pub fn is_fsk(self) -> bool {
        self.shift_hz() > 0.0
    }

    /// Nominal occupied bandwidth in Hz — fldigi's receive-filter width. Drives
    /// the receive filter, the waterfall markers, and the audio-centre clamp.
    pub fn bandwidth_hz(self) -> f64 {
        let raw = if self.is_fsk() { 4.0 * self.shift_hz() } else { 1.2 * self.pixel_rate() };
        5.0 * (raw / 5.0).round()
    }
}
