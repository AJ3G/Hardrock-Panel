//! TRANSMIT (AJ3G, 10 Nov 2026): FT8 and WSPR from the panel's own audio,
//! the QSO sequencer, the TX window and its decode list. The plan is
//! notes/tx-plan.md in the project.
//!
//! An over is rendered at the sound output's own rate ([`wave`]), played
//! from silence so its first symbol lands at the slot's start plus 0.5 s
//! (FT8) or 1 s (WSPR), and keyed through the rig link's TUNE path as
//! [`Keying::Data`]: every TUNE interlock (#6, #7, #8) holds for the whole
//! over, the link's own cap (#12) is its backstop, and the panel unkeys at
//! the end of its own message.

pub(crate) mod freq;
pub(crate) mod msg;
pub(crate) mod occupy;
pub(crate) mod qso;
pub(crate) mod sched;
pub(crate) mod wave;
pub(crate) mod window;

pub(crate) use window::window;

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant, UNIX_EPOCH};

use hardrock::AmpStatus;
use rigcat::{Keying, RigLink, RigStatus, Vfo};
use serde::{Deserialize, Serialize};

use crate::audio;
use crate::decode::{TxHeard, WsprHeard};
use crate::station::{self, StationSettings};
use crate::tune;

/// Which mode the TX window sends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) enum TxMode {
    #[default]
    Ft8,
    Wspr,
}

/// Who moves the FT8 transmit audio (the scope's offset bar).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) enum FreqMode {
    /// The panel picks the clearest gap before each over.
    #[default]
    Auto,
    /// The bar goes where it is dragged, or to a decode clicked.
    Manual,
    /// Only a drag or a typed value moves it.
    Hold,
}

impl FreqMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            FreqMode::Auto => "AUTO",
            FreqMode::Manual => "MANUAL",
            FreqMode::Hold => "HOLD",
        }
    }
}

/// How the decode list is sorted, within each turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) enum Sort {
    #[default]
    Time,
    Snr,
    Distance,
    Country,
}

/// A column of the WSPR lists to sort by (decision 90).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
pub(crate) enum WsprKey {
    #[default]
    Utc,
    Snr,
    Call,
    Km,
}

/// How a WSPR list is sorted: a column, and which way. Newest first to
/// start; sorted by anything but UTC, the list drops its 2 minute cycles.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct WsprSort {
    pub key: WsprKey,
    /// Newest, strongest, farthest or Z first.
    pub desc: bool,
}

impl Default for WsprSort {
    fn default() -> Self {
        WsprSort { key: WsprKey::Utc, desc: true }
    }
}

impl WsprSort {
    /// A click on a column's heading: that column, its natural way first
    /// (newest, strongest, farthest, A to Z); again, the other way.
    pub(crate) fn click(&mut self, k: WsprKey) {
        if self.key == k {
            self.desc = !self.desc;
        } else {
            self.key = k;
            self.desc = k != WsprKey::Call;
        }
    }
}

/// The TX window's settings, kept from run to run. ENABLE TX is not one of
/// them: it is off at every start.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct TxSettings {
    pub popped: bool,
    pub mode: TxMode,
    /// Each mode's level: a share of the radio's power setting, 0 to 1, as
    /// the Tune slider is.
    pub ft8_level: f32,
    pub wspr_level: f32,
    pub freq: FreqMode,
    /// The FT8 transmit audio (tone 0) when the scope's bar isn't up, Hz.
    pub audio_hz: f32,
    /// Call CQ in the even (:00, :30) slots.
    pub tx_even: bool,
    pub auto_seq: bool,
    pub call_first: bool,
    pub skip_tx1: bool,
    /// In Wait CQ, call the station as soon as they sign off with another.
    pub call_after_rr73: bool,
    /// Tx4 sends RRR rather than RR73.
    pub rrr: bool,
    /// The CQ template: "CQ {MYCALL} {MYGRID}", or "CQ DX {MYCALL} {MYGRID}".
    pub cq: String,
    /// Tx5's free-text lines, up to 13 characters each.
    pub macros: Vec<String>,
    pub watchdog_min: u32,
    pub max_calls: u32,
    /// WSPR: share of slots, %; power on the air, dBm; a new offset each
    /// slot in 1400 to 1600 Hz, or `wspr_audio_hz`.
    pub wspr_pct: u8,
    pub wspr_dbm: i32,
    pub wspr_roam: bool,
    pub wspr_audio_hz: f32,
    /// The share TRANSMIT went back to OFF from when ENABLE TX was turned
    /// off, for ENABLE TX to bring back (decision 91).
    pub wspr_pct_last: u8,
    /// How HEARD and HEARD ME are sorted (decision 90).
    pub wspr_heard_sort: WsprSort,
    pub wspr_me_sort: WsprSort,
    /// Ask before logging, rather than logging at once.
    pub prompt_log: bool,
    pub sort: Sort,
    /// The transcript's height as last dragged, px.
    pub transcript_h: f32,
    pub cq_only: bool,
    pub new_only: bool,
}

impl Default for TxSettings {
    fn default() -> Self {
        TxSettings {
            popped: false,
            mode: TxMode::Ft8,
            ft8_level: 0.5,
            wspr_level: 0.5,
            freq: FreqMode::Auto,
            audio_hz: 1500.0,
            tx_even: false,
            auto_seq: true,
            call_first: true,
            skip_tx1: false,
            call_after_rr73: true,
            rrr: false,
            cq: "CQ {MYCALL} {MYGRID}".into(),
            macros: vec!["TNX 73 GL".into(), "QRZ?".into(), "TU 73".into()],
            watchdog_min: 6,
            max_calls: 10,
            wspr_pct: 20,
            wspr_dbm: 37,
            wspr_roam: true,
            wspr_audio_hz: 1500.0,
            wspr_pct_last: 20,
            wspr_heard_sort: WsprSort::default(),
            wspr_me_sort: WsprSort::default(),
            prompt_log: false,
            sort: Sort::Time,
            transcript_h: 250.0,
            cq_only: false,
            new_only: false,
        }
    }
}

impl TxSettings {
    pub(crate) fn level(&self, mode: TxMode) -> f32 {
        match mode {
            TxMode::Ft8 => self.ft8_level,
            TxMode::Wspr => self.wspr_level,
        }
    }
}

/// What the log says of a station, for the list's badges.
#[derive(Default, Clone, Copy, Debug, PartialEq)]
pub(crate) struct Badges {
    pub dupe: bool,
    pub new_call: bool,
    /// The country was worked, but not on this band.
    pub new_band: bool,
    pub new_dxcc: bool,
    pub new_grid: bool,
}

/// A row of the TX window's decode list.
#[derive(Debug, Clone)]
pub(crate) struct ListRow {
    pub d: msg::Decode,
    /// DECODE's row for it.
    pub row: u64,
    pub rf_hz: f64,
    pub country: Option<String>,
    /// The flag its country flies, for [`crate::flags::Flags`]; `""` for
    /// none (decision 105).
    pub flag: &'static str,
    pub continent: Option<&'static str>,
    pub cq_zone: Option<u8>,
    pub itu_zone: Option<u8>,
    /// Heading, °, and distance, km, from our grid; `true` when from their
    /// country's centre rather than their grid (a guess).
    pub path: Option<(f64, f64, bool)>,
    pub badges: Badges,
    /// Addressed to us, or our call anywhere in it.
    pub to_me: bool,
    /// A CQ open to us.
    pub cq_ok: bool,
    /// We sent it (our own over, in the list as WSJT-X shows it).
    pub ours: bool,
}

/// A row of the TX window's WSPR list: a station our WSPR decoder heard.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WsprRow {
    pub h: WsprHeard,
    /// Heading, °, and distance, km, from our grid to theirs.
    pub path: Option<(f64, f64)>,
}

/// An over on the air, or about to be.
struct Over {
    mode: TxMode,
    slot_utc: i64,
    text: String,
    audio_hz: f32,
    /// When the key was asked for, and whether the radio has shown it.
    asked: Instant,
    keyed_seen: bool,
    /// The over's last sample, UTC s.
    end_utc: f64,
}

/// How long the radio is given to show the over keyed: the link reads the
/// radio's input and mode before it keys (#7).
const KEY_WAIT: Duration = Duration::from_secs(3);
/// After the message, before the unkey, s: the sound card's buffer.
const TAIL_S: f64 = 0.25;

/// What the window and the panel around it need from this frame.
#[derive(Default)]
pub(crate) struct TxOut {
    /// Put the scope's bar this far from the receiver's dial, Hz.
    pub set_bar_off: Option<f64>,
    /// How busy AUTO finds each part of the passband (decision 118):
    /// audio Hz from the receiver's dial, from and to, and 0 to 1.
    pub busy: Vec<(f32, f32, f32)>,
    /// The QSO under way, for GLOBE: call, grid, keyed now.
    pub strobe: Option<(String, Option<String>, bool)>,
    /// In split, the VFO B the FT8 overs go from (where it is, or where it
    /// is being moved to), so TX follows RX leaves it there rather than
    /// pulling it back to the receiver (decision 125).
    pub split_b: Option<u64>,
}

/// Everything about the station the TX engine looks at each frame.
pub(crate) struct Env<'a> {
    pub now: f64,
    pub link: Option<&'a RigLink>,
    pub rig: Option<&'a RigStatus>,
    pub amp: &'a AmpStatus,
    /// The sound output that goes to the radio.
    pub device: Option<String>,
    pub outside: &'a mut crate::outside::Outside,
    /// The panel's receiver's dial (radio Hz) and sideband, while it runs.
    pub rx_dial: Option<(f64, bool)>,
    /// The scope's bar, as an offset from the receiver's dial, Hz.
    pub bar_off: Option<f64>,
    /// The bar was dragged by hand since last frame.
    pub bar_moved: bool,
    /// The TX window is open, so the bar is TX's: closed, it is the
    /// operator's measuring cursor and is neither read nor placed.
    pub bar_ours: bool,
    pub station: &'a StationSettings,
    /// DECODE is running FT8.
    pub decoding_ft8: bool,
    /// The last slot DECODE has finished decoding, UTC s.
    pub slot_decoded: Option<i64>,
    /// The panel's receiver's mode, while it runs on the radio's I/Q.
    pub scope_mode: Option<sdr::Mode>,
}

/// The TX engine: the sequencer, the over on the air, and the decode list.
pub(crate) struct Tx {
    /// ENABLE TX: off at every start.
    pub enabled: bool,
    pub qso: qso::Qso,
    player: audio::Player,
    /// The output and level the over plays at, for Now's swap.
    player_device: Option<String>,
    gain: f32,
    over: Option<Over>,
    /// The slot an over last went in, so a slot is used once.
    sent_slot: Option<i64>,
    pub rows: VecDeque<ListRow>,
    /// WSPR's list: the stations heard, newest first.
    pub wspr_rows: VecDeque<WsprRow>,
    /// CLEAR in WSPR: reports of our beacon from before it are not shown,
    /// UTC s.
    pub wspr_cleared: f64,
    /// (slot, audio Hz, SNR dB) of signals heard, for AUTO: kept
    /// [`freq::keep_s`] (decision 115).
    heard: VecDeque<(i64, f32, f32)>,
    /// AUTO's ear for undecoded signal in each slot (decision 115): the
    /// panel puts it on the receiver's audio.
    pub occ: occupy::Occupancy,
    /// Which slots each station was heard in: even or odd.
    parity: HashMap<String, bool>,
    /// The parity of our overs now.
    pub tx_even: bool,
    /// Why nothing is going out, or what last went wrong.
    pub note: Option<String>,
    /// The six message lines, as edited.
    pub lines: [String; 6],
    /// The line picked by hand, repeated each slot while Auto Seq is off.
    pub hand: Option<String>,
    /// The report box: our report of the station, dB.
    pub report: i16,
    /// The AUTO pick, Hz.
    auto_hz: Option<f32>,
    /// The median DT of the last slot's decodes, s.
    pub dt: Option<f32>,
    /// A contact waiting for LOG THIS (prompt to log).
    pub to_log: Option<qso::Logged>,
    /// Contacts to write to the log.
    logged: VecDeque<(qso::Logged, f64)>,
    /// The row picked in the list.
    pub picked: Option<u64>,
    /// The decode list's flag textures (decision 105).
    pub flags: crate::flags::Flags,
    /// The WSPR offset this beacon went at, and whether one has gone.
    wspr_hz: f32,
    wspr_sent: bool,
    /// Seed for ROAM.
    roam: u64,
    /// The dial (radio Hz) the list belongs to: a band change clears it.
    list_dial: Option<f64>,
    /// The station worked, where we last copied them: call, audio Hz and
    /// RF Hz (0 when unknown), from each of their decodes.
    dx_seen: Option<(String, f32, f64)>,
    /// The mode the scope's bar was last placed for, so entering a mode
    /// places it afresh rather than reading it (decision 88).
    bar_mode: Option<TxMode>,
    /// ENABLE TX last frame, to see it turned off in WSPR (decision 91).
    was_enabled: bool,
    /// HALT pressed since the station in hand was picked or ENABLE TX
    /// last turned on: the scope's worked mark is taken away (decision 104).
    halted: bool,
    /// Split (decision 106): VFO B last asked for, and when, so the move
    /// is asked again only if the radio hasn't read it back by then.
    split_sent: Option<(u64, Instant)>,
    /// The slot of an over that was on the air when REPLY left its station
    /// for another (decision 114): its end is not counted against the new
    /// one.
    left_slot: Option<i64>,
    /// The TX audio as this engine last left it, so a value typed, dragged
    /// or clicked in the window since is seen and kept, and the bar moved
    /// to it, rather than the bar's old place read back over it (decision
    /// 126).
    audio_seen: Option<f32>,
    /// The tone is outside what the radio's transmit filter (menu 35 TX BW)
    /// passes cleanly: what to say (decision 125).
    pub filter_note: Option<String>,
}

impl Default for Tx {
    fn default() -> Self {
        Tx {
            enabled: false,
            qso: qso::Qso::new(qso::Config::default()),
            player: audio::Player::default(),
            player_device: None,
            gain: 0.0,
            over: None,
            sent_slot: None,
            rows: VecDeque::new(),
            wspr_rows: VecDeque::new(),
            wspr_cleared: 0.0,
            heard: VecDeque::new(),
            occ: Default::default(),
            parity: HashMap::new(),
            tx_even: false,
            note: None,
            lines: Default::default(),
            hand: None,
            report: -10,
            auto_hz: None,
            dt: None,
            to_log: None,
            logged: VecDeque::new(),
            picked: None,
            flags: Default::default(),
            wspr_hz: 1500.0,
            wspr_sent: false,
            roam: 0x9e37_79b9_7f4a_7c15,
            list_dial: None,
            dx_seen: None,
            bar_mode: None,
            was_enabled: false,
            halted: false,
            split_sent: None,
            audio_seen: None,
            filter_note: None,
            left_slot: None,
        }
    }
}

/// The most rows the list keeps.
const ROWS: usize = 400;

impl Tx {
    /// Keyed now, by this window.
    pub(crate) fn on_air(&self) -> bool {
        self.over.as_ref().is_some_and(|o| o.keyed_seen)
    }

    /// The over going out or about to: its text, mode, audio Hz and end.
    pub(crate) fn over(&self) -> Option<(&str, TxMode, f32, f64)> {
        self.over.as_ref().map(|o| (o.text.as_str(), o.mode, o.audio_hz, o.end_utc))
    }

    /// The samples of the over that have gone to the radio since the last
    /// call (at most the newest `n`), for the scope's TX monitor (decision
    /// 83); `None` when nothing of ours is sounding.
    pub(crate) fn sounding(&self, n: usize) -> Option<audio::TxAudio> {
        self.player.recent(n)
    }

    /// The mode of the over in progress, for the interlocks.
    pub(crate) fn over_mode(&self) -> Option<TxMode> {
        self.over.as_ref().map(|o| o.mode)
    }

    /// The station in hand and where we copy them: call, audio offset Hz
    /// and RF Hz, from their latest decode; for the QSO box (AJ3G, 11 Nov
    /// 2026).
    pub(crate) fn dx_copied(&self) -> Option<(String, f32, f64)> {
        let call = self.qso.dx_call()?;
        self.dx_seen.clone().filter(|(c, ..)| c == call)
    }

    /// The scope's worked-station mark: [`Tx::dx_copied`], only while the
    /// QSO is actually in progress (decision 104). Gone once the contact is
    /// complete and logged, timed out, out of calls, or after HALT; back as
    /// soon as a station is picked (or ENABLE TX resumes the one in hand).
    pub(crate) fn dx_mark(&self) -> Option<(String, f32, f64)> {
        if self.halted || !self.qso.in_progress() {
            return None;
        }
        self.dx_copied()
    }

    /// The offset our WSPR beacon goes out on: the one on the air, else the
    /// one set, else (ROAM) the last one sent.
    pub(crate) fn wspr_offset(&self, s: &TxSettings) -> Option<f32> {
        match self.over.as_ref().filter(|o| o.mode == TxMode::Wspr) {
            Some(o) => Some(o.audio_hz),
            None if !s.wspr_roam => Some(s.wspr_audio_hz),
            None => self.wspr_sent.then_some(self.wspr_hz),
        }
    }

    /// A finished contact for the log, with the dial it was on (radio Hz).
    pub(crate) fn take_logged(&mut self) -> Option<(qso::Logged, f64)> {
        self.logged.pop_front()
    }

    /// The sequencer's settings from the window's and the station's.
    fn configure(&mut self, s: &TxSettings, st: &StationSettings) {
        let c = &mut self.qso.cfg;
        c.my_call = st.call().unwrap_or_default();
        c.my_grid = st.grid().unwrap_or_default();
        c.msg_cq = s.cq.clone();
        c.msg_rr73 = if s.rrr { "{DX} {MYCALL} RRR".into() } else { "{DX} {MYCALL} RR73".into() };
        c.tx_watchdog_min = s.watchdog_min;
        c.max_tx_repeats = s.max_calls;
        c.skip_tx1 = s.skip_tx1;
        c.call_first = s.call_first;
        c.call_after_73 = s.call_after_rr73;
    }

    /// Generate Std Msgs: the six lines for the station in hand.
    pub(crate) fn generate(&mut self, s: &TxSettings) {
        let mut l = self.qso.std_msgs(s.rrr);
        if self.lines[4].trim().is_empty() || self.lines[4] == l[4] {
            l[4] = s.macros.first().cloned().unwrap_or_default();
        } else {
            l[4] = self.lines[4].clone();
        }
        self.lines = l;
    }

    /// REPLY (or a double-click) on a station: work it now. Mid-QSO with
    /// another station, that QSO is stopped as STOP QSO stops it (nothing
    /// logged, not queued again) and this one started at once (decision
    /// 114, AJ3G: "Stop QSO and load the new station"): an over already
    /// on the air finishes, and the next goes to this station. + still
    /// queues ([`Tx::queue`]).
    pub(crate) fn reply(&mut self, s: &TxSettings, r: &ListRow, now: f64) {
        let Some(call) = r.d.from.clone() else { return };
        let wait = !r.d.is_cq && r.d.to.is_some() && !r.to_me;
        let left = self
            .qso
            .dx_call()
            .filter(|c| self.qso.busy() && !c.eq_ignore_ascii_case(&call))
            .map(str::to_string);
        if left.is_some() {
            self.qso.stop();
            // The over on the air was the old station's: its end is not a
            // call to this one.
            self.left_slot = self.over.as_ref().map(|o| o.slot_utc);
        }
        self.tx_even = !sched::is_even(r.d.slot_utc, sched::FT8_PERIOD);
        self.dx_seen = Some((call.clone(), r.d.audio_hz, r.rf_hz));
        self.report = r.d.snr_db;
        self.qso.start_qso(&call, r.d.grid.clone(), r.d.snr_db, wait, now as i64);
        if let Some(old) = left {
            let line = format!("Left {old} for {call}");
            tracing::info!("TX: {line}");
            self.qso.note(line, now as i64);
        }
        self.halted = false;
        self.hand = None;
        self.generate(s);
    }

    /// ENABLE TX on, as its button turns it on: the operator is here, so
    /// the watchdog starts over. REPLY and CALL CQ do this too in FT8
    /// (AJ3G, 11 Nov 2026); every interlock still decides whether an over
    /// keys.
    pub(crate) fn enable(&mut self, s: &mut TxSettings) {
        self.enabled = true;
        self.halted = false;
        self.note = None;
        if s.mode == TxMode::Ft8 {
            self.qso.reset_watchdog();
        }
        // WSPR: TRANSMIT back to the share it had (decision 91).
        if s.mode == TxMode::Wspr && s.wspr_pct == 0 {
            s.wspr_pct = if s.wspr_pct_last == 0 { 20 } else { s.wspr_pct_last };
        }
        self.was_enabled = true;
    }

    /// ENABLE TX off by hand (or by picking TRANSMIT OFF). In WSPR the duty
    /// chips go to OFF, the share kept for ENABLE TX to bring back; HALT,
    /// an interlock or a mode change do the same through `frame`.
    pub(crate) fn disable(&mut self, s: &mut TxSettings) {
        self.enabled = false;
        self.note = None;
        self.duty_off(s);
    }

    /// WSPR's TRANSMIT to OFF, keeping the share it had (decision 91).
    fn duty_off(&mut self, s: &mut TxSettings) {
        if s.mode == TxMode::Wspr && s.wspr_pct != 0 {
            s.wspr_pct_last = s.wspr_pct;
            s.wspr_pct = 0;
        }
        self.was_enabled = false;
    }

    /// TRANSMIT's OFF chip, or a share: OFF also turns ENABLE TX off.
    pub(crate) fn set_duty(&mut self, s: &mut TxSettings, pct: u8) {
        if pct == 0 {
            self.disable(s);
        } else {
            s.wspr_pct = pct;
        }
    }

    /// The LEVEL slider of the over going out, live (decision 87): its
    /// audio follows the slider at once, as TX TUNE's tone follows the Tune
    /// slider, so the drive the interlocks judge (at this same level) is the
    /// drive going out. `ceiling` is the radio's audio ceiling.
    ///
    /// SAFETY INTERLOCK (docs/safety-interlocks.md, #6): called only once
    /// the over has passed the TUNE checks at `level`.
    pub(crate) fn set_level(&mut self, level: f32, ceiling: f32) {
        if self.over.is_none() {
            return;
        }
        let g = audio::gain_for(level, ceiling);
        if g != self.gain {
            self.gain = g;
            self.player.set_gain(g);
        }
    }

    /// + on a station: queue it.
    pub(crate) fn queue(&mut self, r: &ListRow) {
        if let Some(call) = r.d.from.clone() {
            self.qso.queue_add(qso::Queued {
                call,
                grid: r.d.grid.clone(),
                snr_db: r.d.snr_db,
                audio_hz: r.d.audio_hz,
                heard_utc: r.d.slot_utc,
                wait_for_cq: !r.d.is_cq && r.d.to.is_some() && !r.to_me,
            });
        }
    }

    /// CALL CQ (Tx6), in our chosen parity.
    pub(crate) fn call_cq(&mut self, s: &TxSettings) {
        self.tx_even = s.tx_even;
        self.qso.call_cq();
        self.hand = None;
        self.generate(s);
    }

    /// A line's Next (or Now, `now`): Tx1 to Tx4 and Tx6 set the
    /// sequencer's step; Tx5 is sent once. With Auto Seq off, the line is
    /// sent each slot until another is picked. Now, while an over is going
    /// out, changes that over to it.
    pub(crate) fn pick_line(&mut self, s: &TxSettings, i: usize, now_req: bool, now: f64) {
        let text = self.lines[i].trim().to_ascii_uppercase();
        if text.is_empty() {
            return;
        }
        if !s.auto_seq {
            self.hand = Some(text.clone());
        } else {
            use qso::Step;
            let step = [Step::TxGrid, Step::TxReport, Step::TxRReport, Step::TxRr73, Step::Idle, Step::CallingCq][i];
            let stock = self.qso.std_msgs(s.rrr);
            if i == 4 || text != stock[i] || !self.qso.set_step(step) {
                self.qso.queue_text(&text);
            }
            if i == 5 {
                self.tx_even = s.tx_even;
            }
        }
        if now_req {
            self.swap_now(&text, now);
        }
    }

    /// Now on a line while an over is going out: the rest of that over is
    /// the new line.
    fn swap_now(&mut self, text: &str, now: f64) {
        let (Some(o), Some(dev)) = (self.over.as_mut(), self.player_device.clone()) else { return };
        if o.mode != TxMode::Ft8 || !o.keyed_seen || now > o.end_utc - 2.0 {
            return;
        }
        let Some(m) = wave::Message::ft8(text, o.audio_hz) else { return };
        let start = UNIX_EPOCH + Duration::from_secs_f64(o.slot_utc as f64 + wave::FT8_START_S);
        let gain = self.gain;
        if self.player.start(&dev, gain, start, |rate| m.render(rate)).is_ok() {
            o.text = text.to_string();
            self.qso.record_tx(text, now as i64);
        }
    }

    /// HALT TX: unkey now and turn ENABLE TX off.
    pub(crate) fn halt(&mut self, link: Option<&RigLink>) {
        self.enabled = false;
        self.halted = true;
        if self.over.is_some()
            && let Some(l) = link
        {
            l.tune_stop(None);
        }
        self.note = Some("Halted.".into());
    }

    /// Log the contact in hand now (LOG THIS).
    pub(crate) fn log_this(&mut self, now: f64) {
        if let Some(l) = self.to_log.take() {
            self.logged.push_back((l, self.list_dial.unwrap_or(0.0)));
        } else if self.qso.log_by_hand(now as i64) {
            self.take_completed(false);
        }
    }

    fn take_completed(&mut self, prompt: bool) {
        while let Some(l) = self.qso.take_completed() {
            if prompt {
                self.to_log = Some(l);
            } else {
                self.logged.push_back((l, self.list_dial.unwrap_or(0.0)));
            }
        }
    }

    /// The keyed over, while it is keyed (from the panel's `keep_tune`):
    /// why it must stop now, `Some(None)` when its message is done, or
    /// `None` to keep it.
    ///
    /// SAFETY INTERLOCKS (docs/safety-interlocks.md, #6, #7 and #12): the
    /// caller has checked the TUNE interlocks at this mode's level; on top
    /// of those the over stops when its audio stops, and a WSPR beacon when
    /// the amplifier's temperature reaches its warning.
    pub(crate) fn keep(&mut self, now: f64) -> Option<Option<String>> {
        let Some(o) = self.over.as_mut() else {
            return Some(Some("the TX window has no over going out".into()));
        };
        o.keyed_seen = true;
        if !self.player.playing() {
            return Some(Some("the transmit audio stopped".into()));
        }
        // A WSPR beacon already on the air runs to its end whatever the
        // amplifier's heat (interlock #12 is checked before one starts); the
        // HR500's own 158 °F trip still guards the last few degrees (AJ3G,
        // 11 Nov 2026: "Finish the beacon").
        // The sound card's clock and ours: the audio may not run out well
        // before the over's own end (a device that took it at the wrong
        // rate would have sent something else).
        if self.player.finished() && now < o.end_utc - 1.0 {
            return Some(Some("the transmit audio ran out early".into()));
        }
        (now >= o.end_utc + TAIL_S).then_some(None)
    }

    /// Once a frame.
    pub(crate) fn frame(
        &mut self,
        s: &mut TxSettings,
        env: Env<'_>,
        feed: Vec<TxHeard>,
        heard: &station::Heard,
        badges: &dyn Fn(&str, Option<&str>) -> Badges,
    ) -> TxOut {
        let mut out = TxOut::default();
        let was = (self.qso.cfg.my_call.clone(), self.qso.cfg.my_grid.clone());
        self.configure(s, env.station);
        if was != (self.qso.cfg.my_call.clone(), self.qso.cfg.my_grid.clone()) {
            self.generate(s);
        }
        let now = env.now;
        let me = env.station.call().unwrap_or_default();
        self.take_feed(s, &env, feed, heard, badges, &me);
        let tuning = env.rig.and_then(|r| r.tuning);
        // The over: keyed, done, or never keyed.
        if let Some(o) = self.over.as_mut() {
            if tuning.is_some_and(|t| t.keying == Keying::Data) {
                o.keyed_seen = true;
            } else if o.keyed_seen {
                // Unkeyed: at its end, or cut short.
                let done = now >= o.end_utc - 0.5;
                self.player.stop();
                let (mode, slot) = (o.mode, o.slot_utc);
                self.over = None;
                if done {
                    if mode == TxMode::Ft8 && self.hand.is_none() && self.left_slot != Some(slot) {
                        self.qso.note_tx_sent(now as i64);
                    }
                } else if self.enabled {
                    self.enabled = false;
                    let why = env.rig.and_then(|r| r.tune_note.clone());
                    self.note = Some(format!(
                        "The over was cut short{}. ENABLE TX is off.",
                        why.map(|w| format!(": {w}")).unwrap_or_default()
                    ));
                }
            } else if o.asked.elapsed() > KEY_WAIT {
                self.player.stop();
                self.over = None;
                self.enabled = false;
                let why = env.rig.and_then(|r| r.tune_note.clone());
                self.note = Some(format!(
                    "The radio didn't key{}. ENABLE TX is off.",
                    why.map(|w| format!(": {w}")).unwrap_or_default()
                ));
            }
        } else if self.player.playing() && tuning.is_none() {
            self.player.stop();
        }
        // The sequencer's clock, and contacts finished.
        if s.mode == TxMode::Ft8 {
            self.qso.tick(now as i64);
            self.take_completed(s.prompt_log);
            if !self.qso.busy()
                && let Some(q) = self.qso.take_next_queued(now as i64, |c| badges(c, None).dupe)
            {
                self.tx_even = self.parity.get(&q.call).map_or(s.tx_even, |even| !even);
                self.report = q.snr_db;
                self.dx_seen = Some((q.call.clone(), q.audio_hz, 0.0));
                self.qso.start_qso(&q.call, q.grid.clone(), q.snr_db, q.wait_for_cq, now as i64);
                self.halted = false;
                self.generate(s);
            }
        }
        if self.report != self.qso.reports().0.unwrap_or(self.report) {
            self.qso.set_report(self.report);
        }
        // ENABLE TX turned off in WSPR (HALT, an interlock, a mode change):
        // TRANSMIT to OFF (decision 91).
        if self.was_enabled && !self.enabled {
            self.duty_off(s);
        }
        self.was_enabled = self.enabled;
        // The transmit audio: the bar, AUTO's pick, or the box. Entering a
        // mode, the bar is placed for it, never read (it was the other
        // mode's).
        let entering = self.bar_mode != Some(s.mode);
        self.bar_mode = env.bar_ours.then_some(s.mode);
        if s.mode == TxMode::Ft8 && !entering {
            if env.bar_moved && s.freq == FreqMode::Auto {
                s.freq = FreqMode::Manual;
            }
            // A value typed in the window (or a decode clicked) since last
            // frame stands, and the bar goes to it below; otherwise the bar
            // is read, as a drag or a retune moved it (decision 126).
            let typed = self.audio_seen.is_some_and(|a| a != s.audio_hz) && !env.bar_moved;
            if let Some(b) = env.bar_off.filter(|_| self.over.is_none())
                && s.freq != FreqMode::Auto
                && !typed
                && (100.0..=3000.0).contains(&b)
            {
                s.audio_hz = b as f32;
            }
        }
        if s.mode == TxMode::Wspr {
            if entering && env.bar_ours {
                // ROAM starts again from the sub-band's centre.
                self.wspr_sent = false;
            }
            out.set_bar_off = self.wspr_bar(s, &env, entering);
        }
        if s.mode == TxMode::Ft8 && s.freq == FreqMode::Auto {
            // AUTO counts only our own transmit period: the signals decoded
            // in it (strong ones for longer), and the energy the receiver
            // heard in it that nothing decoded (decision 115), each weighted
            // by how recent and how strong (decision 118).
            let marks: Vec<freq::Mark> = self
                .heard
                .iter()
                .filter(|(t, _, snr)| {
                    sched::is_even(*t, sched::FT8_PERIOD) == self.tx_even
                        && *t >= now as i64 - freq::keep_s(*snr)
                })
                .map(|&(t, a, snr)| freq::Mark::decode(t, a, snr, now))
                .chain(
                    self.occ
                        .spans(self.tx_even, now)
                        .into_iter()
                        .map(|(t, a, b, db)| freq::Mark::energy(t, a, b, db, now)),
                )
                .collect();
            // Kept inside the radio's transmit filter (decision 125).
            let range = auto_range(env.rig, env.rx_dial);
            out.busy = freq::shade(&marks, range);
            // Only between overs, and not in the middle of an exchange,
            // so the station we work finds us where they answered.
            if self.over.is_none() && !(self.qso.busy() && self.auto_hz.is_some()) {
                let p = freq::pick(&marks, self.auto_hz.unwrap_or(s.audio_hz), range);
                self.auto_hz = Some(p);
                s.audio_hz = p;
            }
        } else {
            self.auto_hz = None;
        }
        if s.mode == TxMode::Ft8
            && self.over.is_none()
            && env.rx_dial.is_some()
            && env.bar_off.is_none_or(|b| (b - s.audio_hz as f64).abs() >= 1.0)
        {
            out.set_bar_off = Some(s.audio_hz as f64);
        }
        self.audio_seen = (s.mode == TxMode::Ft8).then_some(s.audio_hz);
        let plan = (s.mode == TxMode::Ft8).then(|| self.tx_audio(s, &env)).flatten();
        self.filter_note = plan.and_then(|p| filter_note(env.rig, p));
        // SAFETY INTERLOCK (docs/safety-interlocks.md, #7): VFO B is the TX
        // window's while it sends FT8 in split; it moves it only between
        // overs (`follow_split`) and keys only on it as read back.
        out.split_b = match plan.filter(|_| self.enabled) {
            Some(AudioPlan::MoveB { dial, .. }) => Some(dial),
            Some(AudioPlan::Go { split_dial, .. }) => split_dial,
            None => None,
        };
        // GLOBE's strobe.
        if s.mode == TxMode::Ft8
            && let Some(c) = self.qso.dx_call().filter(|_| self.qso.busy() || self.on_air())
        {
            out.strobe = Some((c.to_string(), self.qso.dx_grid().map(str::to_string), self.on_air()));
        }
        // Start an over? In split, VFO B first, if it must move.
        if self.over.is_none() && self.enabled {
            if s.mode == TxMode::Ft8 {
                self.follow_split(s, &env);
            }
            match self.try_start(s, env, &me) {
                Ok(true) => self.note = None,
                Ok(false) => {}
                Err(why) => self.note = Some(why),
            }
        }
        out
    }

    /// The WSPR decodes since last frame, into the WSPR list.
    pub(crate) fn take_wspr(&mut self, feed: Vec<WsprHeard>, me: Option<station::LatLon>) {
        for h in feed {
            let path = me.zip(station::grid_to_latlon(&h.grid)).map(|(a, b)| (station::bearing_deg(a, b), station::distance_km(a, b)));
            self.wspr_rows.push_front(WsprRow { h, path });
        }
        self.wspr_rows.truncate(ROWS);
    }

    /// The FT8 decodes since last frame: into the sequencer and the list.
    fn take_feed(
        &mut self,
        s: &TxSettings,
        env: &Env<'_>,
        feed: Vec<TxHeard>,
        heard: &station::Heard,
        badges: &dyn Fn(&str, Option<&str>) -> Badges,
        me: &str,
    ) {
        if let Some((dial, _)) = env.rx_dial {
            // A band change (or a big move) clears the FT8 list. WSPR's is
            // kept and shown by band (decision 92).
            if self.list_dial.is_some_and(|d| (d - dial).abs() > 5_000.0) {
                self.rows.clear();
                self.heard.clear();
                self.occ.clear();
            }
            self.list_dial = Some(dial);
        }
        if feed.is_empty() {
            return;
        }
        let my_pos = env.station.pos();
        let my_cont = crate::dxcc::lookup(me).map(|e| e.continent);
        let mut decodes = Vec::new();
        let mut dts = Vec::new();
        for h in feed {
            let snr = h.snr_db.round().clamp(-99.0, 99.0) as i16;
            let d = msg::parse(&h.text, h.slot_utc, snr, h.dt, h.audio_hz);
            dts.push(h.dt);
            self.heard.push_back((h.slot_utc, h.audio_hz, h.snr_db));
            if let Some(f) = &d.from {
                self.parity.insert(f.clone(), sched::is_even(h.slot_utc, sched::FT8_PERIOD));
            }
            let ent = d.from.as_deref().and_then(crate::dxcc::lookup);
            let grid = d.grid.clone().or_else(|| d.from.as_deref().and_then(|c| heard.get(c)).map(|h| h.grid.clone()));
            let their = grid.as_deref().and_then(station::grid_to_latlon);
            let path = match (my_pos, their) {
                (Some(a), Some(b)) => Some((station::bearing_deg(a, b), station::distance_km(a, b), false)),
                (Some(a), None) => ent.as_ref().map(|e| {
                    let b = station::LatLon { lat: e.lat, lon: e.lon };
                    (station::bearing_deg(a, b), station::distance_km(a, b), true)
                }),
                _ => None,
            };
            let to_me = !me.is_empty()
                && (d.to.as_deref() == Some(me)
                    || h.text.split_whitespace().any(|t| t.trim_matches(['<', '>']).eq_ignore_ascii_case(me)));
            let cq_ok = d.is_cq
                && msg::cq_includes(d.cq_to.as_deref(), me, my_cont, ent.as_ref().map(|e| e.continent));
            let b = d.from.as_deref().map(|c| badges(c, d.grid.as_deref())).unwrap_or_default();
            self.rows.push_front(ListRow {
                row: h.row,
                rf_hz: h.rf_hz,
                country: ent.as_ref().map(|e| e.name.clone()),
                flag: ent.as_ref().map_or("", |e| e.flag),
                continent: ent.as_ref().map(|e| e.continent),
                cq_zone: ent.as_ref().map(|e| e.cq_zone),
                itu_zone: ent.as_ref().map(|e| e.itu_zone),
                path,
                badges: b,
                to_me,
                cq_ok,
                ours: false,
                d: d.clone(),
            });
            decodes.push(d);
        }
        self.rows.truncate(ROWS);
        self.note_dx_seen();
        // Strong stations are remembered longer (decision 115).
        let newest = self.heard.iter().map(|h| h.0).max().unwrap_or(0);
        self.heard.retain(|h| h.0 >= newest - freq::keep_s(h.2));
        dts.sort_by(f32::total_cmp);
        self.dt = dts.get(dts.len() / 2).copied();
        if s.auto_seq && s.mode == TxMode::Ft8 && self.qso.on_rx(&decodes, env.now as i64) {
            self.take_completed(s.prompt_log);
            // A station that answered our CQ is now the one worked.
            self.note_dx_seen();
            if let Some(c) = self.qso.dx_call().map(str::to_string)
                && self.lines[0].split_whitespace().next().map(|t| t.trim_matches(['<', '>']))
                    != Some(c.as_str())
            {
                if let Some(e) = self.parity.get(&c) {
                    self.tx_even = !e;
                }
                self.generate(s);
            }
        }
    }

    /// Where the station worked was last copied, from the list's newest
    /// decode of theirs.
    fn note_dx_seen(&mut self) {
        let Some(call) = self.qso.dx_call() else { return };
        if let Some(r) = self.rows.iter().find(|r| !r.ours && r.d.from.as_deref() == Some(call)) {
            self.dx_seen = Some((call.to_string(), r.d.audio_hz, r.rf_hz));
        }
    }

    /// Start an over if one is due: `Ok(true)` when keyed, `Ok(false)`
    /// when none is due, `Err` why one due can't go.
    fn try_start(&mut self, s: &TxSettings, env: Env<'_>, me: &str) -> Result<bool, String> {
        let now = env.now;
        if me.is_empty() {
            return Err("Set your callsign under SETTINGS, Station.".into());
        }
        let (t0, message, text, start_s, len_s, on) = match s.mode {
            TxMode::Ft8 => {
                let Some(t0) = sched::ft8_open(now, self.tx_even) else { return Ok(false) };
                if self.sent_slot == Some(t0) {
                    return Ok(false);
                }
                let plan = if s.auto_seq { self.qso.plan_tx() } else { self.hand.clone() };
                let Some(text) = plan else { return Ok(false) };
                // As it goes out: a non-standard call in WSJT-X's form.
                let text = msg::wsjtx_form(&text);
                if !env.decoding_ft8 {
                    return Err("DECODE isn't running FT8, so the sequencer can't hear replies.".into());
                }
                // Decision 97: the message is chosen once the slot before
                // has been decoded, so a reply that came in it is answered
                // now, not one over late (AJ3G, 11 Nov 2026: a second FM18
                // went out after CT7AUT's report had arrived). If the
                // decode is slow the over waits for it up to DECODE_WAIT_S,
                // then goes with what it has.
                let into = now - t0 as f64;
                if !sched::slot_decoded(t0, env.slot_decoded) && into < sched::DECODE_WAIT_S {
                    return Ok(false);
                }
                let (off, on) = match self.tx_audio(s, &env) {
                    Some(AudioPlan::Go { audio, split_dial }) => (audio, split_dial),
                    // The move is asked for (`follow_split`); the over goes
                    // once the radio reads it back, later in this slot or
                    // in the next.
                    Some(AudioPlan::MoveB { dial, .. }) => {
                        return Err(format!(
                            "Moving VFO B to {} for split; the over goes once the radio reads it \
                             back.",
                            rf_text(dial as f64)
                        ));
                    }
                    None => return Err(self.audio_error(s, &env)),
                };
                let m = wave::Message::ft8(&text, off)
                    .ok_or_else(|| format!("\"{text}\" can't be sent as FT8."))?;
                (t0, m, text, sched::ft8_start_s(into), wave::FT8_LEN_S, on)
            }
            TxMode::Wspr => {
                let Some(t0) = sched::wspr_open(now) else { return Ok(false) };
                if self.sent_slot == Some(t0) || !sched::wspr_ours(me, t0, s.wspr_pct) {
                    return Ok(false);
                }
                let grid = env.station.grid().unwrap_or_default();
                if s.wspr_roam {
                    self.roam = self.roam.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                    self.wspr_hz = 1410.0 + ((self.roam >> 33) % 180) as f32;
                } else {
                    self.wspr_hz = s.wspr_audio_hz;
                }
                self.wspr_sent = true;
                let m = wave::Message::wspr(me, &grid, s.wspr_dbm, self.wspr_hz).ok_or_else(|| {
                    "WSPR Type 1 needs a plain call, a 4-character grid and one of its power levels."
                        .to_string()
                })?;
                let text = format!("{me} {} {}", grid.get(..4).unwrap_or(""), s.wspr_dbm);
                (t0, m, text, wave::WSPR_START_S, wave::WSPR_LEN_S, None)
            }
        };
        // This slot is spent, whatever happens below.
        self.sent_slot = Some(t0);
        // SAFETY INTERLOCK (docs/safety-interlocks.md, #13): no data over
        // while SCOPE's receiver and the radio disagree on the mode, or are
        // not on a sideband (decision 116; AJ3G sent FT8 in USB on RADIO).
        let rig_mode = env.rig.filter(|r| r.connected).and_then(|r| r.mode);
        if let Some(why) = env.scope_mode.and_then(|m| crate::rx::data_modes_clash(m, rig_mode)) {
            return Err(why);
        }
        let (Some(link), Some(r)) = (env.link, env.rig.filter(|r| r.connected)) else {
            return Err("The radio isn't connected.".into());
        };
        let level = s.level(s.mode);
        // SAFETY INTERLOCKS (docs/safety-interlocks.md, #6, #7, #8): as TUNE's
        // tone, at this mode's level; nothing else keyed; for WSPR, the
        // amplifier not in its temperature warning.
        let watts = tune::planned_watts(r, Keying::Data, level);
        let why = if env.outside.holder().is_some() {
            Some("An outside program is keyed.".to_string())
        } else if r.tuning.is_some() || r.transmitting() {
            Some("The radio is already transmitting.".to_string())
        } else {
            tune::tone_blocked(r, env.device.as_deref())
                .or_else(|| tune::key_blocked(env.amp, r, watts))
                .or_else(|| (s.mode == TxMode::Wspr).then(|| too_hot(env.amp)).flatten())
        };
        if let Some(why) = why {
            return Err(why);
        }
        let dev = env.device.clone().expect("checked by tone_blocked");
        let gain = audio::gain_for(level, r.audio_ceiling);
        let start = t0 as f64 + start_s;
        let start_at = UNIX_EPOCH + Duration::from_secs_f64(start);
        // The audio first, then the key: keyed with no audio the radio
        // sends nothing, never anything else.
        self.player.start(&dev, gain, start_at, |rate| message.render(rate))?;
        self.player_device = Some(dev);
        self.gain = gain;
        let end = start + len_s;
        let keyed_for = Duration::from_secs_f64((end + TAIL_S - now).max(1.0) + 1.0);
        env.outside.panel_tune_starting();
        // SAFETY INTERLOCK (docs/safety-interlocks.md, #7): in split, keyed
        // only if the radio reads back the transmit dial the audio was
        // worked out for, straight before (decision 106).
        match on {
            Some(dial) => link.tune_start_on(keyed_for, Keying::Data, dial),
            None => link.tune_start(keyed_for, Keying::Data),
        }
        let audio_hz = match &message {
            wave::Message::Ft8 { audio_hz, .. } | wave::Message::Wspr { audio_hz, .. } => *audio_hz,
        };
        if s.mode == TxMode::Ft8 {
            self.qso.record_tx(&text, t0);
            // The AF column is from the receive dial, as the decodes': in
            // split the tone itself is from VFO B.
            self.rows.push_front(ListRow {
                d: msg::parse(&text, t0, 0, 0.0, s.audio_hz),
                row: 0,
                rf_hz: 0.0,
                country: None,
                flag: "",
                continent: None,
                cq_zone: None,
                itu_zone: None,
                path: None,
                badges: Badges::default(),
                to_me: false,
                cq_ok: false,
                ours: true,
            });
        }
        self.over = Some(Over {
            mode: s.mode,
            slot_utc: t0,
            text,
            audio_hz,
            asked: Instant::now(),
            keyed_seen: false,
            end_utc: end,
        });
        Ok(true)
    }

    /// The FT8 tone 0 to send at, Hz from the radio's transmit dial, for
    /// the signal to land `audio_hz` above the receiver's dial (the bar):
    /// the radio transmits from VFO B in split, which may have to move
    /// first (decision 106).
    fn tx_audio(&self, s: &TxSettings, env: &Env<'_>) -> Option<AudioPlan> {
        let r = env.rig?;
        let rx = env.rx_dial.or_else(|| r.freq_hz.map(|f| (f as f64, r.mode == Some("LSB"))))?;
        audio_plan(r, rx, s.audio_hz as f64)
    }

    /// Split (decision 106): when the over can't go from VFO B where it is,
    /// ask the radio to move it, read back by the link, and say so in the
    /// log. Not while transmitting (the link refuses it then too, #5); asked
    /// again only after [`SPLIT_RETRY`] if the read-back doesn't show it.
    fn follow_split(&mut self, s: &TxSettings, env: &Env<'_>) {
        let (Some(link), Some(r)) = (env.link, env.rig.filter(|r| r.connected)) else { return };
        if r.transmitting() {
            return;
        }
        let Some(AudioPlan::MoveB { dial, audio }) = self.tx_audio(s, env) else {
            self.split_sent = None;
            return;
        };
        if self.split_sent.is_some_and(|(d, t)| d == dial && t.elapsed() < SPLIT_RETRY) {
            return;
        }
        let Some(f) = r.model.protocol().set_freq(Vfo::B, dial) else { return };
        let from = r.freq_b_hz.map_or_else(|| "not read".to_string(), |b| rf_text(b as f64));
        tracing::info!(
            "transmit: split: VFO B moved from {from} to {}, so the {:.0} Hz offset goes out as a \
             {audio:.0} Hz tone from VFO B (the signal lands where the bar is)",
            rf_text(dial as f64),
            s.audio_hz,
        );
        link.set(vec![f]);
        self.split_sent = Some((dial, Instant::now()));
    }

    /// WSPR and the scope's bar (decision 88): a drag sets the beacon's
    /// offset (1400 to 1600 Hz, from the next beacon) and turns ROAM off;
    /// otherwise the bar shows the offset set, or with ROAM the one of the
    /// beacon going (or last gone) out, 1500 Hz before the first. Returns
    /// where the bar should go, as an offset from the receiver's dial.
    fn wspr_bar(&mut self, s: &mut TxSettings, env: &Env<'_>, entering: bool) -> Option<f64> {
        let rx = env.rx_dial?;
        let tx = env.rig.and_then(rig_tx_dial);
        if env.bar_moved
            && !entering
            && let Some(b) = env.bar_off
        {
            let a = wspr_audio_from_bar(b, rx, tx);
            s.wspr_audio_hz = (a.clamp(WSPR_LO_HZ as f64, WSPR_HI_HZ as f64) as f32).round();
            s.wspr_roam = false;
        }
        let want = wspr_bar_from_audio(self.wspr_bar_audio(s) as f64, rx, tx);
        (env.bar_off.is_none_or(|b| (b - want).abs() >= 1.0)).then_some(want)
    }

    /// The audio offset the bar shows in WSPR: the one set, or with ROAM
    /// the beacon's going (or last gone) out, else the sub-band's centre.
    pub(crate) fn wspr_bar_audio(&self, s: &TxSettings) -> f32 {
        if s.wspr_roam { self.wspr_offset(s).unwrap_or(WSPR_MID_HZ) } else { s.wspr_audio_hz }
    }

    fn audio_error(&self, s: &TxSettings, env: &Env<'_>) -> String {
        match env.rig {
            None => "The radio isn't connected.".into(),
            Some(r) if r.freq_hz.is_none() => "The radio's dial hasn't been read yet.".into(),
            Some(r) if r.split == Some(true) && r.vfo == Some(1) => {
                "The radio is in split receiving on VFO B, so it transmits from VFO A, which the \
                 panel doesn't move: receive on VFO A (or turn split off)."
                    .into()
            }
            Some(_) => format!(
                "The radio's transmit dial is too far from the receiver for {:.0} Hz: turn TX \
                 follows RX on, or move the bar.",
                s.audio_hz
            ),
        }
    }
}

/// The audio the radio sends from its transmit dial, Hz, as before split
/// moved VFO B: an over whose tone would fall outside it isn't sent from
/// that dial.
const AUDIO_OK: std::ops::RangeInclusive<f64> = 100.0..=2950.0;

/// In split, VFO B is left where it is while the tone from it stays in the
/// flat middle of the radio's transmit filter, Hz; outside, VFO B is moved
/// so the tone is 1500 Hz give or take 250 (decision 125: on AJ3G's DUO a
/// tone near the filter's top edge went out with almost no power).
const SPLIT_KEEP: std::ops::RangeInclusive<f64> = 1000.0..=2000.0;

/// AUTO's range (decision 125): out of split, as an offset from the
/// receive dial `rx`, the audio menu 35 TX BW passes cleanly from VFO A
/// ([`freq::Range::for_filter`]); the full range when TX BW isn't known,
/// and in split, where VFO B is moved to keep the tone in the middle.
pub(crate) fn auto_range(r: Option<&RigStatus>, rx: Option<(f64, bool)>) -> freq::Range {
    let Some(r) = r else { return freq::FULL };
    if r.split == Some(true) {
        return freq::FULL;
    }
    let cap = freq::Range::for_filter(rigcat::elad::tx_bw_hz(r));
    if cap == freq::FULL {
        return cap;
    }
    // The tone is the offset plus this.
    let shift = match (rx, r.freq_hz) {
        (Some(rx), Some(a)) => audio_from(rx, a as f64, 0.0) as f32,
        _ => 0.0,
    };
    let lo = (cap.lo - shift).max(freq::LO_HZ);
    let hi = (cap.hi - shift).min(freq::HI_HZ);
    freq::Range { lo, hi }
}

/// What to say when the over's tone is outside what menu 35 TX BW passes
/// cleanly (decision 125): set by hand on MANUAL or HOLD, or AUTO held in
/// the middle of an exchange.
pub(crate) fn filter_note(r: Option<&RigStatus>, plan: AudioPlan) -> Option<String> {
    let bw = rigcat::elad::tx_bw_hz(r?)?;
    let cap = freq::Range::for_filter(Some(bw));
    let audio = match plan {
        AudioPlan::Go { audio, .. } | AudioPlan::MoveB { audio, .. } => audio,
    };
    if cap == freq::FULL || cap.holds(audio) {
        return None;
    }
    let (lo, hi) = bw;
    let side = if audio > cap.top_tone() {
        format!("above {:.0} Hz", cap.top_tone())
    } else {
        format!("below {:.0} Hz", cap.lo)
    };
    Some(format!(
        "The {audio:.0} Hz tone is {side}, near the edge of the radio's transmit filter (menu 35 \
         TX BW {lo}-{hi}): it may go out with little power. Move it in, or widen TX BW."
    ))
}

/// Split (decision 106): where the tone goes once VFO B is moved for it,
/// Hz, as WSJT-X's Rig split keeps it in the middle of the radio's audio.
pub(crate) const SPLIT_AUDIO_HZ: f64 = 1500.0;
/// VFO B moves in steps of this, Hz, as WSJT-X's does: the tone lands within
/// 250 Hz of [`SPLIT_AUDIO_HZ`], and small moves of the offset leave VFO B
/// where it is.
pub(crate) const SPLIT_STEP_HZ: f64 = 500.0;
/// How long a VFO B move is waited for before it is asked again.
const SPLIT_RETRY: Duration = Duration::from_millis(1500);

/// How the FT8 over goes, from the radio as read.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum AudioPlan {
    /// Send this tone, Hz from the transmit dial; in split, keyed only on
    /// `split_dial` (VFO B as read when the tone was worked out).
    Go { audio: f32, split_dial: Option<u64> },
    /// In split, VFO B goes to `dial` first; the tone will then be `audio`.
    MoveB { dial: u64, audio: f32 },
}

/// The tone for a signal `off` Hz from the receive dial `rx` (radio Hz, and
/// lower sideband or not), sent from the transmit dial `tx`.
fn audio_from(rx: (f64, bool), tx: f64, off: f64) -> f64 {
    let (rx, lower) = rx;
    if lower { tx - rx + off } else { rx + off - tx }
}

/// WSJT-X's Rig split (decision 106): VFO B for a signal `off` Hz from the
/// receive dial `rx`, the receive dial plus the offset less 1500 Hz in steps
/// of [`SPLIT_STEP_HZ`] (minus on LSB, where the audio rises as the signal
/// goes down), and the tone it then takes, 1500 Hz give or take the rest.
pub(crate) fn split_dial(rx: (f64, bool), off: f64) -> (u64, f32) {
    let shift = ((off - SPLIT_AUDIO_HZ) / SPLIT_STEP_HZ).round() * SPLIT_STEP_HZ;
    let dial = crate::scope::txsig::place(rx.0, rx.1, shift).round().max(0.0);
    (dial as u64, audio_from(rx, dial, off).round() as f32)
}

/// How an FT8 over `off` Hz from the receive dial `rx` goes on the radio
/// `r`. Out of split, from VFO A as before, or not at all if the tone would
/// leave [`AUDIO_OK`]. In split, receiving on A, from VFO B: where it is
/// while the tone stays in [`SPLIT_KEEP`] (TX follows RX, the operator, or
/// an earlier move put it there), else moved by [`split_dial`] (decision
/// 125). Split receiving on B transmits from A, which the panel doesn't
/// move: as before.
///
/// SAFETY INTERLOCK (docs/safety-interlocks.md, #7): in split the over is
/// keyed only on the VFO B returned in `Go`, as read back.
pub(crate) fn audio_plan(r: &RigStatus, rx: (f64, bool), off: f64) -> Option<AudioPlan> {
    let ok = |a: f64| AUDIO_OK.contains(&a).then_some(a.round() as f32);
    if r.split != Some(true) {
        let audio = ok(audio_from(rx, r.freq_hz? as f64, off))?;
        return Some(AudioPlan::Go { audio, split_dial: None });
    }
    if r.vfo == Some(1) {
        return None;
    }
    let keep = |a: f64| SPLIT_KEEP.contains(&a).then_some(a.round() as f32);
    if let Some(b) = r.freq_b_hz.filter(|&b| b > 0)
        && let Some(audio) = keep(audio_from(rx, b as f64, off))
    {
        return Some(AudioPlan::Go { audio, split_dial: Some(b) });
    }
    let (dial, audio) = split_dial(rx, off);
    Some(AudioPlan::MoveB { dial, audio })
}

/// WSPR's sub-band in the audio, Hz: 200 Hz either side of 1500.
pub(crate) const WSPR_LO_HZ: f32 = 1400.0;
pub(crate) const WSPR_HI_HZ: f32 = 1600.0;
pub(crate) const WSPR_MID_HZ: f32 = 1500.0;

/// The radio's transmit dial, Hz: VFO B in split, else the receive VFO.
pub(crate) fn rig_tx_dial(r: &RigStatus) -> Option<f64> {
    crate::scope::txsig::tx_dial(r.freq_hz, r.freq_b_hz, r.vfo, r.split)
}

/// The bar's offset from the receiver's dial (`rx`: dial and lower
/// sideband) for a beacon `audio` Hz from the transmit dial `tx` (the
/// receive dial when not known): the two differ in split.
pub(crate) fn wspr_bar_from_audio(audio: f64, rx: (f64, bool), tx: Option<f64>) -> f64 {
    let (dial, lower) = rx;
    let rf = crate::scope::txsig::place(tx.unwrap_or(dial), lower, audio);
    if lower { dial - rf } else { rf - dial }
}

/// The beacon's audio from the transmit dial for the bar at `bar` Hz from
/// the receiver's dial: [`wspr_bar_from_audio`] the other way.
pub(crate) fn wspr_audio_from_bar(bar: f64, rx: (f64, bool), tx: Option<f64>) -> f64 {
    let (dial, lower) = rx;
    let rf = crate::scope::txsig::place(dial, lower, bar);
    let tx = tx.unwrap_or(dial);
    if lower { tx - rf } else { rf - tx }
}

/// Our signal's true RF, Hz (decision 89): the transmit dial, plus the
/// audio on USB (minus on LSB), plus the transverter's offset.
pub(crate) fn tx_rf_hz(r: &RigStatus, audio_hz: f64, xvtr_hz: f64) -> Option<f64> {
    let dial = rig_tx_dial(r)?;
    Some(crate::scope::txsig::place(dial, r.mode == Some("LSB"), audio_hz) + xvtr_hz)
}

/// A frequency as "10.140 200 MHz (10 140.200 kHz)".
pub(crate) fn rf_text(hz: f64) -> String {
    let hz = hz.round().max(0.0) as u64;
    let (mhz, rest) = (hz / 1_000_000, hz % 1_000_000);
    let (k, h) = (rest / 1000, rest % 1000);
    let khz_whole = hz / 1000;
    let khz = if khz_whole >= 1000 {
        format!("{} {:03}", khz_whole / 1000, khz_whole % 1000)
    } else {
        khz_whole.to_string()
    };
    format!("{mhz}.{k:03} {h:03} MHz ({khz}.{h:03} kHz)")
}

/// A frequency as the WSPR lists show it: "10.140 123" MHz.
pub(crate) fn mhz_cell(hz: f64) -> String {
    let hz = hz.round().max(0.0) as u64;
    let rest = hz % 1_000_000;
    format!("{}.{:03} {:03}", hz / 1_000_000, rest / 1000, rest % 1000)
}

/// Where a WSPR beacon stops for the amplifier's heat: over 155 °F, a step
/// below the HR500's own 70 °C (158 °F) trip. At first this was the 120 °F
/// warning colour, which stopped beacons at 49 °C, far short of anything
/// the amplifier minds; SDRoxide never stopped for it (AJ3G, 11 Nov 2026:
/// "Should not trip until over 155F").
pub(crate) const WSPR_HOT_C: f32 = (155.0 - 32.0) / 1.8;

/// SAFETY INTERLOCK (docs/safety-interlocks.md, #12): a WSPR beacon keys
/// the amplifier for 110.6 s, so none starts once its temperature is over
/// [`WSPR_HOT_C`] (155 °F). One already going out finishes: the HR500's own
/// 158 °F trip guards it (AJ3G, 11 Nov 2026).
pub(crate) fn too_hot(amp: &AmpStatus) -> Option<String> {
    let t = amp.readings.filter(|_| amp.connected)?.known_temp_c()?;
    (t > WSPR_HOT_C).then(|| {
        format!(
            "The amplifier is at {t:.0} °C ({:.0} °F), over the 155 °F a beacon won't start at.",
            t * 1.8 + 32.0
        )
    })
}

#[cfg(test)]
mod hot_tests {
    use super::*;
    use hardrock::AmpReadings;

    fn amp(temperature: f32, celsius: bool) -> AmpStatus {
        AmpStatus {
            connected: true,
            readings: Some(AmpReadings { temperature, celsius: Some(celsius), ..Default::default() }),
            ..Default::default()
        }
    }

    #[test]
    fn a_wspr_beacon_stops_only_over_155_f() {
        // 49 °C (120 °F) is only the warning colour: it once stopped beacons.
        assert!(too_hot(&amp(49.0, true)).is_none());
        assert!(too_hot(&amp(65.0, true)).is_none());
        assert!(too_hot(&amp(155.0, false)).is_none());
        assert!(too_hot(&amp(156.0, false)).is_some());
        assert!(too_hot(&amp(69.0, true)).is_some());
        // Not before the amplifier has said which scale it reads in.
        let mut a = amp(156.0, false);
        a.readings.as_mut().unwrap().celsius = None;
        assert!(too_hot(&a).is_none());
    }
}

#[cfg(test)]
mod round3_tests {
    use super::*;

    fn over(mode: TxMode) -> Over {
        Over {
            mode,
            slot_utc: 0,
            text: "AJ3G FN20 37".into(),
            audio_hz: 1500.0,
            asked: Instant::now(),
            keyed_seen: true,
            end_utc: 110.6,
        }
    }

    #[test]
    fn level_moves_the_over_going_out_at_once() {
        let mut tx = Tx::default();
        // Nothing going out: nothing to change.
        tx.set_level(0.25, 1.0);
        assert_eq!(tx.gain, 0.0);
        tx.over = Some(over(TxMode::Wspr));
        tx.set_level(0.25, 1.0);
        assert_eq!(tx.gain, 0.5);
        assert_eq!(tx.player.gain(), 0.5);
        // The same level as TX TUNE's tone gets, within the radio's ceiling.
        tx.set_level(1.0, 0.24);
        assert_eq!(tx.player.gain(), audio::gain_for(1.0, 0.24));
    }

    #[test]
    fn the_wspr_bar_and_the_beacon_agree_in_split_and_on_lsb() {
        // No split: the bar is the audio.
        assert_eq!(wspr_bar_from_audio(1500.0, (10_138_700.0, false), Some(10_138_700.0)), 1500.0);
        // Split, VFO B 300 Hz above: the beacon's audio lands 300 Hz higher.
        let rx = (10_138_700.0, false);
        let tx = Some(10_139_000.0);
        assert_eq!(wspr_bar_from_audio(1500.0, rx, tx), 1800.0);
        assert_eq!(wspr_audio_from_bar(1800.0, rx, tx), 1500.0);
        // LSB: below the dial.
        let rx = (7_040_100.0, true);
        let tx = Some(7_040_000.0);
        let bar = wspr_bar_from_audio(1450.0, rx, tx);
        assert_eq!(bar, 1550.0);
        assert_eq!(wspr_audio_from_bar(bar, rx, tx), 1450.0);
    }

    #[test]
    fn the_beacons_rf_is_the_transmit_dial_plus_the_offset() {
        let mut r =
            RigStatus { freq_hz: Some(10_138_700), mode: Some("USB"), ..Default::default() };
        assert_eq!(tx_rf_hz(&r, 1500.0, 0.0), Some(10_140_200.0));
        assert_eq!(rf_text(10_140_200.0), "10.140 200 MHz (10 140.200 kHz)");
        // Split: VFO B.
        r.freq_b_hz = Some(10_138_800);
        r.split = Some(true);
        assert_eq!(tx_rf_hz(&r, 1500.0, 0.0), Some(10_140_300.0));
        // LSB, and a transverter 116 MHz up.
        let r = RigStatus { freq_hz: Some(28_490_000), mode: Some("LSB"), ..Default::default() };
        assert_eq!(tx_rf_hz(&r, 1500.0, 116_000_000.0), Some(144_488_500.0));
        assert_eq!(rf_text(144_488_500.0), "144.488 500 MHz (144 488.500 kHz)");
        assert_eq!(rf_text(474_200.0), "0.474 200 MHz (474.200 kHz)");
        assert_eq!(mhz_cell(10_140_123.4), "10.140 123");
        assert_eq!(tx_rf_hz(&RigStatus::default(), 1500.0, 0.0), None);
    }

    #[test]
    fn the_worked_mark_shows_only_while_the_qso_is_in_progress() {
        let mut tx = Tx::default();
        tx.qso.cfg.my_call = "AJ3G".into();
        assert_eq!(tx.dx_mark(), None);
        tx.qso.start_qso("K1ABC", None, -5, false, 0);
        tx.dx_seen = Some(("K1ABC".into(), 1550.0, 14_075_550.0));
        assert_eq!(tx.dx_mark().map(|m| m.0), Some("K1ABC".into()));
        // HALT takes it away; ENABLE TX resumes the QSO and brings it back.
        tx.halt(None);
        assert_eq!(tx.dx_mark(), None);
        assert!(tx.dx_copied().is_some(), "the QSO box still says where");
        let mut s = TxSettings::default();
        tx.enable(&mut s);
        assert!(tx.dx_mark().is_some());
        // Logged: gone, though the call stays in the QSO box.
        assert!(tx.qso.log_by_hand(45));
        assert_eq!(tx.dx_mark(), None);
        assert!(tx.dx_copied().is_some());
    }

    #[test]
    fn enable_tx_and_wsprs_transmit_off_go_together() {
        let mut tx = Tx::default();
        let mut s = TxSettings { mode: TxMode::Wspr, wspr_pct: 33, ..Default::default() };
        tx.enable(&mut s);
        assert_eq!(s.wspr_pct, 33);
        tx.disable(&mut s);
        assert!(!tx.enabled);
        assert_eq!((s.wspr_pct, s.wspr_pct_last), (0, 33));
        tx.enable(&mut s);
        assert_eq!(s.wspr_pct, 33);
        // OFF turns ENABLE TX off, and keeps the share.
        tx.set_duty(&mut s, 0);
        assert!(!tx.enabled);
        assert_eq!((s.wspr_pct, s.wspr_pct_last), (0, 33));
        // A share doesn't turn it on.
        tx.set_duty(&mut s, 10);
        assert!(!tx.enabled);
        tx.enable(&mut s);
        assert_eq!(s.wspr_pct, 10);
        // FT8 leaves WSPR's share alone.
        s.mode = TxMode::Ft8;
        tx.disable(&mut s);
        assert_eq!(s.wspr_pct, 10);
    }

    fn frame(
        tx: &mut Tx,
        s: &mut TxSettings,
        bar_off: Option<f64>,
        bar_moved: bool,
        enabled_by_halt: bool,
    ) -> TxOut {
        let amp = AmpStatus::default();
        let mut outside = crate::outside::Outside::default();
        let station = StationSettings::default();
        let heard = station::Heard::default();
        if enabled_by_halt {
            tx.halt(None);
        }
        let env = Env {
            now: 1_800_000_030.0,
            link: None,
            rig: None,
            amp: &amp,
            device: None,
            outside: &mut outside,
            rx_dial: Some((10_138_700.0, false)),
            bar_off,
            bar_moved,
            bar_ours: true,
            station: &station,
            decoding_ft8: false,
            slot_decoded: None,
            scope_mode: None,
        };
        tx.frame(s, env, Vec::new(), &heard, &|_, _| Badges::default())
    }

    #[test]
    fn the_scopes_bar_sets_wsprs_offset() {
        let mut tx = Tx::default();
        let mut s = TxSettings {
            mode: TxMode::Wspr,
            wspr_roam: true,
            audio_hz: 900.0,
            ..Default::default()
        };
        // Entering WSPR: the bar to the sub-band's centre, whatever it was.
        let o = frame(&mut tx, &mut s, Some(900.0), true, false);
        assert_eq!(o.set_bar_off, Some(1500.0));
        assert!(s.wspr_roam);
        // A drag: the offset, clamped to the sub-band, and ROAM off.
        let o = frame(&mut tx, &mut s, Some(1720.0), true, false);
        assert_eq!(s.wspr_audio_hz, 1600.0);
        assert!(!s.wspr_roam);
        assert_eq!(o.set_bar_off, Some(1600.0));
        let o = frame(&mut tx, &mut s, Some(1452.4), true, false);
        assert_eq!(s.wspr_audio_hz, 1452.0);
        assert_eq!(o.set_bar_off, None);
        // FT8's offset is FT8's.
        assert_eq!(s.audio_hz, 900.0);
        // ROAM: the bar goes where each beacon went.
        s.wspr_roam = true;
        tx.wspr_hz = 1433.0;
        tx.wspr_sent = true;
        let o = frame(&mut tx, &mut s, Some(1452.0), false, false);
        assert_eq!(o.set_bar_off, Some(1433.0));
        // Back to FT8: its own offset, not WSPR's bar.
        s.mode = TxMode::Ft8;
        s.freq = FreqMode::Manual;
        let o = frame(&mut tx, &mut s, Some(1433.0), false, false);
        assert_eq!(s.audio_hz, 900.0);
        assert_eq!(o.set_bar_off, Some(900.0));
    }

    /// Decision 126 (AJ3G: "In Manual make it so I can also type in a
    /// frequency"): a value typed (or a decode clicked) on MANUAL moves the
    /// bar there and stays until the bar is dragged; the bar is otherwise
    /// followed as before.
    #[test]
    fn a_typed_tx_audio_on_manual_moves_the_bar_and_stays() {
        let mut tx = Tx::default();
        let mut s = TxSettings {
            mode: TxMode::Ft8,
            freq: FreqMode::Manual,
            audio_hz: 1200.0,
            ..Default::default()
        };
        // Entering FT8: the bar is placed, not read.
        assert_eq!(frame(&mut tx, &mut s, Some(800.0), false, false).set_bar_off, Some(1200.0));
        assert_eq!(frame(&mut tx, &mut s, Some(1200.0), false, false).set_bar_off, None);
        // The bar moved (a drag, or the receiver retuned): followed.
        frame(&mut tx, &mut s, Some(1350.0), true, false);
        assert_eq!(s.audio_hz, 1350.0);
        frame(&mut tx, &mut s, Some(1400.0), false, false);
        assert_eq!(s.audio_hz, 1400.0);
        // Typed: kept, and the bar sent there.
        s.audio_hz = 2100.0;
        let o = frame(&mut tx, &mut s, Some(1400.0), false, false);
        assert_eq!((s.audio_hz, o.set_bar_off), (2100.0, Some(2100.0)));
        let o = frame(&mut tx, &mut s, Some(2100.0), false, false);
        assert_eq!((s.audio_hz, o.set_bar_off), (2100.0, None));
        // Typed and dragged in the same frame: the drag wins.
        s.audio_hz = 900.0;
        frame(&mut tx, &mut s, Some(1600.0), true, false);
        assert_eq!(s.audio_hz, 1600.0);
        // HOLD keeps a typed value the same way.
        s.freq = FreqMode::Hold;
        s.audio_hz = 1000.0;
        let o = frame(&mut tx, &mut s, Some(1600.0), false, false);
        assert_eq!((s.audio_hz, o.set_bar_off), (1000.0, Some(1000.0)));
    }

    /// SAFETY INTERLOCK #13 (decision 116): no over while SCOPE's receiver
    /// and the radio disagree on the mode, or are not on a sideband.
    #[test]
    fn no_over_while_the_scope_and_the_radio_disagree_on_the_mode() {
        let mut tx = Tx::default();
        let s = TxSettings { mode: TxMode::Wspr, wspr_pct: 50, ..Default::default() };
        let amp = AmpStatus::default();
        let mut outside = crate::outside::Outside::default();
        let station =
            StationSettings { callsign: "AJ3G".into(), grid: "FN10".into(), ..Default::default() };
        // A slot of ours, just opening.
        let t0 = (0..100)
            .map(|k| 1_800_000_000 + k * 120)
            .find(|&t| sched::wspr_ours("AJ3G", t, 50))
            .unwrap();
        let mut try_with = |scope: Option<sdr::Mode>, radio: &'static str| {
            let rig = RigStatus { connected: true, mode: Some(radio), ..RigStatus::default() };
            tx.sent_slot = None;
            let env = Env {
                now: t0 as f64 + 0.2,
                link: None,
                rig: Some(&rig),
                amp: &amp,
                device: None,
                outside: &mut outside,
                rx_dial: Some((14_095_600.0, false)),
                bar_off: None,
                bar_moved: false,
                bar_ours: true,
                station: &station,
                decoding_ft8: false,
                slot_decoded: None,
                scope_mode: scope,
            };
            tx.try_start(&s, env, "AJ3G").unwrap_err()
        };
        // AJ3G's case: the radio in USB, SCOPE's receiver in LSB.
        assert!(try_with(Some(sdr::Mode::Lsb), "USB").contains("SCOPE's receiver is in LSB"));
        assert!(try_with(Some(sdr::Mode::Cw), "CW").contains("needs USB"));
        // Agreed, or no receiver on the radio's I/Q: past this check (and
        // refused here only because the test has no link).
        assert_eq!(try_with(Some(sdr::Mode::Usb), "USB"), "The radio isn't connected.");
        assert_eq!(try_with(None, "USB"), "The radio isn't connected.");
    }

    #[test]
    fn halt_in_wspr_sets_transmit_off() {
        let mut tx = Tx::default();
        let mut s = TxSettings { mode: TxMode::Wspr, wspr_pct: 50, ..Default::default() };
        tx.enable(&mut s);
        frame(&mut tx, &mut s, None, false, false);
        assert_eq!(s.wspr_pct, 50);
        frame(&mut tx, &mut s, None, false, true);
        assert_eq!((s.wspr_pct, s.wspr_pct_last), (0, 50));
    }

    #[test]
    fn a_band_change_keeps_the_wspr_list() {
        let mut tx = Tx::default();
        tx.wspr_rows.push_front(WsprRow {
            h: WsprHeard {
                slot_utc: 0,
                snr_db: -20.0,
                drift_hz: 0.0,
                audio_hz: 1500.0,
                call: "K1ABC".into(),
                grid: "FN42".into(),
                dbm: 37,
                rf_hz: 10_140_200.0,
            },
            path: None,
        });
        tx.list_dial = Some(14_095_600.0);
        let mut s = TxSettings { mode: TxMode::Wspr, ..Default::default() };
        frame(&mut tx, &mut s, None, false, false);
        assert_eq!(tx.wspr_rows.len(), 1);
    }
}

#[cfg(test)]
mod split_tests {
    use super::*;

    fn rig(a: u64, b: Option<u64>, split: Option<bool>) -> RigStatus {
        RigStatus {
            connected: true,
            freq_hz: Some(a),
            freq_b_hz: b,
            split,
            vfo: Some(0),
            ..RigStatus::default()
        }
    }

    /// Decision 106, USB: VFO B is the receive dial plus the offset less
    /// 1500 Hz in 500 Hz steps, and the tone the rest; the signal lands on
    /// the receive dial plus the offset either way.
    #[test]
    fn split_puts_vfo_b_so_the_tone_sits_near_1500_hz_on_usb() {
        let rx = (14_074_000.0, false);
        assert_eq!(split_dial(rx, 2526.0), (14_075_000, 1526.0));
        assert_eq!(split_dial(rx, 1500.0), (14_074_000, 1500.0));
        assert_eq!(split_dial(rx, 1749.0), (14_074_000, 1749.0));
        assert_eq!(split_dial(rx, 1751.0), (14_074_500, 1251.0));
        assert_eq!(split_dial(rx, 300.0), (14_073_000, 1300.0));
        assert_eq!(split_dial(rx, 4210.0), (14_076_500, 1710.0));
        // A receiver between hertz: VFO B to the hertz, the tone the rest.
        let (b, a) = split_dial((14_074_000.4, false), 2526.0);
        assert_eq!((b, a), (14_075_000, 1526.0));
        for off in [300.0, 1234.0, 2526.0, 2990.0, 4210.0] {
            let (b, a) = split_dial(rx, off);
            assert!((1250.0..=1750.0).contains(&a), "{off}: {a}");
            assert_eq!(b as f64 + a as f64, rx.0 + off);
        }
    }

    /// Decision 106, LSB: the mirror, VFO B below the receive dial as the
    /// audio rises.
    #[test]
    fn split_puts_vfo_b_so_the_tone_sits_near_1500_hz_on_lsb() {
        let rx = (7_074_000.0, true);
        assert_eq!(split_dial(rx, 2526.0), (7_073_000, 1526.0));
        assert_eq!(split_dial(rx, 300.0), (7_075_000, 1300.0));
        for off in [300.0, 1234.0, 2526.0, 2990.0] {
            let (b, a) = split_dial(rx, off);
            assert!((1250.0..=1750.0).contains(&a), "{off}: {a}");
            assert_eq!(b as f64 - a as f64, rx.0 - off);
        }
    }

    /// Out of split nothing changes: the tone is the offset from VFO A, and
    /// an offset the radio can't send from there is still refused.
    #[test]
    fn without_split_the_over_goes_from_vfo_a_as_before() {
        let rx = (14_074_000.0, false);
        for split in [None, Some(false)] {
            // VFO B far off is never looked at.
            let r = rig(14_074_000, Some(14_200_000), split);
            assert_eq!(
                audio_plan(&r, rx, 2526.0),
                Some(AudioPlan::Go { audio: 2526.0, split_dial: None })
            );
            assert_eq!(audio_plan(&r, rx, 3200.0), None);
            assert_eq!(audio_plan(&r, rx, 50.0), None);
            // The panel's receiver off VFO A: from VFO A, as before.
            assert_eq!(
                audio_plan(&r, (14_074_300.0, false), 1000.0),
                Some(AudioPlan::Go { audio: 1300.0, split_dial: None })
            );
            // LSB.
            let r = rig(7_074_000, None, split);
            assert_eq!(
                audio_plan(&r, (7_074_000.0, true), 1200.0),
                Some(AudioPlan::Go { audio: 1200.0, split_dial: None })
            );
        }
    }

    /// AJ3G, 11 Nov 2026: in split with VFO B left elsewhere the over was
    /// refused ("transmit dial too far ... 2526 Hz"). Now VFO B is moved,
    /// and the over goes, keyed on VFO B as read, once it reads back there.
    #[test]
    fn in_split_vfo_b_left_elsewhere_is_moved_not_refused() {
        let rx = (14_074_000.0, false);
        let r = rig(14_074_000, Some(14_250_000), Some(true));
        assert_eq!(
            audio_plan(&r, rx, 2526.0),
            Some(AudioPlan::MoveB { dial: 14_075_000, audio: 1526.0 })
        );
        // Not read yet: moved too.
        let r = rig(14_074_000, None, Some(true));
        assert!(matches!(audio_plan(&r, rx, 2526.0), Some(AudioPlan::MoveB { .. })));
        // Read back where it was sent: the over goes from there.
        let r = rig(14_074_000, Some(14_075_000), Some(true));
        assert_eq!(
            audio_plan(&r, rx, 2526.0),
            Some(AudioPlan::Go { audio: 1526.0, split_dial: Some(14_075_000) })
        );
        // VFO B already somewhere the tone sits in the middle of the
        // radio's transmit filter (TX follows RX put it on the receiver):
        // left there.
        let r = rig(14_074_000, Some(14_074_000), Some(true));
        assert_eq!(
            audio_plan(&r, rx, 1400.0),
            Some(AudioPlan::Go { audio: 1400.0, split_dial: Some(14_074_000) })
        );
        // Decision 125: but a tone near the filter's edge from there is not
        // sent; VFO B is moved so it sits mid filter (TX follows RX leaves
        // it to the TX window then, `SplitState::tx_window_b`).
        assert_eq!(
            audio_plan(&r, rx, 2526.0),
            Some(AudioPlan::MoveB { dial: 14_075_000, audio: 1526.0 })
        );
        // LSB, VFO B elsewhere.
        let r = rig(7_074_000, Some(7_000_000), Some(true));
        assert_eq!(
            audio_plan(&r, (7_074_000.0, true), 2526.0),
            Some(AudioPlan::MoveB { dial: 7_073_000, audio: 1526.0 })
        );
        // Split receiving on VFO B transmits from A, which isn't moved.
        let mut r = rig(14_074_000, Some(14_250_000), Some(true));
        r.vfo = Some(1);
        assert_eq!(audio_plan(&r, rx, 2526.0), None);
    }

    /// Decision 125: in split, VFO B stays put while the tone from it is in
    /// 1000 to 2000 Hz, and is moved otherwise, so AUTO can use the whole
    /// waterfall and the tone is always mid filter. A move always lands the
    /// tone inside that window, so it is never moved again for nothing.
    #[test]
    fn in_split_the_tone_is_kept_in_the_middle_of_the_transmit_filter() {
        for (rx, b) in [((14_074_000.0, false), 14_074_000), ((7_074_000.0, true), 7_074_000)] {
            let r = rig(rx.0 as u64, Some(b), Some(true));
            for off in [1000.0, 1500.0, 2000.0] {
                assert!(matches!(audio_plan(&r, rx, off), Some(AudioPlan::Go { .. })), "{off}");
            }
            for off in [200.0, 990.0, 2010.0, 2760.0, 2900.0] {
                let Some(AudioPlan::MoveB { dial, audio }) = audio_plan(&r, rx, off) else {
                    panic!("{off}: not moved");
                };
                // Read back there: goes, on that VFO B, mid filter.
                let moved = rig(rx.0 as u64, Some(dial), Some(true));
                assert_eq!(
                    audio_plan(&moved, rx, off),
                    Some(AudioPlan::Go { audio, split_dial: Some(dial) })
                );
                assert!((1250.0..=1750.0).contains(&audio), "{off}: {audio}");
            }
        }
    }

    fn with_tb(mut r: RigStatus, body: &str) -> RigStatus {
        r.values.insert("TB".into(), body.into());
        r
    }

    /// Decision 125: out of split AUTO keeps under the radio's transmit
    /// filter's top as read (TX BW 100-2700: tone 0 at most 2350 Hz), as an
    /// offset from the receive dial; in split, or with TX BW unread or 3500
    /// and wider, it has the whole range.
    #[test]
    fn auto_range_follows_tx_bw_out_of_split() {
        let rx = Some((14_074_000.0, false));
        let r = with_tb(rig(14_074_000, None, Some(false)), "001");
        let a = auto_range(Some(&r), rx);
        assert_eq!((a.lo, a.top_tone()), (200.0, 2350.0));
        // The receiver 300 Hz above VFO A: the tone is the offset plus 300.
        let a = auto_range(Some(&r), Some((14_074_300.0, false)));
        assert_eq!(a.top_tone(), 2050.0);
        // LSB, the receiver 300 Hz below VFO A: the same.
        let l = with_tb(rig(7_074_000, None, Some(false)), "001");
        assert_eq!(auto_range(Some(&l), Some((7_073_700.0, true))).top_tone(), 2050.0);
        assert_eq!(auto_range(Some(&with_tb(r.clone(), "003")), rx), freq::FULL);
        // 300-4000: the top as before, the bottom kept 100 Hz over 300.
        assert_eq!(
            auto_range(Some(&with_tb(r.clone(), "012")), rx),
            freq::Range { lo: 400.0, hi: freq::HI_HZ }
        );
        assert_eq!(auto_range(Some(&rig(14_074_000, None, Some(false))), rx), freq::FULL);
        assert_eq!(auto_range(Some(&with_tb(r.clone(), "?")), rx), freq::FULL);
        assert_eq!(auto_range(None, rx), freq::FULL);
        let sp = with_tb(rig(14_074_000, Some(14_074_000), Some(true)), "001");
        assert_eq!(auto_range(Some(&sp), rx), freq::FULL);
        // TX BW 100-3000 caps at 2650.
        assert_eq!(auto_range(Some(&with_tb(r, "002")), rx).top_tone(), 2650.0);
    }

    /// Decision 125: a tone set by hand past what TX BW passes cleanly is
    /// named, with menu 35, in the window's warning colour.
    #[test]
    fn a_tone_past_the_transmit_filter_is_warned_of() {
        let r = with_tb(rig(14_074_000, None, Some(false)), "001");
        let go = |a: f32| AudioPlan::Go { audio: a, split_dial: None };
        let n = filter_note(Some(&r), go(2760.0)).unwrap();
        assert!(n.contains("menu 35") && n.contains("100-2700") && n.contains("2350"), "{n}");
        assert!(filter_note(Some(&r), go(2350.0)).is_none());
        assert!(filter_note(Some(&r), go(1500.0)).is_none());
        // Low edge: 300-2700 wants tone 0 from 400 Hz.
        assert!(filter_note(Some(&with_tb(r.clone(), "009")), go(350.0)).unwrap().contains("below 400"));
        // Wide, or not read: nothing to say.
        assert!(filter_note(Some(&with_tb(r.clone(), "003")), go(2760.0)).is_none());
        assert!(filter_note(Some(&rig(14_074_000, None, None)), go(2760.0)).is_none());
        assert!(filter_note(None, go(2760.0)).is_none());
    }
}

#[cfg(test)]
mod round5_tests {
    use super::*;

    const T: i64 = 1_800_000_000;

    fn row(text: &str, slot: i64, snr: i16, audio: f32) -> ListRow {
        ListRow {
            d: msg::parse(text, slot, snr, 0.1, audio),
            row: 1,
            rf_hz: 0.0,
            country: None,
            flag: "",
            continent: None,
            cq_zone: None,
            itu_zone: None,
            path: None,
            badges: Badges::default(),
            to_me: false,
            cq_ok: true,
            ours: false,
        }
    }

    /// One frame at `now`, the radio unkeyed, with `feed` decoded.
    fn frame(tx: &mut Tx, s: &mut TxSettings, now: f64, feed: Vec<TxHeard>) -> TxOut {
        let amp = AmpStatus::default();
        let mut outside = crate::outside::Outside::default();
        let station = StationSettings::default();
        let heard = station::Heard::default();
        let rig = RigStatus::default();
        let env = Env {
            now,
            link: None,
            rig: Some(&rig),
            amp: &amp,
            device: None,
            outside: &mut outside,
            rx_dial: Some((14_074_000.0, false)),
            bar_off: None,
            bar_moved: false,
            bar_ours: true,
            station: &station,
            decoding_ft8: true,
            slot_decoded: None,
            scope_mode: None,
        };
        tx.frame(s, env, feed, &heard, &|_, _| Badges::default())
    }

    #[test]
    fn reply_mid_qso_leaves_the_station_in_hand_for_the_new_one() {
        let mut tx = Tx::default();
        let s = TxSettings::default();
        tx.qso.cfg.my_call = "AJ3G".into();
        tx.reply(&s, &row("CQ K1ABC FN42", T + 15, -12, 800.0), (T + 30) as f64);
        assert_eq!(tx.qso.dx_call(), Some("K1ABC"));
        assert!(tx.qso.busy());
        // Our call to K1ABC is on the air when W9XYZ is picked.
        tx.over = Some(Over {
            mode: TxMode::Ft8,
            slot_utc: T + 30,
            text: "K1ABC AJ3G FN20".into(),
            audio_hz: 1500.0,
            asked: Instant::now(),
            keyed_seen: true,
            end_utc: (T + 30) as f64 + 13.1,
        });
        tx.reply(&s, &row("CQ W9XYZ EN52", T, -7, 1200.0), (T + 35) as f64);
        // W9XYZ is worked at once, in the slot after theirs, with their
        // report; K1ABC is not queued again.
        assert_eq!(tx.qso.dx_call(), Some("W9XYZ"));
        assert_eq!(tx.qso.step(), qso::Step::TxGrid);
        assert_eq!(tx.qso.reports().0, Some(-7));
        assert!(!tx.tx_even);
        assert_eq!(tx.qso.queue().count(), 0);
        assert_eq!(
            tx.qso.transcript().last().map(|l| (l.note, l.text.as_str())),
            Some((true, "Left K1ABC for W9XYZ"))
        );
        // The scope's mark follows.
        assert_eq!(tx.dx_mark().map(|m| (m.0, m.1)), Some(("W9XYZ".into(), 1200.0)));
        // The over to K1ABC runs out: it is not a call to W9XYZ.
        let mut s = TxSettings::default();
        frame(&mut tx, &mut s, (T + 44) as f64, Vec::new());
        assert!(tx.over.is_none());
        assert_eq!(tx.qso.dx_call(), Some("W9XYZ"));
        assert_eq!(tx.qso.calls(), 0);
        // + still queues, leaving W9XYZ in hand.
        tx.queue(&row("CQ N0DEF EM48", T + 45, -3, 600.0));
        assert_eq!(tx.qso.dx_call(), Some("W9XYZ"));
        assert_eq!(tx.qso.queue().map(|q| q.call.as_str()).collect::<Vec<_>>(), vec!["N0DEF"]);
    }

    #[test]
    fn reply_to_the_station_in_hand_or_when_idle_leaves_no_line() {
        let mut tx = Tx::default();
        let s = TxSettings::default();
        tx.qso.cfg.my_call = "AJ3G".into();
        tx.reply(&s, &row("CQ K1ABC FN42", T + 15, -12, 800.0), (T + 30) as f64);
        tx.reply(&s, &row("CQ K1ABC FN42", T + 45, -10, 800.0), (T + 60) as f64);
        assert!(tx.qso.transcript().iter().all(|l| !l.text.starts_with("Left")));
        tx.qso.stop();
        tx.reply(&s, &row("CQ W9XYZ EN52", T + 45, -7, 1200.0), (T + 60) as f64);
        assert_eq!(tx.qso.dx_call(), Some("W9XYZ"));
        assert!(tx.qso.transcript().iter().all(|l| !l.text.starts_with("Left")));
    }

    fn heard(slot: i64, audio: f32, snr: f32) -> TxHeard {
        TxHeard {
            slot_utc: slot,
            snr_db: snr,
            dt: 0.1,
            audio_hz: audio,
            text: "CQ K1ABC FN42".into(),
            rf_hz: 0.0,
            row: 0,
        }
    }

    #[test]
    fn auto_remembers_strong_stations_for_five_minutes() {
        let mut tx = Tx::default();
        let mut s = TxSettings { freq: FreqMode::Auto, audio_hz: 1500.0, ..Default::default() };
        tx.tx_even = true;
        // Two in our (even) period: strong at 1000 Hz, weak at 2000 Hz.
        frame(
            &mut tx,
            &mut s,
            (T + 14) as f64,
            vec![heard(T, 1000.0, -2.0), heard(T, 2000.0, -15.0)],
        );
        let at =
            |o: &TxOut, hz: f32| o.busy.iter().find(|b| b.0 <= hz && hz < b.1).map_or(0.0, |b| b.2);
        let o = frame(&mut tx, &mut s, (T + 100) as f64, Vec::new());
        assert!(at(&o, 1020.0) > 0.0 && at(&o, 2020.0) > 0.0, "{:?}", o.busy);
        assert_eq!(at(&o, 1500.0), 0.0);
        // After 2 minutes the weak one is gone; the strong one stays to 5,
        // counting less than a fresh one.
        let o = frame(&mut tx, &mut s, (T + 220) as f64, vec![heard(T + 210, 500.0, -2.0)]);
        assert_eq!(at(&o, 2020.0), 0.0);
        assert!(at(&o, 1020.0) > 0.0 && at(&o, 1020.0) < at(&o, 520.0), "{:?}", o.busy);
        let o = frame(&mut tx, &mut s, (T + 301) as f64, Vec::new());
        assert_eq!(at(&o, 1020.0), 0.0);
        assert!(at(&o, 520.0) > 0.0);
        // The other period is not counted.
        tx.tx_even = false;
        assert!(frame(&mut tx, &mut s, (T + 302) as f64, Vec::new()).busy.is_empty());
    }

    #[test]
    fn auto_steers_around_undecoded_energy_in_our_period() {
        let mut tx = Tx::default();
        let mut s = TxSettings { freq: FreqMode::Auto, audio_hz: 1500.0, ..Default::default() };
        tx.tx_even = true;
        // A slot (even) with a strong undecoded signal at 1480 Hz, heard by
        // the receiver's audio.
        let rate = sdr::AUDIO_RATE;
        let a: Vec<f32> = (0..(16.0 * rate) as usize)
            .map(|i| {
                let t = i as f64 / rate;
                let n = ((i as u64).wrapping_mul(6364136223846793005) >> 41) as f32
                    / (1u64 << 23) as f32
                    - 0.5;
                0.2 * n + 0.1 * (std::f64::consts::TAU * 1490.0 * t).sin() as f32
            })
            .collect();
        tx.occ.push(&a, T as f64);
        let o = frame(&mut tx, &mut s, (T + 16) as f64, Vec::new());
        assert!(
            o.busy.iter().any(|&(a, b, v)| a <= 1490.0 && b >= 1490.0 && v > 0.0),
            "{:?}",
            o.busy
        );
        assert!(!(1430.0..1500.0).contains(&s.audio_hz), "AUTO stayed on it at {}", s.audio_hz);
        // Only in our own period.
        tx.tx_even = false;
        assert!(frame(&mut tx, &mut s, (T + 17) as f64, Vec::new()).busy.is_empty());
    }
}
