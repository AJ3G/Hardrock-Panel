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
pub(crate) mod qso;
pub(crate) mod sched;
pub(crate) mod wave;
pub(crate) mod window;

pub(crate) use window::window;

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant, UNIX_EPOCH};

use hardrock::AmpStatus;
use rigcat::{Keying, RigLink, RigStatus};
use serde::{Deserialize, Serialize};

use crate::audio;
use crate::decode::TxHeard;
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
    /// Ask before logging, rather than logging at once.
    pub prompt_log: bool,
    pub sort: Sort,
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
            rrr: false,
            cq: "CQ {MYCALL} {MYGRID}".into(),
            macros: vec!["TNX 73 GL".into(), "QRZ?".into(), "TU 73".into()],
            watchdog_min: 6,
            max_calls: 10,
            wspr_pct: 20,
            wspr_dbm: 37,
            wspr_roam: true,
            wspr_audio_hz: 1500.0,
            prompt_log: false,
            sort: Sort::Time,
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
    /// The busy parts AUTO steers around, audio Hz from the receiver's dial.
    pub busy: Vec<(f32, f32)>,
    /// The QSO under way, for GLOBE: call, grid, keyed now.
    pub strobe: Option<(String, Option<String>, bool)>,
    /// DECODE's row to pick (a row clicked here).
    pub pick_row: Option<u64>,
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
    pub station: &'a StationSettings,
    /// DECODE is running FT8.
    pub decoding_ft8: bool,
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
    /// (slot, audio Hz) of signals heard, for AUTO.
    heard: VecDeque<(i64, f32)>,
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
    /// The WSPR offset this beacon went at.
    wspr_hz: f32,
    /// Seed for ROAM.
    roam: u64,
    /// The dial (radio Hz) the list belongs to: a band change clears it.
    list_dial: Option<f64>,
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
            heard: VecDeque::new(),
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
            wspr_hz: 1500.0,
            roam: 0x9e37_79b9_7f4a_7c15,
            list_dial: None,
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

    /// The mode of the over in progress, for the interlocks.
    pub(crate) fn over_mode(&self) -> Option<TxMode> {
        self.over.as_ref().map(|o| o.mode)
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

    /// REPLY (or a double-click) on a station: work it now, or queue it at
    /// the front when a QSO is under way.
    pub(crate) fn reply(&mut self, s: &TxSettings, r: &ListRow, now: f64) {
        let Some(call) = r.d.from.clone() else { return };
        let busy_with_other = self.qso.busy() && self.qso.dx_call() != Some(call.as_str());
        let wait = !r.d.is_cq && r.d.to.is_some() && !r.to_me;
        if busy_with_other {
            self.qso.queue_front(qso::Queued {
                call,
                grid: r.d.grid.clone(),
                snr_db: r.d.snr_db,
                audio_hz: r.d.audio_hz,
                heard_utc: r.d.slot_utc,
                wait_for_cq: wait,
            });
            return;
        }
        self.tx_even = !sched::is_even(r.d.slot_utc, sched::FT8_PERIOD);
        self.report = r.d.snr_db;
        self.qso.start_qso(&call, r.d.grid.clone(), r.d.snr_db, wait, now as i64);
        self.hand = None;
        self.generate(s);
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
    pub(crate) fn keep(&mut self, amp: &AmpStatus, now: f64) -> Option<Option<String>> {
        let Some(o) = self.over.as_mut() else {
            return Some(Some("the TX window has no over going out".into()));
        };
        o.keyed_seen = true;
        if !self.player.playing() {
            return Some(Some("the transmit audio stopped".into()));
        }
        if o.mode == TxMode::Wspr
            && let Some(why) = too_hot(amp)
        {
            return Some(Some(why));
        }
        (self.player.finished() || now > o.end_utc + 2.0 * TAIL_S).then_some(None)
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
        self.configure(s, env.station);
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
                let done = self.player.finished() || now >= o.end_utc - 0.5;
                self.player.stop();
                let mode = o.mode;
                self.over = None;
                if done {
                    if mode == TxMode::Ft8 && self.hand.is_none() {
                        self.qso.note_tx_sent(now as i64);
                    }
                } else {
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
                self.qso.start_qso(&q.call, q.grid.clone(), q.snr_db, q.wait_for_cq, now as i64);
                self.generate(s);
            }
        }
        if self.report != self.qso.reports().0.unwrap_or(self.report) {
            self.qso.set_report(self.report);
        }
        // The transmit audio: the bar, AUTO's pick, or the box.
        if env.bar_moved && s.freq == FreqMode::Auto {
            s.freq = FreqMode::Manual;
        }
        if let Some(b) = env.bar_off.filter(|_| self.over.is_none())
            && s.freq != FreqMode::Auto
            && (100.0..=3000.0).contains(&b)
        {
            s.audio_hz = b as f32;
        }
        let ours: Vec<f32> = self
            .heard
            .iter()
            .filter(|(t, _)| sched::is_even(*t, sched::FT8_PERIOD) == self.tx_even)
            .map(|(_, a)| *a)
            .collect();
        if s.mode == TxMode::Ft8 && s.freq == FreqMode::Auto {
            out.busy = freq::busy(&ours);
            // Only between overs, and not in the middle of an exchange,
            // so the station we work finds us where they answered.
            if self.over.is_none() && !(self.qso.busy() && self.auto_hz.is_some()) {
                let cur = self.auto_hz.unwrap_or(s.audio_hz);
                if let Some(p) = freq::pick(&ours, cur) {
                    self.auto_hz = Some(p);
                    s.audio_hz = p;
                }
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
        // GLOBE's strobe.
        if s.mode == TxMode::Ft8
            && let Some(c) = self.qso.dx_call().filter(|_| self.qso.busy() || self.on_air())
        {
            out.strobe = Some((c.to_string(), self.qso.dx_grid().map(str::to_string), self.on_air()));
        }
        // Start an over?
        if self.over.is_none() && self.enabled {
            match self.try_start(s, env, &me) {
                Ok(true) => self.note = None,
                Ok(false) => {}
                Err(why) => self.note = Some(why),
            }
        }
        out
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
            // A band change (or a big move) clears the list.
            if self.list_dial.is_some_and(|d| (d - dial).abs() > 5_000.0) {
                self.rows.clear();
                self.heard.clear();
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
            self.heard.push_back((h.slot_utc, h.audio_hz));
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
        let newest = self.heard.back().map_or(0, |h| h.0);
        while self.heard.front().is_some_and(|h| h.0 < newest - 120) {
            self.heard.pop_front();
        }
        dts.sort_by(f32::total_cmp);
        self.dt = dts.get(dts.len() / 2).copied();
        if s.auto_seq && s.mode == TxMode::Ft8 && self.qso.on_rx(&decodes, env.now as i64) {
            self.take_completed(s.prompt_log);
            if let Some(c) = self.qso.dx_call().map(str::to_string)
                && self.lines[0].split_whitespace().next() != Some(c.as_str())
            {
                if let Some(e) = self.parity.get(&c) {
                    self.tx_even = !e;
                }
                self.generate(s);
            }
        }
    }

    /// Start an over if one is due: `Ok(true)` when keyed, `Ok(false)`
    /// when none is due, `Err` why one due can't go.
    fn try_start(&mut self, s: &TxSettings, env: Env<'_>, me: &str) -> Result<bool, String> {
        let now = env.now;
        if me.is_empty() {
            return Err("Set your callsign under SETTINGS, Station.".into());
        }
        let (t0, message, text, start_s, len_s) = match s.mode {
            TxMode::Ft8 => {
                let Some(t0) = sched::ft8_open(now, self.tx_even) else { return Ok(false) };
                if self.sent_slot == Some(t0) {
                    return Ok(false);
                }
                let plan = if s.auto_seq { self.qso.plan_tx() } else { self.hand.clone() };
                let Some(text) = plan else { return Ok(false) };
                if !env.decoding_ft8 {
                    return Err("DECODE isn't running FT8, so the sequencer can't hear replies.".into());
                }
                let Some(off) = self.tx_audio(s, &env) else {
                    return Err(self.audio_error(s, &env));
                };
                let m = wave::Message::ft8(&text, off)
                    .ok_or_else(|| format!("\"{text}\" can't be sent as FT8."))?;
                (t0, m, text, wave::FT8_START_S, wave::FT8_LEN_S)
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
                let m = wave::Message::wspr(me, &grid, s.wspr_dbm, self.wspr_hz).ok_or_else(|| {
                    "WSPR Type 1 needs a plain call, a 4-character grid and one of its power levels."
                        .to_string()
                })?;
                let text = format!("{me} {} {}", &grid.get(..4).unwrap_or(""), s.wspr_dbm);
                (t0, m, text, wave::WSPR_START_S, wave::WSPR_LEN_S)
            }
        };
        // This slot is spent, whatever happens below.
        self.sent_slot = Some(t0);
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
        link.tune_start(keyed_for, Keying::Data);
        let audio_hz = match &message {
            wave::Message::Ft8 { audio_hz, .. } | wave::Message::Wspr { audio_hz, .. } => *audio_hz,
        };
        if s.mode == TxMode::Ft8 {
            self.qso.record_tx(&text, t0);
            self.rows.push_front(ListRow {
                d: msg::parse(&text, t0, 0, 0.0, audio_hz),
                row: 0,
                rf_hz: 0.0,
                country: None,
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
    /// the radio may transmit from VFO B in split.
    fn tx_audio(&self, s: &TxSettings, env: &Env<'_>) -> Option<f32> {
        let r = env.rig?;
        let (rx, lower) = env.rx_dial.or_else(|| r.freq_hz.map(|f| (f as f64, r.mode == Some("LSB"))))?;
        let tx = if r.split == Some(true) { r.freq_b_hz? } else { r.freq_hz? } as f64;
        let off = s.audio_hz as f64;
        let a = if lower { tx - rx + off } else { rx + off - tx };
        (100.0..=2950.0).contains(&a).then_some(a.round() as f32)
    }

    fn audio_error(&self, s: &TxSettings, env: &Env<'_>) -> String {
        match env.rig {
            None => "The radio isn't connected.".into(),
            Some(r) if r.freq_hz.is_none() => "The radio's dial hasn't been read yet.".into(),
            Some(_) => format!(
                "The radio's transmit dial is too far from the receiver for {:.0} Hz: turn TX \
                 follows RX on, or move the bar.",
                s.audio_hz
            ),
        }
    }
}

/// SAFETY INTERLOCK (docs/safety-interlocks.md, #12): a WSPR beacon keys
/// the amplifier for 110.6 s, so none starts, and one going out stops,
/// while its temperature is in the warning range (AJ3G, 10 Nov 2026).
pub(crate) fn too_hot(amp: &AmpStatus) -> Option<String> {
    let t = amp.readings.filter(|_| amp.connected)?.known_temp_c()?;
    let warn = amp.model.limits().temp_warn_c;
    (t >= warn).then(|| format!("The amplifier is at {t:.0} °C, in its warning range (from {warn:.0} °C)."))
}
