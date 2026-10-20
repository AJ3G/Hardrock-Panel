// Ported from SDRoxide (https://github.com/dividebysandwich/sdroxide),
// crates/sdroxide-rigctld/src/proto.rs and state.rs, GPL-3.0-or-later. Cut
// down to what the panel's radio offers, and answering from the `Rig` trait
// instead of SDRoxide's engine commands.

//! The rigctld wire protocol, as functions of a line and a [`Rig`].
//!
//! Every compatibility hazard lives here, so none of it needs a socket to
//! test. The reference is Hamlib's own `tests/rigctl_parse.c` (the writer)
//! and `rigs/dummy/netrigctl.c` (the reader every "NET rigctl" client runs).
//!
//! **Response shapes.** In the default protocol a *set* replies `RPRT 0` and a
//! *get* replies only its values, one per line. Any failure replies
//! `RPRT <negative errno>`. In the extended protocol, selected per command by
//! a leading punctuation character, the command is echoed with its long name,
//! every value is labelled, and the block always ends `RPRT n`.

use super::RigctldConfig;
use crate::{Mode, PttKind, PttRequest, PttSource, Rig, RigState, Vfo};

/// Hamlib error codes (`rig.h`), returned negated in `RPRT`.
const OK: i32 = 0;
const EINVAL: i32 = -1;
const ENIMPL: i32 = -4;
const ERJCTED: i32 = -9;
const ENAVAIL: i32 = -11;

/// Hamlib's `RIG_MODE_*` bits (`rig.h`).
mod rig_mode {
    pub const AM: u64 = 0x1;
    pub const CW: u64 = 0x2;
    pub const USB: u64 = 0x4;
    pub const LSB: u64 = 0x8;
    pub const RTTY: u64 = 0x10;
    pub const FM: u64 = 0x20;
    pub const WFM: u64 = 0x40;
    pub const CWR: u64 = 0x80;
    pub const RTTYR: u64 = 0x100;
    pub const PKTLSB: u64 = 0x400;
    pub const PKTUSB: u64 = 0x800;
}

/// Every mode we accept, as one bitmask.
const MODE_MASK: u64 = rig_mode::AM
    | rig_mode::CW
    | rig_mode::USB
    | rig_mode::LSB
    | rig_mode::RTTY
    | rig_mode::FM
    | rig_mode::WFM
    | rig_mode::CWR
    | rig_mode::RTTYR
    | rig_mode::PKTLSB
    | rig_mode::PKTUSB;

/// `RIG_VFO_A | RIG_VFO_B`.
const VFO_MASK: u32 = 0x3;

/// `RIG_LEVEL_STRENGTH`, the only level we report. A client consults the
/// advertised mask and does not ask for a level it thinks we lack.
const LEVEL_STRENGTH: u64 = 1 << 30;
/// `RIG_LEVEL_RFPOWER`, reported but never set from outside.
const LEVEL_RFPOWER: u64 = 1 << 12;

/// `RIG_OP_TOGGLE` and `RIG_OP_TUNE`: the VFO operations we implement.
const OP_TUNE: u32 = 1 << 11;
const OP_TOGGLE: u32 = 1 << 12;

/// The Hamlib name we report for a mode.
pub fn to_hamlib_mode(m: Mode) -> &'static str {
    match m {
        Mode::Lsb => "LSB",
        Mode::Usb => "USB",
        Mode::Cw => "CW",
        Mode::CwR => "CWR",
        Mode::Am => "AM",
        Mode::Fm => "FM",
        Mode::WideFm => "WFM",
        Mode::DataU => "PKTUSB",
        Mode::DataL => "PKTLSB",
        Mode::Rtty => "RTTY",
        Mode::RttyR => "RTTYR",
    }
}

/// Parse a Hamlib mode name. `DIGU`, `DATA-U` and the like are accepted
/// because operators type them, but never sent: Hamlib has no such names.
pub fn from_hamlib_mode(s: &str) -> Option<Mode> {
    Some(match s.to_ascii_uppercase().as_str() {
        "USB" => Mode::Usb,
        "LSB" => Mode::Lsb,
        "CW" => Mode::Cw,
        "CWR" => Mode::CwR,
        "AM" | "AMN" => Mode::Am,
        "FM" | "FMN" | "PKTFM" => Mode::Fm,
        "WFM" => Mode::WideFm,
        "RTTY" => Mode::Rtty,
        "RTTYR" => Mode::RttyR,
        "PKTUSB" | "DIGU" | "DATA-U" | "DATA_U" => Mode::DataU,
        "PKTLSB" | "DIGL" | "DATA-L" | "DATA_L" => Mode::DataL,
        _ => return None,
    })
}

/// What one connection remembers between commands.
#[derive(Debug, Clone, Default)]
pub struct Session {
    /// This connection's id, which its transmit requests carry.
    pub client: u64,
    /// Set by `\set_vfo_opt 1`: every command then carries a VFO argument.
    pub vfo_opt: bool,
    /// Whether this client keyed the radio and has not unkeyed it, so a
    /// disconnect can unkey it.
    pub keyed: bool,
}

/// What the caller should do after handling a line.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// Write this back (already newline-terminated).
    Send(String),
    /// The client said `q`: close without answering.
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Format {
    extended: bool,
    sep: char,
}

impl Format {
    const PLAIN: Format = Format { extended: false, sep: '\n' };
}

/// Split off a leading response-separator character. `+` selects the
/// extended protocol with newlines; other punctuation except `\ _ # ( ) ?`
/// (which begin real commands) selects it with that separator.
fn split_format(line: &str) -> (Format, &str) {
    let mut chars = line.chars();
    match chars.next() {
        Some('+') => (Format { extended: true, sep: '\n' }, chars.as_str()),
        Some(c) if c.is_ascii_punctuation() && !matches!(c, '\\' | '_' | '#' | '(' | ')' | '?') => {
            (Format { extended: true, sep: c }, chars.as_str())
        }
        _ => (Format::PLAIN, line),
    }
}

fn long_name(short: &str) -> Option<&'static str> {
    Some(match short {
        "f" => "get_freq",
        "F" => "set_freq",
        "m" => "get_mode",
        "M" => "set_mode",
        "t" => "get_ptt",
        "T" => "set_ptt",
        "v" => "get_vfo",
        "V" => "set_vfo",
        "s" => "get_split_vfo",
        "S" => "set_split_vfo",
        "i" => "get_split_freq",
        "I" => "set_split_freq",
        "x" => "get_split_mode",
        "X" => "set_split_mode",
        "l" => "get_level",
        "L" => "set_level",
        "G" => "vfo_op",
        "_" => "get_info",
        "1" => "dump_caps",
        "q" | "Q" => "quit",
        _ => return None,
    })
}

fn parse_vfo(s: &str) -> Option<Vfo> {
    match s.to_ascii_uppercase().as_str() {
        "VFOA" | "MAIN" | "A" => Some(Vfo::A),
        "VFOB" | "SUB" | "B" => Some(Vfo::B),
        _ => None,
    }
}

fn vfo_name(v: Vfo) -> &'static str {
    match v {
        Vfo::A => "VFOA",
        Vfo::B => "VFOB",
    }
}

/// Builds a reply in whichever protocol the command selected.
struct Out {
    fmt: Format,
    body: String,
}

impl Out {
    fn new(fmt: Format, long: &str, echo_args: &str) -> Self {
        let mut body = String::new();
        if fmt.extended {
            body.push_str(long);
            body.push(':');
            if !echo_args.is_empty() {
                body.push(' ');
                body.push_str(echo_args);
            }
            body.push(fmt.sep);
        }
        Out { fmt, body }
    }

    fn value(&mut self, label: &str, v: impl std::fmt::Display) {
        if self.fmt.extended {
            self.body.push_str(&format!("{label}: {v}{}", self.fmt.sep));
        } else {
            self.body.push_str(&format!("{v}\n"));
        }
    }

    /// Finish with a return code. The default protocol prints `RPRT` only for
    /// sets and failures; the extended protocol always does.
    fn finish(mut self, code: i32, is_get: bool) -> Reply {
        if self.fmt.extended || code != OK || !is_get {
            self.body.push_str(&format!("RPRT {code}\n"));
        }
        Reply::Send(self.body)
    }

    /// Finish a set from what the rig answered.
    fn result(self, r: Result<(), crate::Refused>) -> Reply {
        match r {
            Ok(()) => self.finish(OK, false),
            Err(why) => {
                tracing::debug!(%why, "rigctld: refused");
                self.finish(ERJCTED, false)
            }
        }
    }
}

/// Handle one received line.
pub fn handle(line: &str, rig: &dyn Rig, cfg: &RigctldConfig, sess: &mut Session) -> Reply {
    let line = line.trim_end_matches(['\r', '\n']);
    let (fmt, rest) = split_format(line);
    let rest = rest.trim();
    if rest.is_empty() {
        return Reply::Send(String::new());
    }

    let mut tokens = rest.split_whitespace();
    let head = tokens.next().unwrap_or_default();
    let long = if let Some(stripped) = head.strip_prefix('\\') {
        stripped.to_string()
    } else if let Some(l) = long_name(head) {
        l.to_string()
    } else {
        // An unknown head gets an error, not silence: a client that gets no
        // answer blocks until it times out.
        return Out::new(fmt, head, "").finish(EINVAL, false);
    };

    let st = rig.state();
    let mut args: Vec<&str> = tokens.collect();
    // In VFO mode every command carries a leading VFO token. Accept one even
    // when not in VFO mode: clients send it anyway.
    let mut vfo = st.active_vfo;
    if !args.is_empty() && long != "set_vfo" && long != "get_vfo_info" {
        if let Some(v) = parse_vfo(args[0]) {
            vfo = v;
            args.remove(0);
        } else if args[0].eq_ignore_ascii_case("currVFO") || args[0].eq_ignore_ascii_case("VFO") {
            args.remove(0);
        }
    }
    let echo = args.join(" ");
    dispatch(&long, &args, &echo, fmt, vfo, &st, rig, cfg, sess)
}

#[allow(clippy::too_many_arguments)]
fn dispatch(
    long: &str,
    args: &[&str],
    echo: &str,
    fmt: Format,
    vfo: Vfo,
    st: &RigState,
    rig: &dyn Rig,
    cfg: &RigctldConfig,
    sess: &mut Session,
) -> Reply {
    let mut out = Out::new(fmt, long, echo);
    // `nan` and `inf` parse; neither may reach the radio.
    let num = |i: usize| args.get(i).and_then(|s| s.parse::<f64>().ok()).filter(|v| v.is_finite());
    let int = |i: usize| args.get(i).and_then(|s| s.parse::<i32>().ok());
    let source = PttSource::Rigctld { client: sess.client };

    match long {
        "quit" => Reply::Close,

        "get_freq" => {
            out.value("Frequency", st.freq(vfo).round() as i64);
            out.finish(OK, true)
        }
        "set_freq" => match num(0) {
            Some(hz) if hz >= 0.0 => out.result(rig.set_freq(vfo, hz)),
            _ => out.finish(EINVAL, false),
        },

        "get_mode" => {
            out.value("Mode", to_hamlib_mode(st.mode));
            out.value("Passband", st.passband_hz());
            out.finish(OK, true)
        }
        "set_mode" | "set_split_mode" => set_mode(args, st, rig, out),

        "get_ptt" => {
            out.value("PTT", i32::from(st.ptt || st.tune));
            out.finish(OK, true)
        }
        "set_ptt" => match int(0) {
            Some(0) => {
                // Unkeying is never refused, whoever keyed.
                sess.keyed = false;
                rig.release_ptt(source);
                out.finish(OK, false)
            }
            Some(_) => {
                // SAFETY INTERLOCK: transmit from rigctld only when the server
                // allows it and the panel grants it (safety interlock #8).
                if !cfg.allow_tx || !st.can_tx {
                    return out.finish(ERJCTED, false);
                }
                match rig.request_ptt(PttRequest { source, kind: PttKind::RigAudio }) {
                    Ok(()) => {
                        sess.keyed = true;
                        out.finish(OK, false)
                    }
                    Err(why) => {
                        tracing::info!(%why, "rigctld: transmit refused");
                        out.finish(ERJCTED, false)
                    }
                }
            }
            None => out.finish(EINVAL, false),
        },

        "get_vfo" => {
            out.value("VFO", vfo_name(st.active_vfo));
            out.finish(OK, true)
        }
        "set_vfo" => match args.first().and_then(|s| parse_vfo(s)) {
            // Selecting the VFO already selected changes nothing, and must
            // succeed on a radio that cannot select at all.
            Some(v) if v == st.active_vfo => out.finish(OK, false),
            Some(v) => out.result(rig.select_vfo(v)),
            None => out.finish(EINVAL, false),
        },
        "get_split_vfo" => {
            out.value("Split", i32::from(st.split));
            out.value("TX VFO", vfo_name(st.tx_vfo()));
            out.finish(OK, true)
        }
        "set_split_vfo" => match int(0) {
            Some(v) if (v != 0) == st.split => out.finish(OK, false),
            Some(v) => out.result(rig.set_split(v != 0)),
            None => out.finish(EINVAL, false),
        },
        "get_split_freq" => {
            out.value("TX Frequency", st.freq(st.tx_vfo()).round() as i64);
            out.finish(OK, true)
        }
        "set_split_freq" => match num(0) {
            Some(hz) if hz >= 0.0 => out.result(rig.set_freq(st.tx_vfo(), hz)),
            _ => out.finish(EINVAL, false),
        },
        "get_split_mode" => {
            out.value("TX Mode", to_hamlib_mode(st.mode));
            out.value("TX Passband", st.passband_hz());
            out.finish(OK, true)
        }

        "get_level" => match args.first().map(|s| s.to_ascii_uppercase()) {
            Some(l) if l == "STRENGTH" => {
                // Hamlib's STRENGTH is dB relative to S9 (−73 dBm).
                out.value("Level Value", st.strength_dbm + 73);
                out.finish(OK, true)
            }
            Some(l) if l == "RFPOWER" => {
                out.value("Level Value", format!("{:.6}", st.drive_pct.min(100) as f64 / 100.0));
                out.finish(OK, true)
            }
            Some(_) => out.finish(ENAVAIL, true),
            None => out.finish(EINVAL, true),
        },
        // The transmit power stays the panel's to set: an outside program
        // raising it could overdrive the amplifier.
        "set_level" => out.finish(ENAVAIL, false),

        "vfo_op" => match args.first().map(|s| s.to_ascii_uppercase()).unwrap_or_default().as_str()
        {
            "TOGGLE" => out.result(rig.select_vfo(st.active_vfo.other())),
            "TUNE" => {
                // SAFETY INTERLOCK: as `set_ptt`; a tuning carrier is a
                // transmission (safety interlock #8).
                if !cfg.allow_tx || !st.can_tx {
                    return out.finish(ERJCTED, false);
                }
                match rig.request_ptt(PttRequest { source, kind: PttKind::Tune }) {
                    Ok(()) => {
                        sess.keyed = true;
                        out.finish(OK, false)
                    }
                    Err(why) => {
                        tracing::info!(%why, "rigctld: tune refused");
                        out.finish(ERJCTED, false)
                    }
                }
            }
            "" => out.finish(EINVAL, false),
            _ => out.finish(ENAVAIL, false),
        },

        "get_info" => {
            out.value("Info", format!("{} {}", cfg.rig_name, env!("CARGO_PKG_VERSION")));
            out.finish(OK, true)
        }
        "dump_caps" => {
            out.value("Caps", format!("{} rigctld-compatible", cfg.rig_name));
            out.finish(OK, true)
        }
        "dump_state" => Reply::Send(dump_state(st, cfg)),
        "chk_vfo" => {
            // A bare integer: netrigctl reads it with `sscanf("%d")`.
            if fmt.extended {
                Reply::Send(format!("ChkVFO: {}{}\n", i32::from(sess.vfo_opt), fmt.sep))
            } else {
                Reply::Send(format!("{}\n", i32::from(sess.vfo_opt)))
            }
        }
        "set_vfo_opt" => match int(0) {
            Some(v) => {
                sess.vfo_opt = v != 0;
                out.finish(OK, false)
            }
            None => out.finish(EINVAL, false),
        },
        "get_powerstat" => {
            out.value("Power Status", 1);
            out.finish(OK, true)
        }
        "set_powerstat" => match int(0) {
            Some(1) => out.finish(OK, false),
            Some(_) => out.finish(ENIMPL, false),
            None => out.finish(EINVAL, false),
        },
        "get_vfo_info" => {
            let v = args.first().and_then(|s| parse_vfo(s)).unwrap_or(st.active_vfo);
            out.value("Freq", st.freq(v).round() as i64);
            out.value("Mode", to_hamlib_mode(st.mode));
            out.value("Width", st.passband_hz());
            out.value("Split", i32::from(st.split));
            out.value("SatMode", 0);
            out.finish(OK, true)
        }
        "get_rig_info" => Reply::Send(format!(
            "VFO={} Freq={} Mode={} Width={} RX=1 TX={}\nSplit={} SatMode=0\nRig={} Version={} App=hardrock-panel\n",
            vfo_name(st.active_vfo),
            st.freq(st.active_vfo).round() as i64,
            to_hamlib_mode(st.mode),
            st.passband_hz(),
            i32::from(st.ptt || st.tune),
            i32::from(st.split),
            cfg.rig_name,
            env!("CARGO_PKG_VERSION"),
        )),
        "get_lock_mode" => {
            out.value("Locked", 0);
            out.finish(OK, true)
        }
        "set_lock_mode" => out.finish(OK, false),

        _ => out.finish(EINVAL, false),
    }
}

/// `set_mode` / `set_split_mode`.
///
/// WSJT-X reads the mode and re-sends what it read every few seconds, so a
/// request for the mode already running is a success that changes nothing,
/// unless it carries a new width (`M USB 2400` is how rigctl narrows the
/// filter). A width of 0 means "leave the filter alone".
fn set_mode(args: &[&str], st: &RigState, rig: &dyn Rig, out: Out) -> Reply {
    let Some(name) = args.first() else { return out.finish(EINVAL, false) };
    let Some(mode) = from_hamlib_mode(name) else { return out.finish(EINVAL, false) };
    let width = args.get(1).and_then(|s| s.parse::<i32>().ok()).filter(|w| *w > 0);
    let width = width.map(|w| w as u32);
    if mode == st.mode {
        return match width {
            Some(w) if w as i32 != st.passband_hz() => out.result(rig.set_mode(mode, Some(w))),
            _ => out.finish(OK, false),
        };
    }
    out.result(rig.set_mode(mode, width))
}

/// One `start end 0xmodes low_pwr high_pwr 0xvfo 0xant` range line. Hamlib
/// reads these with `%lf %lf %llx %d %d %x %x`, so the field types matter.
fn range_line(lo: f64, hi: f64, tx: bool) -> String {
    let (low_pwr, high_pwr) = if tx { (1, 100) } else { (-1, -1) };
    format!("{lo:.6} {hi:.6} 0x{MODE_MASK:x} {low_pwr} {high_pwr} 0x{VFO_MASK:x} 0x0\n")
}

/// The `\dump_state` block every Hamlib "NET rigctl" client parses on connect.
///
/// * The RX and TX sentinels need exactly seven integers, the tuning-step and
///   filter sentinels exactly two; Hamlib fails the connection otherwise.
/// * The RX range list must not be empty, or Hamlib refuses every `set_mode`.
/// * Preamp and attenuator lines must be present even when empty.
/// * Protocol version 1 must end with `done`, or the client waits for its
///   timeout ("rig control read failed").
fn dump_state(st: &RigState, cfg: &RigctldConfig) -> String {
    let mut s = String::new();
    s.push_str("1\n2\n0\n");

    let rx: Vec<(f64, f64)> =
        if st.rx_ranges.is_empty() { vec![(0.0, 6_000_000_000.0)] } else { st.rx_ranges.clone() };
    for (lo, hi) in &rx {
        s.push_str(&range_line(*lo, *hi, false));
    }
    s.push_str("0 0 0 0 0 0 0\n");

    // TX ranges: empty when transmit is off, which makes Hamlib itself
    // refuse to key rather than ask and be refused.
    let tx_on = st.can_tx && cfg.allow_tx;
    if tx_on {
        let tx: Vec<(f64, f64)> =
            if st.tx_ranges.is_empty() { rx.clone() } else { st.tx_ranges.clone() };
        for (lo, hi) in &tx {
            s.push_str(&range_line(*lo, *hi, true));
        }
    }
    s.push_str("0 0 0 0 0 0 0\n");

    s.push_str(&format!("0x{MODE_MASK:x} 1\n"));
    s.push_str("0 0\n");

    s.push_str(&format!("0x{:x} 2700\n", rig_mode::USB | rig_mode::LSB));
    s.push_str(&format!("0x{:x} 500\n", rig_mode::CW | rig_mode::CWR));
    s.push_str(&format!("0x{:x} 6000\n", rig_mode::AM));
    s.push_str(&format!("0x{:x} 12000\n", rig_mode::FM));
    s.push_str(&format!("0x{:x} 3000\n", rig_mode::PKTUSB | rig_mode::PKTLSB));
    s.push_str(&format!("0x{:x} 500\n", rig_mode::RTTY | rig_mode::RTTYR));
    s.push_str("0 0\n");

    s.push_str("0\n"); // max_rit
    s.push_str("0\n"); // max_xit
    s.push_str("0\n"); // max_ifshift
    s.push_str("0\n"); // announces
    s.push_str("0\n"); // preamp list
    s.push_str("0\n"); // attenuator list

    // get_func, set_func, get_level, set_level, get_parm, set_parm.
    s.push_str("0x0\n0x0\n");
    s.push_str(&format!("0x{:x}\n0x0\n", LEVEL_STRENGTH | LEVEL_RFPOWER));
    s.push_str("0x0\n0x0\n");

    let ops = if tx_on { OP_TOGGLE | OP_TUNE } else { OP_TOGGLE };
    s.push_str(&format!("vfo_ops=0x{ops:x}\n"));
    s.push_str(&format!("ptt_type={}\n", if tx_on { "0x1" } else { "0x0" }));
    s.push_str("targetable_vfo=0x1\n");
    s.push_str("has_set_vfo=1\n");
    s.push_str("has_get_vfo=1\n");
    s.push_str("has_set_freq=1\n");
    s.push_str("has_get_freq=1\n");
    s.push_str("has_set_conf=0\n");
    s.push_str("has_get_conf=0\n");
    s.push_str("has_power2mW=0\n");
    s.push_str("has_mW2power=0\n");
    s.push_str("has_get_ant=0\n");
    s.push_str("has_set_ant=0\n");
    s.push_str("timeout=2000\n");
    s.push_str("rig_model=2\n");
    s.push_str(&format!("rigctld_version={} {}\n", cfg.rig_name, env!("CARGO_PKG_VERSION")));
    s.push_str("done\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testrig::TestRig;
    use std::sync::atomic::Ordering;

    fn rig() -> TestRig {
        TestRig::new(RigState {
            vfo_a_hz: 14_074_000.0,
            vfo_b_hz: 7_074_000.0,
            mode: Mode::Usb,
            passband: (150.0, 2850.0),
            can_tx: true,
            ..RigState::default()
        })
    }

    fn cfg_tx() -> RigctldConfig {
        RigctldConfig { allow_tx: true, ..RigctldConfig::default() }
    }

    fn run(line: &str, r: &TestRig, cfg: &RigctldConfig) -> String {
        let mut sess = Session::default();
        match handle(line, r, cfg, &mut sess) {
            Reply::Send(t) => t,
            Reply::Close => "<close>".into(),
        }
    }

    #[test]
    fn a_plain_get_returns_only_the_value() {
        let r = rig();
        assert_eq!(run("f", &r, &cfg_tx()), "14074000\n");
        assert_eq!(run("m", &r, &cfg_tx()), "USB\n2700\n");
        assert_eq!(run("f VFOB", &r, &cfg_tx()), "7074000\n");
    }

    #[test]
    fn the_extended_protocol_labels_and_ends_with_rprt() {
        let r = rig();
        assert_eq!(run("+f", &r, &cfg_tx()), "get_freq:\nFrequency: 14074000\nRPRT 0\n");
        assert_eq!(run(";\\get_freq", &r, &cfg_tx()), "get_freq:;Frequency: 14074000;RPRT 0\n");
    }

    #[test]
    fn sets_reach_the_rig() {
        let r = rig();
        assert_eq!(run("F 7100000", &r, &cfg_tx()), "RPRT 0\n");
        assert_eq!(run("M PKTUSB 0", &r, &cfg_tx()), "RPRT 0\n");
        assert_eq!(run("S 1 VFOB", &r, &cfg_tx()), "RPRT 0\n");
        assert_eq!(r.log(), vec!["freq A 7100000", "mode DataU None", "split true"]);
        // `nan` must never reach the radio.
        assert_eq!(run("F nan", &r, &cfg_tx()), "RPRT -1\n");
        assert_eq!(run("bogus", &r, &cfg_tx()), "RPRT -1\n");
    }

    /// WSJT-X re-sends the mode it read every few seconds; that must change
    /// nothing, while a new width on the same mode changes the filter.
    #[test]
    fn the_mode_already_running_changes_nothing() {
        let r = rig();
        assert_eq!(run("M USB 2700", &r, &cfg_tx()), "RPRT 0\n");
        assert_eq!(run("M USB 0", &r, &cfg_tx()), "RPRT 0\n");
        assert!(r.log().is_empty());
        assert_eq!(run("M USB 2400", &r, &cfg_tx()), "RPRT 0\n");
        assert_eq!(r.log(), vec!["mode Usb Some(2400)"]);
    }

    #[test]
    fn transmit_is_refused_unless_the_server_allows_it_and_the_panel_grants_it() {
        let r = rig();
        r.allow_ptt.store(true, Ordering::SeqCst);
        // allow_tx off: refused without asking the panel.
        assert_eq!(run("T 1", &r, &RigctldConfig::default()), "RPRT -9\n");
        assert!(r.log().is_empty());
        assert!(!dump_state(&r.state(), &RigctldConfig::default()).contains("ptt_type=0x1"));

        // allow_tx on but the panel says no.
        r.allow_ptt.store(false, Ordering::SeqCst);
        let mut sess = Session { client: 3, ..Session::default() };
        assert_eq!(handle("T 1", &r, &cfg_tx(), &mut sess), Reply::Send("RPRT -9\n".into()));
        assert!(!sess.keyed);
        assert_eq!(r.log(), vec!["ptt Rigctld { client: 3 } RigAudio"]);

        // The panel says yes.
        r.allow_ptt.store(true, Ordering::SeqCst);
        assert_eq!(handle("T 1", &r, &cfg_tx(), &mut sess), Reply::Send("RPRT 0\n".into()));
        assert!(sess.keyed);
        assert_eq!(run("t", &r, &cfg_tx()), "1\n");

        // Unkeying always goes through, even with transmit switched off.
        assert_eq!(
            handle("T 0", &r, &RigctldConfig::default(), &mut sess),
            Reply::Send("RPRT 0\n".into())
        );
        assert!(!sess.keyed);
        assert_eq!(r.log().last().unwrap(), "unptt Rigctld { client: 3 }");
    }

    #[test]
    fn a_receive_only_radio_never_asks_the_panel_to_key() {
        let r = rig();
        r.st.lock().unwrap().can_tx = false;
        r.allow_ptt.store(true, Ordering::SeqCst);
        assert_eq!(run("T 1", &r, &cfg_tx()), "RPRT -9\n");
        assert_eq!(run("G TUNE", &r, &cfg_tx()), "RPRT -9\n");
        assert!(r.log().is_empty());
    }

    #[test]
    fn the_transmit_power_cannot_be_set_from_outside() {
        let r = rig();
        assert_eq!(run("L RFPOWER 1.0", &r, &cfg_tx()), "RPRT -11\n");
    }

    #[test]
    fn dump_state_has_the_shape_hamlib_checks() {
        let r = rig();
        let d = dump_state(&r.state(), &cfg_tx());
        assert!(d.starts_with("1\n2\n0\n"));
        assert!(d.ends_with("done\n"));
        let sentinels = d.lines().filter(|l| *l == "0 0 0 0 0 0 0").count();
        assert_eq!(sentinels, 2);
        assert!(d.lines().filter(|l| *l == "0 0").count() >= 2);
        assert!(d.contains("ptt_type=0x1"));
        // Every range line has seven fields.
        for l in d.lines().filter(|l| l.contains(" 0x3 0x0")) {
            assert_eq!(l.split_whitespace().count(), 7, "{l}");
        }
    }

    #[test]
    fn every_mode_reports_a_name_it_parses_back() {
        for m in Mode::ALL {
            assert_eq!(from_hamlib_mode(to_hamlib_mode(m)), Some(m));
        }
    }

    #[test]
    fn quit_closes() {
        let r = rig();
        assert_eq!(run("q", &r, &cfg_tx()), "<close>");
    }
}
