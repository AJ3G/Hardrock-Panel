//! Each link from I/Q at the DUO's rates, at true RF: the vendored off-air
//! captures (HFDL, VDL2) and xng's own modulators (ACARS, AIS, Mode S).

use std::time::Duration;

use datalink::{LinkConfig, LinkDecoder, LinkEvent, LinkMode, LinkWorker, MessageBody, Tuning};
use decoders::resample::Resampler;
use sdr::Complex32;
use sdr::nco::Nco;

const UTC: f64 = 1_730_841_489.5;

fn fixture(rel: &str) -> Vec<Complex32> {
    let path = format!("{}/../../vendor/xng/crates/{rel}", env!("CARGO_MANIFEST_DIR"));
    let raw = std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    raw.chunks_exact(4)
        .map(|b| {
            Complex32::new(
                i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0,
                i16::from_le_bytes([b[2], b[3]]) as f32 / 32768.0,
            )
        })
        .collect()
}

/// `x` at `from` brought to `to` and moved `shift_hz` up.
fn place(x: &[Complex32], from: f64, to: f64, shift_hz: f64) -> Vec<Complex32> {
    let mut r = Resampler::<Complex32>::new(from, to);
    let mut up = Vec::new();
    r.process(x, &mut up);
    let mut out = Vec::new();
    Nco::new(shift_hz, to).mix(&up, &mut out);
    out
}

fn add(into: &mut [Complex32], x: &[Complex32]) {
    for (a, b) in into.iter_mut().zip(x) {
        *a += b;
    }
}

fn run(dec: &mut LinkDecoder, iq: &[Complex32], block: usize) -> Vec<datalink::Decoded> {
    let rate = dec.config().iq_rate;
    iq.chunks(block)
        .enumerate()
        .flat_map(|(i, c)| dec.process(c, UTC + (i * block) as f64 / rate))
        .collect::<Vec<_>>()
}

fn hfdl_squitter_gs(d: &datalink::Decoded) -> Option<u64> {
    match &d.message.body {
        MessageBody::Hfdl { kind, details } if kind == "squitter" => details["gs_id"].as_u64(),
        _ => None,
    }
}

#[test]
fn hfdl_off_air_from_a_24k_lane() {
    let iq = fixture("xng-mode-hfdl/tests/data/hfdl_21931khz_8s.i16");
    let cfg = LinkConfig {
        mode: LinkMode::Hfdl,
        iq_rate: 24_000.0,
        tuning: Tuning::new(21_931_000.0),
        channels_hz: vec![21_931_000],
    };
    let mut dec = LinkDecoder::new(cfg).unwrap();
    let got = run(&mut dec, &iq, 4800);
    let sq = got.iter().find(|d| hfdl_squitter_gs(d) == Some(4)).expect("Riverhead squitter");
    assert_eq!(sq.rf_hz, 21_931_000);
    assert_eq!(sq.message.frequency_hz, 21_931_000);
    assert!(sq.summary.contains("Riverhead"), "{}", sq.summary);
    assert!(sq.utc > UTC && sq.utc <= UTC + 8.0);
    assert_eq!(sq.message.timestamp.timestamp(), sq.utc.floor() as i64);
}

#[test]
fn hfdl_two_channels_at_once_from_192k_on_a_worker() {
    let lane = fixture("xng-mode-hfdl/tests/data/hfdl_21931khz_8s.i16");
    let rate = 192_000.0;
    // The radio centred 37 kHz below 21 931 kHz; the same signal copied onto
    // a second carrier 89 kHz below that.
    let centre = 21_894_000.0;
    let mut iq = place(&lane, 24_000.0, rate, 37_000.0);
    let other = place(&lane, 24_000.0, rate, -52_000.0);
    add(&mut iq, &other);
    let cfg = LinkConfig {
        mode: LinkMode::Hfdl,
        iq_rate: rate,
        tuning: Tuning::new(centre),
        channels_hz: vec![21_931_000, 21_842_000],
    };
    let w = LinkWorker::spawn(cfg).unwrap();
    let mut inter = Vec::with_capacity(iq.len() * 2);
    for s in &iq {
        inter.extend([s.re, s.im]);
    }
    let block = 2 * 19_200;
    for (i, c) in inter.chunks(block).enumerate() {
        w.push_interleaved(c, UTC + (i * block / 2) as f64 / rate);
        // Keep within the queue: the test feeds faster than real time.
        std::thread::sleep(Duration::from_millis(5));
    }
    let mut heard = Vec::new();
    while let Ok(ev) = w.events().recv_timeout(Duration::from_secs(20)) {
        if let LinkEvent::Decoded(d) = ev
            && hfdl_squitter_gs(&d) == Some(4)
        {
            heard.push(d.rf_hz);
        }
        if heard.contains(&21_931_000) && heard.contains(&21_842_000) {
            break;
        }
    }
    assert!(heard.contains(&21_931_000) && heard.contains(&21_842_000), "{heard:?}");
    assert_eq!(w.dropped(), 0);
}

#[test]
fn vdl2_off_air_through_a_converter_at_768k() {
    let lane = fixture("xng-mode-vdl2/tests/data/vdl2_offair_6s.i16");
    let rate = 768_000.0;
    // A 108 MHz converter LO with the DUO at 28.75 MHz: 136.75 MHz in the
    // middle, the capture placed on 136.875.
    let tuning = Tuning { centre_hz: 28_750_000.0, lo_hz: 108_000_000.0, ..Tuning::default() };
    let iq = place(&lane, 50_000.0, rate, 125_000.0);
    let cfg = LinkConfig {
        mode: LinkMode::Vdl2,
        iq_rate: rate,
        tuning,
        channels_hz: vec![136_875_000, 136_975_000],
    };
    let mut dec = LinkDecoder::new(cfg).unwrap();
    let got = run(&mut dec, &iq, 65_536);
    let hit = got
        .iter()
        .find(|d| d.json().contains("HB-IJW"))
        .unwrap_or_else(|| panic!("{:?}", got.iter().map(|d| &d.summary).collect::<Vec<_>>()));
    assert_eq!(hit.rf_hz, 136_875_000);
    assert_eq!(hit.channel, 0);
}

#[test]
fn acars_on_two_channels_at_1536k() {
    use xng_mode_acars::modulate::{FrameSpec, burst_iq};
    let rate = 1_536_000.0;
    let tuning = Tuning::new(131_000_000.0);
    let spec = |text, flight| FrameSpec {
        mode: '2',
        tail: "N3AJG",
        ack: None,
        label: "H1",
        block_id: '3',
        msg_num: Some("M01A"),
        flight: Some(flight),
        text,
        etb: false,
    };
    let a = burst_iq(&spec("HELLO FROM 131550", "AJ0001"), rate, 550_000.0, 0.4);
    let b = burst_iq(&spec("HELLO FROM 130450", "AJ0002"), rate, -550_000.0, 0.4);
    let mut iq = vec![Complex32::new(0.0, 0.0); a.len().max(b.len()) + 20_000];
    add(&mut iq[1000..], &a);
    add(&mut iq[9000..], &b);
    let cfg = LinkConfig::new(LinkMode::Acars, rate, tuning);
    assert!(cfg.channels_hz.contains(&131_550_000) && cfg.channels_hz.contains(&130_450_000));
    let mut dec = LinkDecoder::new(cfg).unwrap();
    let got = run(&mut dec, &iq, 65_536);
    let by = |hz| got.iter().find(|d| d.rf_hz == hz).map(|d| d.summary.clone());
    assert!(by(131_550_000).is_some_and(|s| s.contains("HELLO FROM 131550")), "{got:?}");
    assert!(by(130_450_000).is_some_and(|s| s.contains("AJ0002")), "{got:?}");
}

#[test]
fn ais_both_channels_through_a_converter_at_192k() {
    use xng_mode_ais::modulate::burst_iq;
    use xng_mode_ais::nmea::payload_to_bits;
    let rate = 192_000.0;
    let tuning = Tuning { centre_hz: 46_000_000.0, lo_hz: 116_000_000.0, ..Tuning::default() };
    let bits = payload_to_bits("177KQJ5000G?tO`K>RA1wUbN0TKH");
    let a = burst_iq(&bits, rate, -25_000.0, 0.4);
    let b = burst_iq(&bits, rate, 25_000.0, 0.4);
    let mut iq = vec![Complex32::new(0.0, 0.0); 4 * a.len()];
    add(&mut iq[2000..], &a);
    add(&mut iq[2 * a.len()..], &b);
    let mut dec = LinkDecoder::new(LinkConfig::new(LinkMode::Ais, rate, tuning)).unwrap();
    let got = run(&mut dec, &iq, 8192);
    for hz in [161_975_000, 162_025_000] {
        let d = got.iter().find(|d| d.rf_hz == hz).unwrap_or_else(|| panic!("{hz}: {got:?}"));
        assert!(d.summary.contains("477553000"), "{}", d.summary);
    }
}

#[test]
fn mode_s_at_6144k_off_centre() {
    use xng_mode_adsb::modulate::frame_iq;
    const ID_FRAME: [u8; 14] =
        [0x8D, 0x48, 0x40, 0xD6, 0x20, 0x2C, 0xC3, 0x71, 0xC3, 0x2C, 0xE0, 0x57, 0x60, 0x98];
    // Made at 12 MS/s, brought to 6.144 MS/s, 1090 MHz 300 kHz above centre.
    let mut hi = vec![Complex32::new(0.0, 0.0); 12_000];
    hi.extend(frame_iq(&ID_FRAME, 12, 0.6));
    hi.extend(vec![Complex32::new(0.0, 0.0); 24_000]);
    let rate = 6_144_000.0;
    let iq = place(&hi, 12e6, rate, 300_000.0);
    let cfg = LinkConfig::new(LinkMode::ModeS, rate, Tuning::new(1_089_700_000.0));
    let mut dec = LinkDecoder::new(cfg).unwrap();
    let got = run(&mut dec, &iq, 16_384);
    let d = got.first().unwrap_or_else(|| panic!("no frame"));
    assert!(d.summary.contains("KLM1023"), "{}", d.summary);
    assert_eq!(d.rf_hz, 1_090_000_000);
}
