//! The digital modes' dial frequencies: the one table the scope's band-plan
//! markers and DATA keys, the radio window's data keys and DECODE's snap all
//! read (decision 39), with what each mode wants of the receiver: its
//! sideband, its audio passband and, for the keyboard modes, where in that
//! audio the decoder listens.
//!
//! All of it is public frequency information:
//!
//! * FT8, FT4, JT65, JT9, FST4, FST4W, Q65, MSK144 and WSPR: WSJT-X's default
//!   frequency list (IARU Region 2 entries; WSJT-X 2.6 "Frequencies" table)
//!   and WSPRnet's band list. FST4W shares WSPR's dials.
//! * JS8: JS8Call's default frequency list.
//! * PSK31 and RTTY: common practice on each band (the ARRL's and IARU
//!   Region 2's band plans give the segments; the dials are where activity
//!   is usually found, RTTY with a 2125/2295 Hz tone pair on USB).
//! * SSTV: the usual calling frequencies, LSB below 10 MHz and USB above.
//! * WEFAX: NOAA's (Boston NMF, New Orleans NMG, Pt. Reyes NMC) and DWD's
//!   (Hamburg DDH) published schedules. The assigned frequency is the
//!   carrier; the USB dial is 1.9 kHz below it, so the picture's 1500 to
//!   2300 Hz sits where the decoder looks.
//! * NAVTEX: 518 kHz (international), 490 kHz (national) and 4209.5 kHz
//!   (HF NAVTEX), ITU-R M.540 and the IMO NAVTEX manual. SITOR-B, 170 Hz
//!   shift, the dial 1.7 kHz below so its centre falls on the decoder's
//!   1700 Hz.
//! * ACARS 131.550 / 131.725 / 130.025 / 130.450 / 131.125 / 129.125 MHz,
//!   VDL2 136.975 / 136.725 / 136.775 / 136.875 MHz (ICAO and ARINC channel
//!   plans), AIS 161.975 and 162.025 MHz (ITU-R M.1371), marine DSC on
//!   channel 70, 156.525 MHz, and ADS-B on 1090 MHz. Only a transverter or
//!   converter brings these inside the FDM-DUO's 9 kHz to 54 MHz; they are
//!   drawn and offered only when one does.

use std::collections::BTreeMap;

/// Which sideband or mode a data mode is received in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Usb,
    Lsb,
    /// Lower sideband below 10 MHz and upper above, as voice and SSTV are.
    Voice,
    Am,
    Fm,
}

/// What a data mode is, for the markers' colour and the radio window's keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The WSJT-X modes, JS8.
    Weak,
    /// Keyboard modes: PSK31, RTTY, NAVTEX.
    Keyboard,
    /// Pictures: SSTV, WEFAX.
    Picture,
    /// Aircraft and ship data links, decoded from the I/Q.
    Link,
}

/// One data mode.
#[derive(Debug, PartialEq)]
pub struct Digi {
    /// As written on the keys and the markers.
    pub name: &'static str,
    pub side: Side,
    pub kind: Kind,
    /// The audio passband, Hz from the dial: for the sidebands, the audio
    /// range the decoder listens to; for AM and FM, `hi` is half the
    /// channel's width and `lo` is 0.
    pub lo: u32,
    pub hi: u32,
    /// Where in the audio the keyboard decoders listen (RTTY's mark/space
    /// centre, PSK31's carrier, NAVTEX's centre), Hz.
    pub audio_hz: Option<f64>,
    /// The dials: the USB or LSB dial for the sideband modes, the channel's
    /// centre for AM and FM. Hz, in frequency order.
    pub dials: &'static [u64],
    /// Offered as a key on the radio's face and in the scope's DATA row.
    pub key: bool,
}

/// Every data mode, in the order the markers and keys list them.
pub const DIGIS: &[Digi] = &[
    Digi {
        name: "FT8",
        side: Side::Usb,
        kind: Kind::Weak,
        lo: 200,
        hi: 3000,
        audio_hz: None,
        dials: &[
            1_840_000,
            3_573_000,
            5_357_000,
            7_074_000,
            10_136_000,
            14_074_000,
            18_100_000,
            21_074_000,
            24_915_000,
            28_074_000,
            50_313_000,
            50_323_000,
            70_154_000,
            144_174_000,
            432_174_000,
            1_296_174_000,
        ],
        key: true,
    },
    Digi {
        name: "FT4",
        side: Side::Usb,
        kind: Kind::Weak,
        lo: 200,
        hi: 3000,
        audio_hz: None,
        dials: &[
            3_575_500,
            7_047_500,
            10_140_000,
            14_080_000,
            18_104_000,
            21_140_000,
            24_919_000,
            28_180_000,
            50_318_000,
            144_170_000,
        ],
        key: true,
    },
    Digi {
        name: "WSPR",
        side: Side::Usb,
        kind: Kind::Weak,
        lo: 1400,
        hi: 1600,
        audio_hz: None,
        dials: &[
            136_000,
            474_200,
            1_836_600,
            3_568_600,
            5_364_700,
            7_038_600,
            10_138_700,
            14_095_600,
            18_104_600,
            21_094_600,
            24_924_600,
            28_124_600,
            50_293_000,
            144_489_000,
            432_300_000,
        ],
        key: true,
    },
    Digi {
        name: "JT65",
        side: Side::Usb,
        kind: Kind::Weak,
        lo: 200,
        hi: 3000,
        audio_hz: None,
        dials: &[
            1_838_000, 3_570_000, 7_076_000, 10_138_000, 14_076_000, 18_102_000, 21_076_000,
            24_917_000, 28_076_000, 50_310_000,
        ],
        key: false,
    },
    Digi {
        name: "JT9",
        side: Side::Usb,
        kind: Kind::Weak,
        lo: 200,
        hi: 3000,
        audio_hz: None,
        dials: &[
            1_839_000, 3_572_000, 7_078_000, 10_140_000, 14_078_000, 18_104_000, 21_078_000,
            24_919_000, 28_078_000, 50_312_000,
        ],
        key: false,
    },
    Digi {
        name: "FST4",
        side: Side::Usb,
        kind: Kind::Weak,
        lo: 200,
        hi: 3000,
        audio_hz: None,
        dials: &[136_000, 474_200],
        key: false,
    },
    Digi {
        name: "FST4W",
        side: Side::Usb,
        kind: Kind::Weak,
        lo: 1400,
        hi: 1600,
        audio_hz: None,
        dials: &[136_000, 474_200, 1_836_600, 3_568_600, 7_038_600, 10_138_700, 14_095_600],
        key: false,
    },
    Digi {
        name: "Q65",
        side: Side::Usb,
        kind: Kind::Weak,
        lo: 200,
        hi: 3000,
        audio_hz: None,
        dials: &[50_275_000],
        key: false,
    },
    Digi {
        name: "MSK144",
        side: Side::Usb,
        kind: Kind::Weak,
        lo: 300,
        hi: 2700,
        audio_hz: None,
        dials: &[50_260_000, 144_150_000],
        key: false,
    },
    Digi {
        name: "JS8",
        side: Side::Usb,
        kind: Kind::Weak,
        lo: 200,
        hi: 3000,
        audio_hz: None,
        dials: &[
            1_842_000, 3_578_000, 7_078_000, 10_130_000, 14_078_000, 18_104_000, 21_078_000,
            24_922_000, 28_078_000, 50_318_000,
        ],
        key: true,
    },
    Digi {
        name: "PSK31",
        side: Side::Usb,
        kind: Kind::Keyboard,
        lo: 1350,
        hi: 1650,
        audio_hz: Some(1500.0),
        dials: &[
            1_838_000, 3_580_000, 7_070_000, 10_142_000, 14_070_000, 18_100_000, 21_070_000,
            24_920_000, 28_120_000, 50_290_000,
        ],
        key: true,
    },
    Digi {
        name: "RTTY",
        side: Side::Usb,
        kind: Kind::Keyboard,
        lo: 2000,
        hi: 2420,
        audio_hz: Some(2210.0),
        dials: &[
            3_580_000, 7_043_000, 10_142_000, 14_083_000, 18_101_000, 21_083_000, 24_920_000,
            28_083_000, 50_300_000,
        ],
        key: true,
    },
    Digi {
        name: "NAVTEX",
        side: Side::Usb,
        kind: Kind::Keyboard,
        lo: 1500,
        hi: 1900,
        audio_hz: Some(1700.0),
        dials: &[488_300, 516_300, 4_207_800],
        key: false,
    },
    Digi {
        name: "SSTV",
        side: Side::Voice,
        kind: Kind::Picture,
        lo: 1100,
        hi: 2400,
        audio_hz: None,
        dials: &[3_845_000, 7_171_000, 14_230_000, 21_340_000, 28_680_000],
        key: false,
    },
    Digi {
        name: "WEFAX",
        side: Side::Usb,
        kind: Kind::Picture,
        lo: 1100,
        hi: 2700,
        audio_hz: None,
        dials: &[
            3_853_100,  // DDH 3855
            4_233_100,  // NMF 4235
            4_316_000,  // NMG 4317.9
            4_344_100,  // NMC 4346
            6_338_600,  // NMF 6340.5
            7_878_100,  // DDH 7880
            8_502_000,  // NMG 8503.9
            8_680_100,  // NMC 8682
            9_108_100,  // NMF 9110
            12_748_100, // NMF 12750
            12_784_100, // NMC 12786
            12_788_000, // NMG 12789.9
            13_880_600, // DDH 13882.5
            17_144_500, // NMG 17146.4
            17_149_300, // NMC 17151.2
            22_525_100, // NMC 22527
        ],
        key: false,
    },
    Digi {
        name: "ACARS",
        side: Side::Am,
        kind: Kind::Link,
        lo: 0,
        hi: 5000,
        audio_hz: None,
        dials: &[129_125_000, 130_025_000, 130_450_000, 131_125_000, 131_550_000, 131_725_000],
        key: false,
    },
    Digi {
        name: "VDL2",
        side: Side::Am,
        kind: Kind::Link,
        lo: 0,
        hi: 8000,
        audio_hz: None,
        dials: &[136_725_000, 136_775_000, 136_875_000, 136_975_000],
        key: false,
    },
    Digi {
        name: "DSC",
        side: Side::Fm,
        kind: Kind::Link,
        lo: 0,
        hi: 8000,
        audio_hz: None,
        dials: &[156_525_000],
        key: false,
    },
    Digi {
        name: "AIS",
        side: Side::Fm,
        kind: Kind::Link,
        lo: 0,
        hi: 8000,
        audio_hz: None,
        dials: &[161_975_000, 162_025_000],
        key: false,
    },
    Digi {
        name: "ADS-B",
        side: Side::Am,
        kind: Kind::Link,
        lo: 0,
        hi: 12_000,
        audio_hz: None,
        dials: &[1_090_000_000],
        key: false,
    },
];

/// The mode called `name`.
pub fn digi(name: &str) -> Option<&'static Digi> {
    DIGIS.iter().find(|d| d.name.eq_ignore_ascii_case(name))
}

/// The modes with a key on the radio's face and the scope's DATA row.
pub fn keys() -> impl Iterator<Item = &'static Digi> {
    DIGIS.iter().filter(|d| d.key)
}

/// A band the snap chooses within: its name for the notes, its edges in Hz.
#[derive(Debug, Clone, PartialEq)]
pub struct Band {
    pub label: String,
    pub lo: u64,
    pub hi: u64,
}

/// Where a mode's key or DECODE's snap sends the radio.
#[derive(Debug, Clone, PartialEq)]
pub struct Pick {
    /// The dial (or channel centre), as on the LCD: after any transverter
    /// offset.
    pub dial: u64,
    /// Said beside the keys when the mode has no frequency on the band the
    /// radio is on, and another one was taken.
    pub note: Option<String>,
}

impl Digi {
    /// The sideband's name, for the hover texts.
    pub fn side_name(&self) -> &'static str {
        match self.side {
            Side::Usb => "USB",
            Side::Lsb => "LSB",
            Side::Voice => "LSB below 10 MHz, USB above",
            Side::Am => "AM",
            Side::Fm => "FM",
        }
    }

    /// The receiver's mode at `dial`.
    pub fn sdr_mode(&self, dial: u64) -> sdr::Mode {
        match self.side {
            Side::Usb => sdr::Mode::Usb,
            Side::Lsb => sdr::Mode::Lsb,
            Side::Voice if dial < 10_000_000 => sdr::Mode::Lsb,
            Side::Voice => sdr::Mode::Usb,
            Side::Am => sdr::Mode::Am,
            Side::Fm => sdr::Mode::Fm,
        }
    }

    /// The radio's mode at `dial`, by its CAT name.
    pub fn rig_mode(&self, dial: u64) -> &'static str {
        match self.sdr_mode(dial) {
            sdr::Mode::Lsb => "LSB",
            sdr::Mode::Am => "AM",
            sdr::Mode::Fm => "FM",
            _ => "USB",
        }
    }

    /// The receiver's passband at `dial`, Hz from the dial as the scope
    /// draws it (negative below).
    pub fn passband(&self, dial: u64) -> (f32, f32) {
        let (lo, hi) = (self.lo as f32, self.hi as f32);
        match self.sdr_mode(dial) {
            sdr::Mode::Lsb => (-hi, -lo),
            sdr::Mode::Am | sdr::Mode::Fm => (-hi, hi),
            _ => (lo, hi),
        }
    }

    /// The width the radio's own receive filter is asked for, Hz.
    pub fn filter_width(&self) -> u32 {
        match self.side {
            Side::Am | Side::Fm => 2 * self.hi,
            _ => self.hi,
        }
    }

    /// The RF the mode occupies when the dial is at `dial`, Hz: what its
    /// marker shades on the band plan.
    pub fn extent(&self, dial: u64) -> (f64, f64) {
        let (a, b) = self.passband(dial);
        (dial as f64 + a as f64, dial as f64 + b as f64)
    }

    /// The dial to go to from `here` (as on the LCD): the mode's frequency on
    /// the band `here` is in, or on the nearest band if it is between bands;
    /// where the mode has none there, its frequency nearest `here`, with a
    /// note. Only dials `reach` allows (inside the radio's coverage through
    /// any converter) are taken; `None` if there are none.
    pub fn pick(&self, here: u64, bands: &[Band], reach: impl Fn(u64) -> bool) -> Option<Pick> {
        let can: Vec<u64> = self.dials.iter().copied().filter(|&f| reach(f)).collect();
        let near = |list: &[u64]| list.iter().copied().min_by_key(|f| f.abs_diff(here));
        let band = bands
            .iter()
            .find(|b| here >= b.lo && here <= b.hi)
            .or_else(|| bands.iter().min_by_key(|b| b.lo.abs_diff(here).min(b.hi.abs_diff(here))));
        if let Some(b) = band {
            let on: Vec<u64> = can.iter().copied().filter(|&f| f >= b.lo && f <= b.hi).collect();
            if let Some(f) = near(&on) {
                return Some(Pick { dial: f, note: None });
            }
        }
        let f = near(&can)?;
        let to = bands.iter().find(|b| f >= b.lo && f <= b.hi).map(|b| b.label.as_str());
        let note = match (band, to) {
            (Some(b), Some(t)) => format!("No {} frequency on {}, so {}", self.name, b.label, t),
            (Some(b), None) => {
                format!("No {} frequency on {}, so {}", self.name, b.label, mhz(f))
            }
            (None, _) => format!("{} at {}", self.name, mhz(f)),
        };
        Some(Pick { dial: f, note: Some(note) })
    }
}

/// "14.074 MHz", "518.0 kHz".
pub fn mhz(hz: u64) -> String {
    if hz < 1_000_000 {
        format!("{:.1} kHz", hz as f64 / 1e3)
    } else {
        let t = format!("{:.4}", hz as f64 / 1e6);
        format!("{} MHz", t.trim_end_matches('0').trim_end_matches('.'))
    }
}

/// Every mode's dial inside `[lo, hi]` Hz (as on the LCD), for the markers
/// on the band plan strip: by dial, the modes there (FT8 and others may
/// share one), and whose extent reaches into the view counts too.
pub fn marks(lo: f64, hi: f64) -> BTreeMap<u64, Vec<&'static Digi>> {
    let mut m: BTreeMap<u64, Vec<&'static Digi>> = BTreeMap::new();
    for d in DIGIS {
        for &f in d.dials {
            let (a, b) = d.extent(f);
            if b >= lo && a <= hi {
                m.entry(f).or_default().push(d);
            }
        }
    }
    m
}

/// The marker under `hz` (as on the LCD), if any: a dial within `tol` Hz, or
/// else one whose extent holds `hz`, the narrowest first.
pub fn mark_at(hz: f64, tol: f64) -> Option<(&'static Digi, u64)> {
    let mut best: Option<(&'static Digi, u64, f64)> = None;
    for d in DIGIS {
        for &f in d.dials {
            let (a, b) = d.extent(f);
            let near = (hz - f as f64).abs() <= tol;
            let inside = hz >= a && hz <= b;
            if near || inside {
                let score = if near { -1.0 } else { b - a };
                if best.is_none_or(|(_, _, s)| score < s) {
                    best = Some((d, f, score));
                }
            }
        }
    }
    best.map(|(d, f, _)| (d, f))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hf() -> Vec<Band> {
        [
            ("160 m", 1_800_000, 2_000_000),
            ("80 m", 3_500_000, 4_000_000),
            ("40 m", 7_000_000, 7_300_000),
            ("30 m", 10_100_000, 10_150_000),
            ("20 m", 14_000_000, 14_350_000),
            ("6 m", 50_000_000, 54_000_000),
            ("2 m", 144_000_000, 148_000_000),
        ]
        .map(|(l, lo, hi)| Band { label: l.to_string(), lo, hi })
        .to_vec()
    }

    /// The FDM-DUO with no converter.
    fn duo(f: u64) -> bool {
        (9_000..=54_000_000).contains(&f)
    }

    #[test]
    fn every_table_is_in_order_and_every_name_unique() {
        for d in DIGIS {
            assert!(d.dials.windows(2).all(|w| w[0] < w[1]), "{}", d.name);
            assert!(d.lo < d.hi || d.lo == 0, "{}", d.name);
            assert_eq!(digi(d.name).map(|x| x.name), Some(d.name));
        }
        let keys: Vec<&str> = keys().map(|d| d.name).collect();
        assert_eq!(keys, ["FT8", "FT4", "WSPR", "JS8", "PSK31", "RTTY"]);
    }

    #[test]
    fn a_key_goes_to_the_modes_frequency_on_this_band() {
        let ft8 = digi("FT8").unwrap();
        assert_eq!(ft8.pick(7_150_000, &hf(), duo), Some(Pick { dial: 7_074_000, note: None }));
        // Between bands: the nearest band's.
        assert_eq!(ft8.pick(9_000_000, &hf(), duo).unwrap().dial, 10_136_000);
        // On 6 m, the nearer of the two.
        assert_eq!(ft8.pick(50_320_000, &hf(), duo).unwrap().dial, 50_323_000);
        // None on this band: the nearest, and it says so.
        let rtty = digi("RTTY").unwrap();
        let p = rtty.pick(1_900_000, &hf(), duo).unwrap();
        assert_eq!(p.dial, 3_580_000);
        assert_eq!(p.note.as_deref(), Some("No RTTY frequency on 160 m, so 80 m"));
        // NAVTEX is on no amateur band.
        let p = digi("navtex").unwrap().pick(3_600_000, &hf(), duo).unwrap();
        assert_eq!(p.dial, 4_207_800);
        assert!(p.note.unwrap().contains("4.2078 MHz"));
    }

    #[test]
    fn only_frequencies_the_radio_reaches_are_taken() {
        let ft8 = digi("FT8").unwrap();
        // No converter: 2 m is out of reach, so the nearest on 6 m.
        let p = ft8.pick(145_000_000, &hf(), duo).unwrap();
        assert_eq!(p.dial, 50_323_000);
        assert_eq!(p.note.as_deref(), Some("No FT8 frequency on 2 m, so 6 m"));
        // A 2 m converter on 28 MHz (116 MHz LO).
        let xv = |f: u64| (116_009_000..=170_000_000).contains(&f);
        assert_eq!(ft8.pick(144_300_000, &hf(), xv).unwrap().dial, 144_174_000);
        assert_eq!(digi("ACARS").unwrap().pick(14_074_000, &hf(), duo), None);
        assert_eq!(digi("ACARS").unwrap().pick(131_000_000, &hf(), xv).unwrap().dial, 131_125_000);
    }

    #[test]
    fn each_mode_has_its_sideband_and_passband() {
        let ft8 = digi("FT8").unwrap();
        assert_eq!(ft8.passband(14_074_000), (200.0, 3000.0));
        assert_eq!(ft8.extent(14_074_000), (14_074_200.0, 14_077_000.0));
        let sstv = digi("SSTV").unwrap();
        assert_eq!(sstv.sdr_mode(7_171_000), sdr::Mode::Lsb);
        assert_eq!(sstv.rig_mode(7_171_000), "LSB");
        assert_eq!(sstv.passband(7_171_000), (-2400.0, -1100.0));
        assert_eq!(sstv.rig_mode(14_230_000), "USB");
        assert_eq!(digi("WSPR").unwrap().passband(14_095_600), (1400.0, 1600.0));
        let ais = digi("AIS").unwrap();
        assert_eq!(ais.passband(161_975_000), (-8000.0, 8000.0));
        assert_eq!(ais.filter_width(), 16_000);
        // NAVTEX's centre lands on the decoder's 1700 Hz.
        let nav = digi("NAVTEX").unwrap();
        assert_eq!(nav.dials[1] + nav.audio_hz.unwrap() as u64, 518_000);
        // WEFAX's 1900 Hz centre on the assigned frequency.
        assert_eq!(digi("WEFAX").unwrap().dials[1] + 1900, 4_235_000);
    }

    #[test]
    fn markers_on_screen_and_under_the_pointer() {
        let m = marks(14_060_000.0, 14_100_000.0);
        assert_eq!(m[&14_074_000].iter().map(|d| d.name).collect::<Vec<_>>(), ["FT8"]);
        assert!(m.contains_key(&14_070_000));
        assert!(m[&14_095_600].iter().any(|d| d.name == "FST4W"));
        assert!(!m.contains_key(&14_230_000));
        // A dial just left of the view still shades into it.
        assert!(marks(14_075_000.0, 14_076_000.0).contains_key(&14_074_000));
        // Under the pointer: the tick first, else the narrowest extent.
        assert_eq!(
            mark_at(14_074_050.0, 100.0).map(|(d, f)| (d.name, f)),
            Some(("FT8", 14_074_000))
        );
        assert_eq!(mark_at(14_076_050.0, 100.0).map(|(d, _)| d.name), Some("JT65"));
        assert_eq!(mark_at(14_075_000.0, 100.0).map(|(d, _)| d.name), Some("FT8"));
        assert_eq!(mark_at(14_200_000.0, 100.0), None);
    }

    #[test]
    fn frequencies_read_as_mhz_or_khz() {
        assert_eq!(mhz(14_074_000), "14.074 MHz");
        assert_eq!(mhz(518_000), "518.0 kHz");
        assert_eq!(mhz(144_000_000), "144 MHz");
    }
}
