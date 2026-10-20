// Ported from SDRoxide (https://github.com/dividebysandwich/sdroxide),
// crates/sdroxide-tci/src/server/text.rs, GPL-3.0-or-later, with SDRoxide's
// engine commands replaced by this crate's own requests.

//! Text translation for the server: client commands become [`ClientReq`]s,
//! and a changed [`RigState`] becomes the status lines to broadcast. No I/O,
//! so every rule here is unit-testable.

use super::protocol as p;
use crate::{RigState, Vfo};

/// One decoded request from a client.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ClientReq {
    SetFreq(Vfo, f64),
    SetCenter(f64),
    SetMode(crate::Mode),
    SetSplit(bool),
    /// `trx:<rx>,<bool>[,tci]`: key or unkey. `tci` says the client streams
    /// the audio; without it the radio's own input is transmitted.
    Key {
        on: bool,
        tci_audio: bool,
    },
    /// `tune:<rx>,<bool>`.
    Tune(bool),
    Iq(bool),
    IqRate(u32),
    Audio(bool),
    TxSensors(bool),
    RxSensors(bool),
    /// Understood but not acted on; the client is told what is true.
    Ignored,
}

/// The receiver we serve. The panel has one, with two VFOs as channels.
pub(crate) const RX: u32 = 0;

fn flag(s: &str) -> Option<bool> {
    match s.trim().to_lowercase().as_str() {
        "true" | "1" | "on" => Some(true),
        "false" | "0" | "off" => Some(false),
        _ => None,
    }
}

fn vfo_of(channel: &str) -> Option<Vfo> {
    match channel.trim() {
        "0" => Some(Vfo::A),
        "1" => Some(Vfo::B),
        _ => None,
    }
}

fn parts(args: &str) -> Vec<&str> {
    args.split(',').map(str::trim).collect()
}

/// A flag that may or may not be prefixed by a receiver index.
fn indexed_flag(args: &[&str]) -> Option<bool> {
    match args {
        [v] => flag(v),
        [_rx, v, ..] => flag(v),
        _ => None,
    }
}

/// Translate one `(command, args)` pair. Unknown commands give `None`, and
/// the caller stays silent rather than guess.
pub(crate) fn translate(cmd: &str, args: &str, state: &RigState) -> Option<ClientReq> {
    let a = parts(args);
    let hz = |s: &str| s.parse::<f64>().ok().filter(|v| v.is_finite());
    Some(match cmd {
        "vfo" => match a.as_slice() {
            [_rx, ch, f, ..] => ClientReq::SetFreq(vfo_of(ch)?, hz(f)?),
            _ => return None,
        },
        "dds" => match a.as_slice() {
            [_rx, f, ..] => ClientReq::SetCenter(hz(f)?),
            _ => return None,
        },
        // An offset from the I/Q centre; the VFO is tuned to the result.
        "if" => match a.as_slice() {
            [_rx, ch, off, ..] => ClientReq::SetFreq(vfo_of(ch)?, state.center_hz + hz(off)?),
            _ => return None,
        },
        "modulation" => match a.as_slice() {
            [_rx, m, ..] => ClientReq::SetMode(p::tci_to_mode(m)?),
            _ => return None,
        },
        "split_enable" => ClientReq::SetSplit(indexed_flag(&a)?),

        "trx" => match a.as_slice() {
            [_rx, on, rest @ ..] => ClientReq::Key {
                on: flag(on)?,
                tci_audio: rest.first().is_some_and(|s| s.eq_ignore_ascii_case("tci")),
            },
            _ => return None,
        },
        "tune" => ClientReq::Tune(indexed_flag(&a)?),
        // The transmit power stays the panel's: a client raising it could
        // overdrive the amplifier. Acknowledged with the real value.
        "drive" | "tune_drive" => ClientReq::Ignored,
        "tx_sensors_enable" => ClientReq::TxSensors(flag(a.first()?)?),
        "rx_sensors_enable" => ClientReq::RxSensors(flag(a.first()?)?),
        // Audio level and mute are the operator's; acknowledged, not obeyed.
        "mute" | "volume" | "rx_mute" => ClientReq::Ignored,

        "iq_start" | "iq_stop" => match a.first().and_then(|r| r.parse::<u32>().ok()) {
            Some(RX) | None => ClientReq::Iq(cmd == "iq_start"),
            _ => ClientReq::Ignored,
        },
        "audio_start" | "audio_stop" => match a.first().and_then(|r| r.parse::<u32>().ok()) {
            Some(RX) | None => ClientReq::Audio(cmd == "audio_start"),
            _ => ClientReq::Ignored,
        },
        "iq_samplerate" => ClientReq::IqRate(a.first()?.parse().ok()?),
        "audio_samplerate" | "rx_enable" | "start" | "stop" => ClientReq::Ignored,
        _ => return None,
    })
}

/// For a command not obeyed, the line that states what is true instead of
/// what the client asked. `None` means a plain echo is honest.
pub(crate) fn truth_for(cmd: &str, st: &RigState) -> Option<String> {
    Some(match cmd {
        "rx_enable" => p::rx_enable(RX, true),
        "audio_samplerate" => p::audio_samplerate(super::AUDIO_RATE_HZ),
        "drive" => p::drive(RX, st.drive_pct),
        "tune_drive" => p::tune_drive(RX, st.drive_pct),
        "vfo" | "if" => {
            return Some(format!(
                "{}{}{}{}",
                p::vfo(RX, 0, st.vfo_a_hz),
                p::vfo(RX, 1, st.vfo_b_hz),
                p::if_offset(RX, 0, st.vfo_a_hz - st.center_hz),
                p::if_offset(RX, 1, st.vfo_b_hz - st.center_hz)
            ));
        }
        "dds" => p::dds(RX, st.center_hz),
        "modulation" => p::modulation(RX, p::mode_to_tci(st.mode)),
        "split_enable" => p::split_enable(RX, st.split),
        "trx" => p::trx(RX, st.ptt, false),
        "tune" => p::tune(RX, st.tune),
        _ => return None,
    })
}

/// `rx_smeter:<trx>,<channel>,<dBm>;`, for clients that asked for receive
/// sensors.
pub(crate) fn rx_smeter(st: &RigState) -> String {
    format!("rx_smeter:{RX},0,{};", st.strength_dbm)
}

/// Status lines describing `next`. With `prev`, only what changed, so an
/// unchanged tick sends nothing; without, everything (the connect burst).
pub(crate) fn diff_lines(prev: Option<&RigState>, next: &RigState) -> Vec<String> {
    let mut out = Vec::new();
    let changed = |f: fn(&RigState) -> f64| prev.is_none_or(|p| f(p) != f(next));
    let flipped = |f: fn(&RigState) -> bool| prev.is_none_or(|p| f(p) != f(next));

    if changed(|s| s.center_hz) {
        out.push(p::dds(RX, next.center_hz));
    }
    if changed(|s| s.vfo_a_hz) || changed(|s| s.center_hz) {
        out.push(p::vfo(RX, 0, next.vfo_a_hz));
        out.push(p::if_offset(RX, 0, next.vfo_a_hz - next.center_hz));
    }
    if changed(|s| s.vfo_b_hz) || changed(|s| s.center_hz) {
        out.push(p::vfo(RX, 1, next.vfo_b_hz));
        out.push(p::if_offset(RX, 1, next.vfo_b_hz - next.center_hz));
    }
    if prev.is_none_or(|p| p.mode != next.mode) {
        out.push(p::modulation(RX, p::mode_to_tci(next.mode)));
    }
    if flipped(|s| s.split) {
        out.push(p::split_enable(RX, next.split));
    }
    if flipped(|s| s.ptt) {
        out.push(p::trx(RX, next.ptt, false));
    }
    if flipped(|s| s.tune) {
        out.push(p::tune(RX, next.tune));
    }
    if prev.is_none_or(|p| p.drive_pct != next.drive_pct) {
        out.push(p::drive(RX, next.drive_pct));
        out.push(p::tune_drive(RX, next.drive_pct));
    }
    if prev.is_none_or(|p| p.iq_rate != next.iq_rate) && next.iq_rate > 0 {
        out.push(p::iq_samplerate(next.iq_rate));
    }
    out
}

/// The connect burst: identity, limits, the state, then `ready;`.
pub(crate) fn connect_burst(device_name: &str, st: &RigState, allow_tx: bool) -> Vec<String> {
    let (lo, hi) =
        st.rx_ranges.iter().fold((f64::MAX, 0.0f64), |(l, h), r| (l.min(r.0), h.max(r.1)));
    let (lo, hi) = if st.rx_ranges.is_empty() { (0.0, 6_000_000_000.0) } else { (lo, hi) };
    let half_span = st.iq_rate as f64 / 2.0;
    let mut out = vec![
        p::protocol_line("hardrock-panel", super::TCI_VERSION),
        p::device(device_name),
        p::receive_only(!(st.can_tx && allow_tx)),
        p::trx_count(1),
        p::channels_count(2),
        p::vfo_limits(lo, hi),
        p::if_limits(-half_span, half_span),
        p::modulations_list(&p::MODULATIONS),
    ];
    if st.iq_rate > 0 {
        out.push(p::iq_samplerate(st.iq_rate));
    }
    out.push(p::audio_samplerate(super::AUDIO_RATE_HZ));
    out.push(p::rx_enable(RX, true));
    out.extend(diff_lines(None, st));
    out.push(p::start().to_string());
    out.push(p::ready().to_string());
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Mode;

    fn snap() -> RigState {
        RigState {
            center_hz: 14_100_000.0,
            vfo_a_hz: 14_074_000.0,
            vfo_b_hz: 14_080_000.0,
            mode: Mode::Usb,
            ..RigState::default()
        }
    }

    fn tr(line: &str) -> Option<ClientReq> {
        let parsed = p::parse_status(line);
        let (c, a) = parsed.first()?;
        translate(c, a, &snap())
    }

    #[test]
    fn tuning_commands() {
        assert_eq!(tr("vfo:0,0,7100000;"), Some(ClientReq::SetFreq(Vfo::A, 7_100_000.0)));
        assert_eq!(tr("vfo:0,1,7105000;"), Some(ClientReq::SetFreq(Vfo::B, 7_105_000.0)));
        assert_eq!(tr("dds:0,14100000;"), Some(ClientReq::SetCenter(14_100_000.0)));
        assert_eq!(tr("if:0,0,-26000;"), Some(ClientReq::SetFreq(Vfo::A, 14_074_000.0)));
        assert_eq!(tr("modulation:0,digu;"), Some(ClientReq::SetMode(Mode::DataU)));
        assert_eq!(tr("split_enable:0,true;"), Some(ClientReq::SetSplit(true)));
    }

    #[test]
    fn transmit_commands() {
        assert_eq!(tr("trx:0,true,tci;"), Some(ClientReq::Key { on: true, tci_audio: true }));
        assert_eq!(tr("trx:0,true;"), Some(ClientReq::Key { on: true, tci_audio: false }));
        assert_eq!(tr("trx:0,false;"), Some(ClientReq::Key { on: false, tci_audio: false }));
        assert_eq!(tr("tune:0,true;"), Some(ClientReq::Tune(true)));
        // The power is not the client's to set.
        assert_eq!(tr("drive:0,100;"), Some(ClientReq::Ignored));
        assert_eq!(truth_for("drive", &snap()), Some("drive:0,0;".into()));
    }

    #[test]
    fn stream_commands() {
        assert_eq!(tr("iq_start:0;"), Some(ClientReq::Iq(true)));
        assert_eq!(tr("iq_stop:0;"), Some(ClientReq::Iq(false)));
        assert_eq!(tr("iq_samplerate:192000;"), Some(ClientReq::IqRate(192_000)));
        assert_eq!(tr("audio_start:0;"), Some(ClientReq::Audio(true)));
        assert_eq!(tr("iq_start:1;"), Some(ClientReq::Ignored));
        assert_eq!(tr("rx_enable:0,false;"), Some(ClientReq::Ignored));
    }

    #[test]
    fn junk_is_rejected() {
        assert_eq!(tr("bogus:1,2,3;"), None);
        assert_eq!(tr("vfo:0;"), None);
        assert_eq!(tr("vfo:0,7,7100000;"), None);
        assert_eq!(tr("vfo:0,0,nan;"), None);
        assert_eq!(tr("modulation:0,zzz;"), None);
        assert_eq!(tr("trx:0,maybe;"), None);
    }

    #[test]
    fn diff_emits_only_changes() {
        let a = snap();
        assert!(diff_lines(Some(&a), &a).is_empty());
        let mut b = a.clone();
        b.vfo_a_hz = 14_075_000.0;
        assert_eq!(diff_lines(Some(&a), &b), vec!["vfo:0,0,14075000;", "if:0,0,-25000;"]);
        let mut c = a.clone();
        c.ptt = true;
        assert_eq!(diff_lines(Some(&a), &c), vec!["trx:0,true;"]);
        assert!(diff_lines(None, &a).len() > 5);
    }

    #[test]
    fn the_burst_ends_with_ready_and_says_receive_only_unless_transmit_is_allowed() {
        let mut s = snap();
        s.can_tx = true;
        let burst = connect_burst("DUO", &s, false);
        assert_eq!(burst.last().map(String::as_str), Some("ready;"));
        assert!(burst.iter().any(|l| l == "device:DUO;"));
        assert!(burst.iter().any(|l| l == "receive_only:true;"));
        assert!(!burst.iter().any(|l| l.starts_with("iq_samplerate:")));
        assert!(connect_burst("DUO", &s, true).iter().any(|l| l == "receive_only:false;"));
        s.iq_rate = 192_000;
        let burst = connect_burst("DUO", &s, true);
        assert!(burst.iter().any(|l| l == "iq_samplerate:192000;"));
        assert!(burst.iter().any(|l| l == "if_limits:-96000,96000;"));
    }
}
