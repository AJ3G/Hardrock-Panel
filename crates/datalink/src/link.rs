//! One data link over a span of I/Q: the channels split out, decoded in
//! parallel, and reported at true RF.

use chrono::{DateTime, Utc};
use rayon::prelude::*;
use sdr::Complex32;
use sdr::nco::Nco;
use xng_dsp::{ChannelizedDdc, SharedDdc};
use xng_mode_acars::AcarsMultiChannelDecoder;
use xng_mode_adsb::AdsbDecoder;
use xng_mode_ais::AisChannelDecoder;
use xng_mode_hfdl::HfdlChannelDecoder;
use xng_mode_vdl2::Vdl2ChannelDecoder;
use xng_types::source::{AppInfo, ChannelInfo, SdrInfo, StationIdentity};
use xng_types::{Message, MessageBody, Provenance};

use decoders::FreqCal;

use crate::hfdl_table::HfdlTable;

/// The data links.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinkMode {
    /// HF Data Link (ARINC 635), on the HF bands.
    Hfdl,
    /// Plain VHF ACARS (AM MSK, 2400 bd), 129 to 137 MHz.
    Acars,
    /// VDL Mode 2 (D8PSK), 136.6 to 137 MHz.
    Vdl2,
    /// Marine AIS, 161.975 and 162.025 MHz.
    Ais,
    /// Mode S and ADS-B on 1090 MHz.
    ModeS,
}

impl LinkMode {
    pub const ALL: [LinkMode; 5] =
        [LinkMode::Hfdl, LinkMode::Acars, LinkMode::Vdl2, LinkMode::Ais, LinkMode::ModeS];

    pub fn label(self) -> &'static str {
        match self {
            LinkMode::Hfdl => "HFDL",
            LinkMode::Acars => "ACARS",
            LinkMode::Vdl2 => "VDL2",
            LinkMode::Ais => "AIS",
            LinkMode::ModeS => "Mode S",
        }
    }

    /// The well-known channels, Hz. HFDL's come from [`HfdlTable`].
    pub fn known_channels_hz(self) -> Vec<u64> {
        match self {
            LinkMode::Hfdl => HfdlTable::new().all_hz(),
            // North American and European ACARS.
            LinkMode::Acars => vec![
                129_125_000,
                130_025_000,
                130_425_000,
                130_450_000,
                131_125_000,
                131_450_000,
                131_525_000,
                131_550_000,
                131_725_000,
                131_825_000,
                136_700_000,
                136_750_000,
                136_800_000,
                136_900_000,
            ],
            LinkMode::Vdl2 => vec![
                136_650_000,
                136_700_000,
                136_725_000,
                136_775_000,
                136_800_000,
                136_825_000,
                136_875_000,
                136_900_000,
                136_975_000,
            ],
            LinkMode::Ais => vec![161_975_000, 162_025_000],
            LinkMode::ModeS => vec![MODE_S_HZ],
        }
    }

    /// One-sided width each channel needs, Hz (Mode S: its whole signal).
    fn passband_hz(self) -> f64 {
        match self {
            // The carrier up to the top of its 3 kHz USB channel.
            LinkMode::Hfdl => 3_000.0,
            LinkMode::Acars => xng_mode_acars::CHANNEL_PASSBAND_HZ,
            LinkMode::Vdl2 => xng_mode_vdl2::CHANNEL_PASSBAND_HZ,
            LinkMode::Ais => xng_mode_ais::CHANNEL_PASSBAND_HZ,
            LinkMode::ModeS => 1_000_000.0,
        }
    }
}

const MODE_S_HZ: u64 = 1_090_000_000;

/// Where the I/Q sits in the radio spectrum.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Tuning {
    /// The I/Q centre the radio reports, Hz.
    pub centre_hz: f64,
    /// A converter's local oscillator, added to the radio's frequency to give
    /// the true RF (negative for a converter whose LO is above the band); 0
    /// without one.
    pub lo_hz: f64,
    /// The radio reference's correction (0 ppm for a GPS-locked radio).
    pub cal: FreqCal,
}

impl Tuning {
    pub fn new(centre_hz: f64) -> Self {
        Tuning { centre_hz, ..Tuning::default() }
    }

    /// The true RF at the centre of the I/Q.
    pub fn rf_centre_hz(&self) -> f64 {
        self.cal.rf_of_iq(self.centre_hz, 0.0, self.lo_hz)
    }

    /// Where true RF `rf_hz` falls in the I/Q, Hz from its centre.
    pub fn offset_of(&self, rf_hz: f64) -> f64 {
        rf_hz - self.rf_centre_hz()
    }
}

/// What to decode.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkConfig {
    pub mode: LinkMode,
    /// The I/Q's complex sample rate.
    pub iq_rate: f64,
    pub tuning: Tuning,
    /// The channels, true RF in Hz: for HFDL the SSB carrier (the frequency
    /// the station list gives), otherwise the channel centre. Ignored for
    /// Mode S.
    pub channels_hz: Vec<u64>,
}

impl LinkConfig {
    /// Every known channel of `mode` that fits in the span.
    pub fn new(mode: LinkMode, iq_rate: f64, tuning: Tuning) -> Self {
        let mut cfg = LinkConfig { mode, iq_rate, tuning, channels_hz: Vec::new() };
        cfg.channels_hz = mode.known_channels_hz().into_iter().filter(|&f| cfg.fits(f)).collect();
        cfg
    }

    /// The span's usable half-width: the outer tenth of the I/Q is left to
    /// the radio's own anti-alias filter.
    fn half_span(&self) -> f64 {
        0.45 * self.iq_rate
    }

    /// Where channel `rf_hz`'s band lies, Hz from the I/Q centre.
    fn band(&self, rf_hz: u64) -> (f64, f64) {
        let off = self.tuning.offset_of(rf_hz as f64);
        let pb = self.mode.passband_hz();
        match self.mode {
            LinkMode::Hfdl => (off, off + pb),
            _ => (off - pb, off + pb),
        }
    }

    fn fits(&self, rf_hz: u64) -> bool {
        let (lo, hi) = self.band(rf_hz);
        lo >= -self.half_span() && hi <= self.half_span()
    }

    /// Whether this can be decoded, and if not why.
    pub fn check(&self) -> Result<(), String> {
        if self.mode == LinkMode::ModeS {
            mode_s_rate(self.iq_rate)?;
            if !self.fits(MODE_S_HZ) {
                return Err(format!(
                    "1090 MHz is {:.0} kHz from the I/Q centre: tune closer",
                    self.tuning.offset_of(MODE_S_HZ as f64) / 1e3
                ));
            }
            return Ok(());
        }
        if self.channels_hz.is_empty() {
            return Err(format!("no {} channels in the span", self.mode.label()));
        }
        for &f in &self.channels_hz {
            if !self.fits(f) {
                return Err(format!("{:.3} MHz is outside the span", f as f64 / 1e6));
            }
        }
        Ok(())
    }
}

/// The rate Mode S is decoded at, for I/Q at `iq_rate`: the I/Q's own, which
/// xng's demodulator takes at any rate of 2 MS/s or more (a fractional number
/// of samples a microsecond included), so nothing is resampled.
fn mode_s_rate(iq_rate: f64) -> Result<f64, String> {
    if iq_rate >= 2e6 {
        Ok(iq_rate)
    } else {
        Err(format!(
            "Mode S needs at least 2 MS/s of I/Q (the DUO's 3.072 or 6.144 MHz); {:.3} MHz is too narrow",
            iq_rate / 1e6
        ))
    }
}

/// One decoded message.
#[derive(Debug, Clone)]
pub struct Decoded {
    pub mode: LinkMode,
    /// UTC (Unix seconds, system clock) at the end of the I/Q block the
    /// message finished in.
    pub utc: f64,
    /// The channel's true RF, Hz (the SSB carrier for HFDL).
    pub rf_hz: u64,
    /// Index into [`LinkConfig::channels_hz`].
    pub channel: usize,
    pub level_dbfs: f32,
    pub snr_db: Option<f32>,
    /// One line for a list: who and what.
    pub summary: String,
    /// The whole decode, in xng's model, timestamped with `utc`.
    pub message: Message,
}

impl Decoded {
    /// The message as JSON (xng's schema).
    pub fn json(&self) -> String {
        serde_json::to_string(&self.message).unwrap_or_default()
    }
}

/// The channel splitter in front of a set of channel decoders: one shared
/// polyphase channelizer pass, or (where its bins do not suit the rate) one
/// shared decimation.
enum Front {
    Channelized(ChannelizedDdc),
    Shared(SharedDdc),
}

impl Front {
    fn new(rate: f64, out_rate: f64, offsets: &[f64], passband: f64) -> Result<Self, String> {
        match ChannelizedDdc::new(rate, out_rate, offsets, passband) {
            Ok(c) => Ok(Front::Channelized(c)),
            Err(e) => SharedDdc::new(rate, out_rate, offsets, passband)
                .map(Front::Shared)
                .map_err(|e2| format!("channelizer: {e}; shared: {e2}")),
        }
    }

    fn process(&mut self, input: &[Complex32], out: &mut [Vec<Complex32>]) {
        for o in out.iter_mut() {
            o.clear();
        }
        match self {
            Front::Channelized(c) => c.process(input, out),
            Front::Shared(s) => s.process(input, out),
        }
    }
}

/// Use a shared front for more than one channel when the I/Q is much wider
/// than a channel; otherwise each decoder's own downconverter is cheaper.
fn wants_front(n: usize, iq_rate: f64, out_rate: f64) -> bool {
    n > 1 && iq_rate > 4.0 * out_rate
}

/// Channel decoders and, when shared, their front.
struct Bank<D> {
    front: Option<Front>,
    bufs: Vec<Vec<Complex32>>,
    decs: Vec<D>,
}

impl<D: Send> Bank<D> {
    /// Run every channel over `input`, in parallel, collecting `(channel,
    /// what it gave)`.
    fn run<T: Send>(
        &mut self,
        input: &[Complex32],
        f: impl Fn(&mut D, &[Complex32]) -> Vec<T> + Sync,
    ) -> Vec<(usize, T)> {
        let per: Vec<Vec<T>> = match &mut self.front {
            Some(front) => {
                front.process(input, &mut self.bufs);
                self.decs.par_iter_mut().zip(self.bufs.par_iter()).map(|(d, b)| f(d, b)).collect()
            }
            None => self.decs.par_iter_mut().map(|d| f(d, input)).collect(),
        };
        per.into_iter().enumerate().flat_map(|(i, v)| v.into_iter().map(move |t| (i, t))).collect()
    }
}

struct ModeS {
    nco: Option<Nco>,
    mixed: Vec<Complex32>,
    dec: AdsbDecoder,
}

enum Inner {
    Hfdl(Bank<HfdlChannelDecoder>),
    Vdl2(Bank<Vdl2ChannelDecoder>),
    Ais(Bank<AisChannelDecoder>),
    Acars(AcarsMultiChannelDecoder),
    ModeS(Box<ModeS>),
}

/// One data link's decoders over a span of I/Q.
pub struct LinkDecoder {
    cfg: LinkConfig,
    inner: Inner,
    prov: Vec<Provenance>,
}

impl LinkDecoder {
    pub fn new(cfg: LinkConfig) -> Result<Self, String> {
        cfg.check()?;
        let rate = cfg.iq_rate;
        let offsets: Vec<f64> =
            cfg.channels_hz.iter().map(|&f| cfg.tuning.offset_of(f as f64)).collect();
        let n = offsets.len();
        let inner = match cfg.mode {
            LinkMode::Hfdl => {
                let out = xng_mode_hfdl::CHANNEL_RATE;
                let sub = xng_mode_hfdl::SUBCARRIER_OFFSET_HZ;
                let bank = if wants_front(n, rate, out) {
                    // The front centres each channel on its subcarrier, which
                    // the decoder then takes as already there.
                    let subs: Vec<f64> = offsets.iter().map(|o| o + sub).collect();
                    Bank {
                        front: Some(Front::new(
                            rate,
                            out,
                            &subs,
                            xng_mode_hfdl::CHANNEL_PASSBAND_HZ,
                        )?),
                        bufs: vec![Vec::new(); n],
                        decs: (0..n)
                            .map(|_| HfdlChannelDecoder::new(out, -sub))
                            .collect::<Result<_, _>>()?,
                    }
                } else {
                    Bank {
                        front: None,
                        bufs: Vec::new(),
                        decs: offsets
                            .iter()
                            .map(|&o| HfdlChannelDecoder::new(rate, o))
                            .collect::<Result<_, _>>()?,
                    }
                };
                Inner::Hfdl(bank)
            }
            LinkMode::Vdl2 => {
                let out = xng_mode_vdl2::CHANNEL_RATE;
                let bank = if wants_front(n, rate, out) {
                    Bank {
                        front: Some(Front::new(
                            rate,
                            out,
                            &offsets,
                            xng_mode_vdl2::CHANNEL_PASSBAND_HZ,
                        )?),
                        bufs: vec![Vec::new(); n],
                        decs: (0..n)
                            .map(|_| Vdl2ChannelDecoder::new(out, 0.0))
                            .collect::<Result<_, _>>()?,
                    }
                } else {
                    Bank {
                        front: None,
                        bufs: Vec::new(),
                        decs: offsets
                            .iter()
                            .map(|&o| Vdl2ChannelDecoder::new(rate, o))
                            .collect::<Result<_, _>>()?,
                    }
                };
                Inner::Vdl2(bank)
            }
            LinkMode::Ais => {
                let out = xng_mode_ais::CHANNEL_RATE;
                let bank = if wants_front(n, rate, out) {
                    Bank {
                        front: Some(Front::new(
                            rate,
                            out,
                            &offsets,
                            xng_mode_ais::CHANNEL_PASSBAND_HZ,
                        )?),
                        bufs: vec![Vec::new(); n],
                        decs: cfg
                            .channels_hz
                            .iter()
                            .map(|&f| AisChannelDecoder::new(out, 0.0, f))
                            .collect::<Result<_, _>>()?,
                    }
                } else {
                    Bank {
                        front: None,
                        bufs: Vec::new(),
                        decs: offsets
                            .iter()
                            .zip(&cfg.channels_hz)
                            .map(|(&o, &f)| AisChannelDecoder::new(rate, o, f))
                            .collect::<Result<_, _>>()?,
                    }
                };
                Inner::Ais(bank)
            }
            LinkMode::Acars => Inner::Acars(AcarsMultiChannelDecoder::new(rate, &offsets)?),
            LinkMode::ModeS => {
                let out = mode_s_rate(rate)?;
                let off = cfg.tuning.offset_of(MODE_S_HZ as f64);
                Inner::ModeS(Box::new(ModeS {
                    nco: (off.abs() > 1.0).then(|| Nco::new(-off, rate)),
                    mixed: Vec::new(),
                    dec: AdsbDecoder::new_live(out)?,
                }))
            }
        };
        let chan_rate = match cfg.mode {
            LinkMode::Hfdl => xng_mode_hfdl::CHANNEL_RATE,
            LinkMode::Acars => xng_mode_acars::CHANNEL_RATE,
            LinkMode::Vdl2 => xng_mode_vdl2::CHANNEL_RATE,
            LinkMode::Ais => xng_mode_ais::CHANNEL_RATE,
            LinkMode::ModeS => mode_s_rate(rate)?,
        };
        let freqs: Vec<u64> =
            if cfg.mode == LinkMode::ModeS { vec![MODE_S_HZ] } else { cfg.channels_hz.clone() };
        let prov =
            freqs.iter().enumerate().map(|(i, &f)| provenance(i as u32, f, chan_rate)).collect();
        Ok(LinkDecoder { cfg, inner, prov })
    }

    pub fn config(&self) -> &LinkConfig {
        &self.cfg
    }

    /// Decode a block of I/Q whose first sample was heard at `utc` (Unix
    /// seconds, system clock).
    pub fn process(&mut self, iq: &[Complex32], utc: f64) -> Vec<Decoded> {
        let end = utc + iq.len() as f64 / self.cfg.iq_rate;
        let mode = self.cfg.mode;
        let chans = &self.cfg.channels_hz;
        let prov = &self.prov;
        let mk = |channel: usize, rf_hz: u64, level: f32, mut message: Message| {
            message.timestamp = utc_datetime(end);
            Decoded {
                mode,
                utc: end,
                rf_hz,
                channel,
                level_dbfs: level,
                snr_db: message.signal.snr_db,
                summary: summary(&message),
                message,
            }
        };
        match &mut self.inner {
            Inner::Hfdl(bank) => bank
                .run(iq, |d, x| {
                    let ev = d.process(x);
                    let level = d.level_dbfs();
                    ev.into_iter().map(|e| (e, level)).collect()
                })
                .into_iter()
                .map(|(i, (e, level))| {
                    let m = xng_mode_hfdl::to_message(&e, chans[i], level, prov[i].clone());
                    mk(i, chans[i], level, m)
                })
                .collect(),
            Inner::Vdl2(bank) => bank
                .run(iq, |d, x| {
                    let fr = d.process(x);
                    let level = d.level_dbfs();
                    fr.into_iter().map(|f| (f, level)).collect()
                })
                .into_iter()
                .map(|(i, (f, level))| {
                    let m = xng_mode_vdl2::to_message(&f, chans[i], level, prov[i].clone());
                    mk(i, chans[i], level, m)
                })
                .collect(),
            Inner::Ais(bank) => bank
                .run(iq, |d, x| {
                    let fr = d.process(x);
                    let level = d.level_dbfs();
                    fr.into_iter().map(|f| (f, level)).collect()
                })
                .into_iter()
                .map(|(i, ((f, nmea), level))| {
                    let m = xng_mode_ais::to_message(&f, nmea, chans[i], level, prov[i].clone());
                    mk(i, chans[i], level, m)
                })
                .collect(),
            Inner::Acars(dec) => {
                let got = dec.process(iq);
                let mut out = Vec::new();
                for (i, frames) in got {
                    let (level, noise) = (dec.level_dbfs(i), dec.noise_dbfs(i));
                    for f in frames {
                        let m =
                            xng_mode_acars::to_message(&f, chans[i], level, noise, prov[i].clone());
                        out.push(mk(i, chans[i], level, m));
                    }
                }
                out
            }
            Inner::ModeS(m) => {
                let mut x = iq;
                if let Some(nco) = &mut m.nco {
                    m.mixed.clear();
                    nco.mix(x, &mut m.mixed);
                    x = &m.mixed;
                }
                let frames = m.dec.process(x);
                let level = m.dec.level_dbfs();
                frames
                    .into_iter()
                    .map(|f| {
                        let msg = xng_mode_adsb::to_message(&f, MODE_S_HZ, prov[0].clone());
                        mk(0, MODE_S_HZ, level, msg)
                    })
                    .collect()
            }
        }
    }
}

fn provenance(index: u32, frequency_hz: u64, sample_rate: f64) -> Provenance {
    Provenance {
        station: StationIdentity::new("hardrock-panel"),
        app: AppInfo { name: "hardrock-panel".into(), version: env!("CARGO_PKG_VERSION").into() },
        sdr: Some(SdrInfo { id: "duo".into(), driver: "elad".into(), serial: None }),
        channel: Some(ChannelInfo { index, frequency_hz, sample_rate }),
    }
}

fn utc_datetime(utc: f64) -> DateTime<Utc> {
    let secs = utc.floor();
    DateTime::from_timestamp(secs as i64, ((utc - secs) * 1e9) as u32).unwrap_or_default()
}

/// A one-line description of a message for a list.
pub fn summary(m: &Message) -> String {
    let short = |s: &str| {
        let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
        if s.chars().count() > 60 {
            format!("{}…", s.chars().take(60).collect::<String>())
        } else {
            s
        }
    };
    match &m.body {
        MessageBody::Acars(a) => {
            let who = [a.flight.as_deref(), a.tail.as_deref()]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ");
            format!("{} {} {}", who, a.label, short(&a.text)).trim().to_string()
        }
        MessageBody::Ais { msg_type, mmsi, .. } => format!(
            "MMSI {} type {}",
            mmsi.map(|m| m.to_string()).unwrap_or_else(|| "?".into()),
            msg_type.map(|t| t.to_string()).unwrap_or_else(|| "?".into())
        ),
        MessageBody::ModeS { df, icao, callsign, altitude_ft, .. } => {
            let mut s = format!("DF{df} {}", icao.as_deref().unwrap_or("?"));
            if let Some(c) = callsign {
                s.push_str(&format!(" {}", c.trim()));
            }
            if let Some(a) = altitude_ft {
                s.push_str(&format!(" {a} ft"));
            }
            s
        }
        MessageBody::Hfdl { kind, details } | MessageBody::Vdl2 { kind, details } => {
            let gs = details["gs_name"]
                .as_str()
                .map(str::to_string)
                .or_else(|| {
                    details["gs_id"].as_u64().and_then(|id| {
                        crate::hfdl_table::station(id as u8).map(|s| s.name.to_string())
                    })
                })
                .filter(|_| matches!(m.body, MessageBody::Hfdl { .. }));
            match gs {
                Some(g) => format!("{kind} {g}"),
                None => kind.clone(),
            }
        }
        _ => format!("{:?}", m.mode),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_s_fits_only_the_wide_rates() {
        let t = Tuning::new(1_090_000_000.0);
        for (rate, ok) in [
            (192e3, false),
            (384e3, false),
            (768e3, false),
            (1.536e6, false),
            (3.072e6, true),
            (6.144e6, true),
        ] {
            let c = LinkConfig::new(LinkMode::ModeS, rate, t);
            assert_eq!(c.check().is_ok(), ok, "{rate}");
        }
        assert_eq!(mode_s_rate(6.144e6), Ok(6.144e6));
    }

    #[test]
    fn a_converter_lo_puts_channels_at_true_rf() {
        // AIS behind a 116 MHz converter: the DUO tunes 46 MHz.
        let t = Tuning { centre_hz: 46_000_000.0, lo_hz: 116_000_000.0, ..Tuning::default() };
        let c = LinkConfig::new(LinkMode::Ais, 192e3, t);
        assert_eq!(c.channels_hz, vec![161_975_000, 162_025_000]);
        assert_eq!(c.tuning.offset_of(161_975_000.0), -25_000.0);
        assert!(c.check().is_ok());
        // A radio tuned elsewhere finds no channels.
        let far = LinkConfig::new(LinkMode::Ais, 192e3, Tuning::new(150e6));
        assert!(far.check().is_err());
    }

    #[test]
    fn hfdl_picks_the_carriers_in_view() {
        let c = LinkConfig::new(LinkMode::Hfdl, 192e3, Tuning::new(8_910_000.0));
        assert!(c.channels_hz.contains(&8_912_000));
        assert!(c.channels_hz.iter().all(|&f| (8_823_600..=8_993_400).contains(&f)));
    }
}
