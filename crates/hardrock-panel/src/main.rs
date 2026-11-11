//! Hardrock Panel: a standalone control panel for a HobbyPCB Hardrock-500
//! amplifier and its ATU-500 tuner, on the amplifier's USB serial port.
//!
//! It is the AMP window from SDRoxide, running on its own. In SDRoxide the
//! radio's own readings and its dial came from SDRoxide; here they come from
//! the radio's CAT port directly, read only ([`rigcat`]), when one is set up:
//!
//! - **Readings in bypass.** In OFF the amplifier reports no FWD, RFL or SWR.
//!   With the FDM-DUO connected they are the radio's own (`FP`, `RP`, `WR`), as
//!   in SDRoxide; without it, FWD shows the amplifier's DRV, the radio's power
//!   passing straight through (`docs/design-decisions.md`, decisions 2 and 7).
//! - **Band.** With the radio connected the amplifier's band can follow its
//!   dial, between transmissions only (interlock 3). Without it the band is
//!   switched here (DN/UP) or by the radio over the amplifier's own CAT or ACC
//!   input, and the panel says so along its top.
//!
//! - **Scope, receiver and decoders.** With the ELAD's USB I/Q (or the demo
//!   band) chosen under SETTINGS, SCOPE shows the panadapter and runs the
//!   panel's own receiver, which plays on the chosen sound output and, in
//!   split, sets where the radio transmits (decision 22); DECODE runs the
//!   panel's decoders on it (decisions 26 to 28, 32).
//! - **Outside programs.** TCI and rigctld servers, and WSJT-X UDP, off until
//!   turned on under SETTINGS (decision 25, interlock #8).
//!
//! The safety interlocks are listed in `docs/safety-interlocks.md`.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// SDRoxide's look, carried over whole rather than trimmed, so that a change to
// it there can be copied across as it stands. The panel uses only part of it.
#[allow(dead_code, unused_imports)]
mod chrome;
#[allow(dead_code, unused_imports)]
mod layout;
#[allow(dead_code, unused_imports)]
mod repaint;
#[allow(dead_code, unused_imports)]
mod style;
#[allow(dead_code, unused_imports)]
mod theme;

mod audio;
mod decode;
mod decodelog;
mod dxcc;
mod entity_flags;
mod flags;
mod globe;
mod gpu_gate;
mod gpuload;
mod logbook;
mod logfile;
mod outside;
mod panel;
mod popout;
mod radio;
mod rx;
// The panadapter scope. Some of its API is for the example and the tests.
#[allow(dead_code)]
mod scope;
mod scopewin;
mod settings;
mod skimlog;
mod slices;
mod slotclock;
mod spots;
mod station;
mod tune;
mod tx;

use eframe::egui::{self, RichText};
use eframe::egui_wgpu::RenderState;
use hardrock::{AmpAction, AmpConfig, AmpLink, AmpReadings, AmpStatus, Band};
use rigcat::{Keying, RigConfig, RigLink, RigStatus};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::{Duration, Instant};

use panel::{AmpMeters, TxMeters};

/// The app's name, which is also the folder eframe keeps its settings in
/// ([`eframe::storage_dir`]); the log goes there too.
const APP_NAME: &str = "Hardrock Panel";

/// Where eframe keeps what's saved between runs.
const STORAGE_KEY: &str = "hardrock-panel";

/// The SWR warning SDRoxide gives with its default settings: its transmitter's
/// tune limit, 5:1.
const DEFAULT_SWR_WARN: f32 = 5.0;

/// Everything kept from one run to the next. The window's size and place are
/// kept by eframe itself.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    amp: AmpConfig,
    rig: RigConfig,
    swr_warn: f32,
    /// How long TUNE may run, in seconds.
    tune_secs: u32,
    /// The Tune slider: TUNE's level within the radio's power setting, 0 to 1.
    drive: f32,
    /// The sound output TUNE's tone plays on; `None` picks the radio's own
    /// USB audio by its name.
    audio_out: Option<String>,
    /// The radio's window was popped out into its own.
    radio_popped: bool,
    /// The amplifier's panel was popped out into its own (AJ3G, 25 Oct 2026).
    amp_popped: bool,
    /// The meters' needle picks (see [`AmpMeters::saved`]).
    meters: Option<[u8; 4]>,
    /// The panel's receiver and where its I/Q comes from (decision 22).
    rx: rx::RxSettings,
    /// The scope's display settings, and whether it was popped out.
    scope: scope::ScopeSettings,
    scope_popped: bool,
    decode: decode::DecodeSettings,
    /// DECODE was popped out into its own window.
    decode_popped: bool,
    /// TCI, rigctld and WSJT-X UDP (decision 25).
    outside: outside::OutsideSettings,
    /// SETTINGS → Station: callsign, grid, distance unit.
    station: station::StationSettings,
    /// GLOBE's layers, view and sources (decision 65).
    globe: globe::GlobeSettings,
    /// The TX window (AJ3G, 10 Nov 2026), and whether it was open.
    tx: tx::TxSettings,
    show_tx: bool,
    /// LOG and SETTINGS → Logbook (decision 69).
    log: logbook::LogSettings,
}

impl Default for Saved {
    fn default() -> Self {
        Saved {
            amp: AmpConfig::default(),
            rig: RigConfig::default(),
            swr_warn: DEFAULT_SWR_WARN,
            tune_secs: tune::DEFAULT_TUNE_SECS,
            drive: tune::DEFAULT_DRIVE,
            audio_out: None,
            radio_popped: false,
            amp_popped: false,
            meters: None,
            rx: rx::RxSettings::default(),
            scope: scope::ScopeSettings::default(),
            scope_popped: false,
            decode: decode::DecodeSettings::default(),
            decode_popped: false,
            outside: outside::OutsideSettings::default(),
            station: station::StationSettings::default(),
            globe: globe::GlobeSettings::default(),
            tx: tx::TxSettings::default(),
            show_tx: false,
            log: logbook::LogSettings::default(),
        }
    }
}

struct App {
    saved: Saved,
    link: Option<AmpLink>,
    /// Shared (weakly) with the outside programs' links, which unkey through
    /// it at once.
    rig: Option<Arc<RigLink>>,
    view: AmpMeters,
    show_settings: bool,
    /// The amplifier's panel, docked as the main window's body or popped out.
    /// Shown at start; the AMP key on the bar hides it (AJ3G, 25 Oct 2026).
    show_amp: bool,
    show_radio: bool,
    /// The settings as being edited, applied with APPLY.
    draft: AmpConfig,
    draft_rig: RigConfig,
    ports: Vec<String>,
    /// The band last sent to the amplifier from the radio's dial, and whether
    /// the amplifier was connected then, so a reconnect sends it again.
    band_told: Option<Band>,
    amp_was_connected: bool,
    /// A band button pressed here, when, and the band the amplifier was on.
    band_press: Option<(std::time::Instant, Option<u8>)>,
    /// The last band press here was undone by the amplifier: its band is
    /// set by the radio's cable (decision 13).
    band_held: bool,
    /// The operator dismissed the band-held note; it stays away until a
    /// band press here sticks again.
    band_dismissed: bool,
    /// TUNE's tone, and the sound outputs it can play on.
    tone: audio::Tone,
    sounds: Vec<String>,
    /// When the tone was started for a TUNE not yet keyed.
    tone_asked: Option<Instant>,
    /// TUNE was started with the amplifier showing a trip, which keying
    /// clears; it doesn't stop that TUNE until it has cleared (#6).
    trip_carried: bool,
    /// TX IN has been put on USB audio for this connection (decision 100).
    usb_audio_set: bool,
    /// The amplifier's last good readings, which an over in progress rides
    /// a short silence on (#6).
    amp_hold: tune::AmpHold,
    /// TX TUNE's and RIG TUNE's SWR rule (interlock #6): the run of high
    /// readings it is timing.
    tune_swr: tune::TuneSwr,
    /// The over (by when it was keyed) whose stop has been logged, so each
    /// stop's reason goes in the log once.
    stop_logged: Option<Instant>,
    /// Why the tone wouldn't play, last time.
    tone_error: Option<String>,
    /// Since when the radio has been seen transmitting.
    rig_keyed_since: Option<Instant>,
    /// What says the station is transmitting, as last logged, and since when.
    tx_sources: (String, Option<Instant>),
    render_state: Option<RenderState>,
    scope: scope::Scope,
    rx: rx::Rx,
    show_scope: bool,
    show_decode: bool,
    show_globe: bool,
    globe: globe::Globe,
    /// The logbook (decision 69), and the LOG window.
    logbook: logbook::Logbook,
    log_ui: logbook::LogUi,
    show_log: bool,
    decode: decode::Decode,
    /// TRANSMIT: FT8 and WSPR, the QSO sequencer and its decode list.
    tx: tx::Tx,
    outside: outside::Outside,
    /// The I/Q source as being chosen, applied with APPLY.
    draft_source: rx::IqChoice,
    draft_outside: outside::OutsideSettings,
    elad_devices: Vec<elad_iq::EladDevice>,
    /// The receiver is to be (re)started, asked when.
    rx_restart: Option<Instant>,
    /// The transverter offset last seen, to keep the radio window's and the
    /// scope's the same.
    xvtr_seen: Option<i64>,
    /// The look the command line asks for (`--theme`, or `--demo`'s ELAD
    /// look), over the one the chosen radio gives (decision 39).
    forced_look: Option<theme::StationLook>,
}

/// How long the radio is given to show a TUNE keyed before its tone is
/// stopped again, and to be seen transmitting before the amplifier's PTT is
/// looked at.
const KEY_SETTLE: Duration = Duration::from_millis(1500);

/// How long after DN or UP the amplifier is given to show the new band. The
/// HR500 reads a radio's band lines three or four times a second, so a band
/// the cable holds is back well inside this.
const BAND_SETTLE: std::time::Duration = std::time::Duration::from_millis(1500);

impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        theme::apply(&cc.egui_ctx);
        let mut saved: Saved =
            cc.storage.and_then(|s| eframe::get_value(s, STORAGE_KEY)).unwrap_or_default();
        apply_args(&mut saved.amp, &mut saved.rig, std::env::args().skip(1));
        let view_args = ViewArgs::parse(std::env::args().skip(1));
        if view_args.demo {
            saved.rx.source = rx::IqChoice::Demo;
        } else if view_args.elad {
            saved.rx.source = rx::IqChoice::Elad;
        }
        let mut view = AmpMeters::default();
        if let Some(m) = saved.meters {
            view.restore(m);
        }
        let link = saved.amp.enabled.then(|| AmpLink::start(saved.amp.clone()));
        let rig = saved.rig.enabled.then(|| Arc::new(RigLink::start(saved.rig.clone())));
        // Straight to the settings on the first run, when there is nothing to
        // connect to yet.
        let show_settings = !saved.amp.enabled && !view_args.demo;
        let render_state = cc.wgpu_render_state.clone();
        let scope = scope::Scope::new(render_state.as_ref(), saved.scope.clone());
        let rx = rx::Rx::new(&saved.rx);
        // The raw I/Q onto the GPU for its spectrum, on the pipeline's thread.
        if let Some(sink) = scope.gpu_iq_sink() {
            rx.taps.set_iq("scope-gpu", Some(Box::new(move |b| sink(b.iq, b.center, b.rate))));
        }
        let mut outside = outside::Outside::default();
        outside.apply(&saved.outside, &rx.taps);
        App {
            render_state,
            scope,
            rx,
            show_scope: view_args.demo || view_args.elad || view_args.scope,
            show_decode: view_args.decode,
            show_globe: false,
            globe: globe::Globe::default(),
            logbook: logbook::Logbook::load_default(),
            log_ui: logbook::LogUi::default(),
            show_log: false,
            decode: decode::Decode::default(),
            tx: tx::Tx::default(),
            outside,
            draft_source: saved.rx.source,
            draft_outside: saved.outside.clone(),
            elad_devices: Vec::new(),
            rx_restart: Some(Instant::now()),
            xvtr_seen: None,
            forced_look: view_args.look(),
            draft: saved.amp.clone(),
            draft_rig: saved.rig.clone(),
            saved,
            link,
            rig,
            view,
            show_settings,
            show_amp: true,
            show_radio: false,
            ports: serial_ports(),
            band_told: None,
            amp_was_connected: false,
            band_press: None,
            band_held: false,
            band_dismissed: false,
            tone: audio::Tone::default(),
            sounds: audio::output_devices(),
            trip_carried: false,
            usb_audio_set: false,
            amp_hold: tune::AmpHold::default(),
            tune_swr: tune::TuneSwr::default(),
            stop_logged: None,
            tone_asked: None,
            tx_sources: (String::new(), None),
            tone_error: None,
            rig_keyed_since: None,
        }
    }

    /// The sound output TUNE's tone plays on: the one chosen, or the radio's
    /// own USB audio.
    fn tone_device(&self) -> Option<String> {
        self.saved.audio_out.clone().or_else(|| audio::radio_device(&self.sounds))
    }

    fn tone_gain(&self, r: &RigStatus) -> f32 {
        audio::gain_for(self.saved.drive, r.audio_ceiling)
    }

    /// What the TUNE strip shows, for both of its copies.
    fn tune_args(&mut self, amp: &AmpStatus, r: &RigStatus) -> tune::TuneArgs<'_> {
        let dev = self.tone_device();
        let tone = tune::planned_watts(r, Keying::Audio, self.saved.drive);
        let carrier = tune::planned_watts(r, Keying::Carrier, self.saved.drive);
        tune::TuneArgs {
            tone_blocked: tune::tone_blocked(r, dev.as_deref())
                .or_else(|| tune::tune_blocked(amp, r, tone)),
            carrier_blocked: tune::tune_blocked(amp, r, carrier),
            limit_in_line: limit_in_line(amp),
            secs: &mut self.saved.tune_secs,
            drive: &mut self.saved.drive,
            tone_error: self.tone_error.clone(),
            panel_vol: (!self.rx.sound_via_radio(&self.saved.rx)).then_some(self.saved.rx.volume),
            rx_dbm: self.rx.meter_dbm(r),
        }
    }

    fn status(&self) -> AmpStatus {
        match &self.link {
            Some(l) => l.status(),
            None => AmpStatus { model: self.saved.amp.model, ..AmpStatus::default() },
        }
    }

    /// Save the settings and (re)open the port. The old link is dropped first,
    /// which closes the port, so the new one can open it.
    fn apply(&mut self) {
        self.saved.amp = self.draft.clone();
        self.link = None;
        if self.saved.amp.enabled {
            self.link = Some(AmpLink::start(self.saved.amp.clone()));
        }
        self.saved.rig = self.draft_rig.clone();
        self.rig = None;
        self.tone.stop();
        if self.saved.rig.enabled {
            self.rig = Some(Arc::new(RigLink::start(self.saved.rig.clone())));
        }
        self.band_told = None;
        if self.draft_source != self.saved.rx.source || self.rx.error.is_some() {
            self.saved.rx.source = self.draft_source;
            self.rx.stop();
            self.rx_restart = Some(Instant::now());
        }
        self.saved.outside = self.draft_outside.clone();
        self.outside.apply(&self.saved.outside, &self.rx.taps);
    }

    /// While TUNE is keyed, tell the link the panel still wants it, or stop
    /// it if interlock #6 says so. Keep the tone at the Tune slider's level,
    /// and stop it once TUNE has unkeyed.
    fn keep_tune(&mut self, amp: &AmpStatus, rig: Option<&RigStatus>) {
        let outside = self.outside.holder().is_some();
        let tuning = rig.and_then(|r| r.tuning);
        let waiting = self.tone_asked.is_some_and(|t| t.elapsed() < KEY_SETTLE);
        if tuning.is_some() {
            self.tone_asked = None;
        } else if !waiting && self.tone.playing() {
            // After the link's unkey, never before it.
            self.tone.stop();
        }
        if !amp.alerts.tripped {
            // Cleared, by this keying or otherwise: any trip from now is new.
            self.trip_carried = false;
        }
        let now = Instant::now();
        self.amp_hold.note(amp, now);
        if !tuning.is_some_and(|t| matches!(t.keying, Keying::Audio | Keying::Carrier)) {
            self.tune_swr.reset();
        }
        let (Some(link), Some(r), Some(t)) = (&self.rig, rig, tuning) else { return };
        // SAFETY INTERLOCK (docs/safety-interlocks.md, #6): an over in
        // progress is judged by the amplifier's last readings for up to 5 s
        // of silence (the HR500 skips replies while keyed), and stopped
        // after that; see `tune::AmpHold`.
        let what = match t.keying {
            _ if outside => "outside program's over",
            Keying::Data => "TX window over",
            Keying::Audio => "panel TUNE",
            Keying::Carrier => "RIG TUNE",
        };
        let held = match self.amp_hold.over_view(amp, now) {
            Ok(v) => v,
            Err(why) => {
                stop_over(link, &mut self.stop_logged, &t, what, Some(why));
                return;
            }
        };
        let amp = &held;
        if outside {
            // SAFETY INTERLOCK (docs/safety-interlocks.md, #8 with #6): an
            // outside program's transmission is watched as TUNE is, at the
            // radio's full power setting (its audio can bring it that high),
            // and stopped when transmitting from outside is turned off.
            let watts = tune::planned_watts(r, Keying::Carrier, 1.0);
            let why =
                tune::tune_must_stop(amp, r, self.saved.swr_warn, watts, false).or_else(|| {
                    (!self.outside.allow_tx())
                        .then(|| "transmitting from outside programs was turned off".to_string())
                });
            match why {
                Some(why) => stop_over(link, &mut self.stop_logged, &t, what, Some(why)),
                None => link.tune_alive(),
            }
            return;
        }
        if t.keying == Keying::Data {
            // A TX window over: the TUNE interlocks at that mode's level,
            // and the window's own (its audio, WSPR's temperature, #12).
            let level = self.tx.over_mode().map_or(1.0, |m| self.saved.tx.level(m));
            let watts = tune::planned_watts(r, Keying::Data, level);
            let why = tune::tune_must_stop(amp, r, self.saved.swr_warn, watts, false)
                .map(Some)
                .or_else(|| self.tx.keep(decoders::slot::utc_now()));
            match why {
                Some(why) => stop_over(link, &mut self.stop_logged, &t, what, why),
                None => {
                    // SAFETY INTERLOCK (docs/safety-interlocks.md, #6): the
                    // over's audio follows LEVEL live, set only once the
                    // checks above pass at that same level (decision 87).
                    self.tx.set_level(level, r.audio_ceiling);
                    link.tune_alive();
                }
            }
            return;
        }
        if t.keying == Keying::Audio {
            self.tone.set_gain(self.tone_gain(r));
        }
        let watts = tune::planned_watts(r, t.keying, self.saved.drive);
        // SAFETY INTERLOCK (docs/safety-interlocks.md, #6): a TUNE in
        // progress is stopped the moment the amplifier trips, leaves the
        // radio's band in line, is driven past its limit (or the Tune slider
        // is raised past it), or the SWR stays high past the tuner's
        // sweep (`tune::TuneSwr`: the limit doubled, 5 s to settle, and
        // never the radio's while the amplifier's tuner is tuning).
        let keyed_for = now.saturating_duration_since(t.since);
        let why = self
            .tune_swr
            .must_stop(amp, r, self.saved.swr_warn, watts, self.trip_carried, keyed_for, now)
            .or_else(|| {
                (t.keying == Keying::Audio && !self.tone.playing())
                    .then(|| "the tone stopped playing".to_string())
            });
        match why {
            Some(why) => stop_over(link, &mut self.stop_logged, &t, what, Some(why)),
            None => link.tune_alive(),
        }
    }

    /// Log each change in what says the station is transmitting, and how long
    /// after the first the last let go: what holds the scope after an over.
    fn log_tx_sources(&mut self, rig: Option<&RigStatus>, amp: &AmpStatus) {
        let now = tx_sources(rig, amp);
        if now == self.tx_sources.0 {
            return;
        }
        let since = self.tx_sources.1.get_or_insert_with(Instant::now).elapsed();
        if now.is_empty() {
            tracing::info!(
                "transmit: receive again {:.1} s after the first sign",
                since.as_secs_f32()
            );
            self.tx_sources.1 = None;
        } else {
            tracing::info!("transmit: {now} (at {:.1} s)", since.as_secs_f32());
        }
        self.tx_sources.0 = now;
    }

    /// TRANSMIT, once a frame: the sequencer and the over on the air, the
    /// scope's bar, GLOBE's strobe, the log, and the window when open.
    fn run_tx(&mut self, ctx: &egui::Context, amp: &AmpStatus, rig: Option<&RigStatus>, xvtr: i64) {
        let now = decoders::slot::utc_now();
        // The band as worked: the true RF of where the panel listens.
        let heard = self.heard_hz(rig, xvtr);
        let band = heard.map_or("", |f| logbook::adif_band(f as f64));
        let feed = std::mem::take(&mut self.decode.tx_feed);
        self.tx.take_wspr(std::mem::take(&mut self.decode.wspr_feed), self.saved.station.pos());
        let rx_dial = self.rx.active().then(|| (self.rx.rx_hz, self.rx.mode.is_lower()));
        let bar_off = match (self.scope.bar_hz(), rx_dial) {
            (Some(b), Some((d, lower))) => Some(if lower { d - b } else { b - d }),
            _ => None,
        };
        let bar_moved = self.scope.take_bar_moved();
        // SETTINGS → Logbook's "log automatically", or ask first.
        self.saved.tx.prompt_log = !self.saved.log.auto_log;
        let device = self.tone_device();
        let decoding_ft8 = self.decode.running_kind() == Some(decode::Kind::Ft8);
        let scope_mode = self.scope_tx_mode();
        let station = self.saved.station.clone();
        let env = tx::Env {
            now,
            link: self.rig.as_deref(),
            rig,
            amp,
            device,
            outside: &mut self.outside,
            rx_dial,
            bar_off,
            bar_moved,
            bar_ours: self.saved.show_tx,
            station: &station,
            decoding_ft8,
            slot_decoded: self.decode.last_slot_utc(),
            scope_mode,
        };
        let log = &self.logbook;
        let badges = |call: &str, grid: Option<&str>| {
            let w = log.worked(call, band, "FT8", grid);
            tx::Badges {
                dupe: w.dupe,
                new_call: w.new_call,
                new_band: w.new_band,
                new_dxcc: w.new_dxcc,
                new_grid: w.new_grid,
            }
        };
        // TX freq AUTO's ear on the receiver's audio, only while it is in
        // use (decision 115).
        let auto = self.saved.show_tx
            && self.saved.tx.mode == tx::TxMode::Ft8
            && self.saved.tx.freq == tx::FreqMode::Auto
            && rx_dial.is_some();
        self.tx.occ.tap(&self.rx.taps, auto);
        let mut out = self.tx.frame(&mut self.saved.tx, env, feed, &self.decode.heard, &badges);
        // The bar and the busy shading are TX's only while its window is
        // open: elsewhere the bar is the operator's measuring cursor.
        let tx_shown = self.saved.show_tx;
        if !tx_shown {
            out.busy.clear();
        }
        if let (Some(off), Some((d, lower)), true) = (out.set_bar_off, rx_dial, tx_shown) {
            self.scope.set_bar_hz(if lower { d - off } else { d + off });
        }
        let busy = match rx_dial {
            Some((d, lower)) => out
                .busy
                .iter()
                .map(|&(a, b)| {
                    let (a, b) = (a as f64, b as f64);
                    if lower { (d - b, d - a) } else { (d + a, d + b) }
                })
                .collect(),
            None => Vec::new(),
        };
        self.scope.set_tx_busy(busy);
        // The station being worked, in green where we copy it (decision
        // 102), only while the QSO is in progress (decision 104): from the
        // receive dial and their audio offset.
        let dx = self.tx.dx_mark().filter(|_| tx_shown).and_then(|(call, off, _)| {
            let (d, lower) = rx_dial?;
            let off = off as f64;
            Some((if lower { d - off } else { d + off }, call))
        });
        self.scope.set_dx_mark(dx);
        self.globe.set_qso(out.strobe.map(|(c, g, keyed)| {
            (c, g, if keyed { globe::StrobeDir::Tx } else { globe::StrobeDir::Rx })
        }));
        // A finished contact: into the log at once (the demarcation point
        // is the sequencer's, `tx::qso`), and to WSJT-X's listeners.
        while let Some((l, _)) = self.tx.take_logged() {
            let rpt = |r: Option<i16>| r.map(tx::msg::fmt_report).unwrap_or_default();
            let rf = heard.map_or(0.0, |f| f as f64) + self.saved.tx.audio_hz as f64;
            let rec = logbook::QsoRecord {
                call: l.call.clone(),
                grid: l.grid.clone().unwrap_or_default(),
                rst_sent: rpt(l.rst_sent),
                rst_rcvd: rpt(l.rst_rcvd),
                freq_hz: rf,
                mode: "FT8".into(),
                band: logbook::adif_band(rf).into(),
                start_utc: l.start_utc,
                end_utc: l.end_utc,
                my_call: station.call().unwrap_or_default(),
                my_grid: station.grid().unwrap_or_default(),
                tx_pwr_w: rig.and_then(|r| {
                    tune::planned_watts(r, Keying::Data, self.saved.tx.ft8_level)
                }),
                ..Default::default()
            };
            let adif = logbook::adif::record(&rec);
            let id = self.logbook.add(rec);
            tracing::info!("log: {} logged from TX (#{id})", l.call);
            let entity = dxcc::lookup(&l.call).map(|e| e.name);
            self.tx.qso.note_worked(&l.call, entity.as_deref());
            if let Some(u) = self.outside.wsjtx.as_ref() {
                u.logged_adif(&adif);
            }
        }
        if !self.saved.show_tx {
            return;
        }
        let mode = self.saved.tx.mode;
        let watts = rig.and_then(|r| tune::planned_watts(r, Keying::Data, self.saved.tx.level(mode)));
        let blocked = rig.filter(|r| r.connected).map_or(Some("The radio isn't connected.".into()), |r| {
            tune::tone_blocked(r, self.tone_device().as_deref())
                .or_else(|| tune::key_blocked(amp, r, watts))
                .or_else(|| (mode == tx::TxMode::Wspr).then(|| tx::too_hot(amp)).flatten())
                // SAFETY INTERLOCK (docs/safety-interlocks.md, #13): as
                // `Tx::try_start` checks it (decision 116).
                .or_else(|| self.scope_tx_mode().and_then(|m| rx::data_modes_clash(m, r.mode)))
        });
        let band = rig.and_then(|r| r.freq_hz).and_then(band_of).map(|b| b.label());
        let mut open = true;
        let o = tx::window(
            ctx,
            &mut open,
            &mut self.tx,
            &mut self.saved.tx,
            tx::window::Ctx {
                now,
                link: self.rig.as_deref(),
                station: &station,
                decoding: self.decode.running_kind().map(|k| k.label()),
                blocked,
                watts,
                band,
                rig,
                heard_me: self.globe.heard_me(),
                heard_hz: heard.map(|f| f as f64),
                xvtr_hz: xvtr as f64,
            },
        );
        if let Some(p) = o.pop {
            self.saved.tx.popped = p;
        }
        if let Some(id) = o.pick_row {
            self.decode.pick_row(id);
        }
        if let Some(m) = o.decode_mode {
            self.saved.decode.kind = match m {
                tx::TxMode::Ft8 => decode::Kind::Ft8,
                tx::TxMode::Wspr => decode::Kind::Wspr,
            };
        }
        self.saved.show_tx = open;
    }

    /// Act on the TUNE strip.
    fn do_tune(&mut self, t: tune::TuneOut, amp: &AmpStatus, r: &RigStatus) {
        if self.rig.is_none() {
            return;
        }
        if t.stop
            && let Some(link) = &self.rig
        {
            tracing::info!("transmit: TUNE stopped: pressed again");
            link.tune_stop(None);
        }
        let secs = self.saved.tune_secs.clamp(*tune::TUNE_SECS.start(), *tune::TUNE_SECS.end());
        let max = Duration::from_secs(secs.into());
        if t.start.is_some() {
            // SAFETY INTERLOCK (docs/safety-interlocks.md, #8): one keying at
            // a time. TUNE isn't started while an outside program holds the
            // key, and none may take it from now until TUNE shows keyed, so
            // a running TUNE is never mistaken for a program's transmission
            // (which isn't watched for TUNE's tone).
            if self.outside.holder().is_some() {
                return;
            }
            self.outside.panel_tune_starting();
        }
        // SAFETY INTERLOCKS (docs/safety-interlocks.md, #6 and #7): checked
        // again here, not only by the greyed buttons.
        if t.start.is_some() {
            self.trip_carried = amp.alerts.tripped;
        }
        match t.start {
            Some(Keying::Carrier) => {
                let watts = tune::planned_watts(r, Keying::Carrier, self.saved.drive);
                if tune::tune_blocked(amp, r, watts).is_none()
                    && let Some(link) = &self.rig
                {
                    link.tune_start(max, Keying::Carrier);
                }
            }
            Some(Keying::Audio) => {
                let watts = tune::planned_watts(r, Keying::Audio, self.saved.drive);
                let dev = self.tone_device();
                if tune::tune_blocked(amp, r, watts).is_none()
                    && tune::tone_blocked(r, dev.as_deref()).is_none()
                    && let Some(dev) = dev
                {
                    // The tone first, then the key: keyed with no tone, the
                    // radio would send nothing, but never anything else.
                    match self.tone.start(&dev, self.tone_gain(r)) {
                        Ok(()) => {
                            self.tone_error = None;
                            self.tone_asked = Some(Instant::now());
                            if let Some(link) = &self.rig {
                                link.tune_start(max, Keying::Audio);
                            }
                        }
                        Err(e) => self.tone_error = Some(e),
                    }
                }
            }
            // The TUNE strip never keys a data message: that is the TX
            // window's, through `tx_key`.
            Some(Keying::Data) | None => {}
        }
        if let Some(link) = &self.rig {
            if let Some(code) = t.power {
                link.set_power(code);
            }
            if t.usb_audio {
                link.use_usb_audio();
            }
        }
    }

    /// The radio is transmitting, the amplifier is in line and has had time
    /// to key, and its own TX light says it hasn't: the radio's PTT line is
    /// not reaching it.
    fn ptt_missing(&mut self, amp: &AmpStatus, rig: Option<&RigStatus>) -> Option<Keying> {
        let r = rig.filter(|r| r.connected)?;
        let keyed = r.keyed() || r.tuning.is_some();
        if !keyed {
            self.rig_keyed_since = None;
            return None;
        }
        let since = *self.rig_keyed_since.get_or_insert_with(Instant::now);
        let in_line = amp.connected && amp.readings.is_some_and(|x| x.ptt);
        (since.elapsed() >= KEY_SETTLE && in_line && amp.transmitting == Some(false))
            .then(|| r.tuning.map_or(Keying::Audio, |t| t.keying))
    }

    /// See whether a band press here sticks. If the amplifier is back on its
    /// old band once it has had time to switch, something else sets its
    /// band: on AJ3G's station, the FDM-DUO over the cable between them, with
    /// the HR500's Transceiver menu on ELAD FDM DUO. The firmware has no
    /// command to change that menu, so the panel says where to (decision 13).
    fn watch_band(&mut self, st: &AmpStatus, actions: &[AmpAction]) {
        let r = st.readings;
        let pressed = actions
            .iter()
            .any(|a| matches!(a, AmpAction::BandUp | AmpAction::BandDown | AmpAction::SetBand(_)));
        // A press refused for RF (interlocks 1 and 2) is not the cable's doing.
        if pressed && st.connected && r.is_some_and(|r| !rf_present(&r)) {
            self.band_press = Some((std::time::Instant::now(), r.and_then(|r| r.band)));
        }
        if let Some((at, before)) = self.band_press
            && at.elapsed() >= BAND_SETTLE
        {
            self.band_press = None;
            let now = r.and_then(|r| r.band);
            if before.is_some() && now.is_some() {
                self.band_held = now == before;
                if !self.band_held {
                    self.band_dismissed = false;
                }
            }
        }
    }

    /// Keep the amplifier's band on the radio's, when asked to. The link holds
    /// each change until the amplifier shows no RF (interlock 3); this side
    /// also waits while the radio says it is transmitting.
    /// Once each connection, the radio's TX IN (menu 32) on its USB audio
    /// when it reads anything else, as SDRoxide does when its port opens
    /// (decision 100). Once only, so a change made on the radio afterwards
    /// stands until the next connection.
    fn usb_audio_at_connect(&mut self, rig: Option<&RigStatus>) {
        let Some(r) = rig.filter(|r| r.connected) else {
            self.usb_audio_set = false;
            return;
        };
        if self.usb_audio_set || !self.saved.rig.usb_audio_at_connect {
            return;
        }
        let Some(input) = r.tx_input.as_deref() else { return };
        self.usb_audio_set = true;
        if input != "USB audio"
            && let Some(link) = &self.rig
        {
            tracing::info!("rigcat: TX IN (menu 32) read {input}; set to USB audio on connecting");
            link.use_usb_audio();
        }
    }

    fn follow_band(&mut self, amp: &AmpStatus, rig: Option<&RigStatus>, xvtr_hz: i64) {
        let reconnected = amp.connected && !self.amp_was_connected;
        self.amp_was_connected = amp.connected;
        let (Some(link), Some(rig)) = (&self.link, rig) else { return };
        if !self.saved.rig.follow_band || !amp.connected || !rig.connected {
            return;
        }
        // SAFETY INTERLOCK (docs/safety-interlocks.md, #3): with a transverter
        // the radio's dial is an IF, not the band on the air, so the
        // amplifier's band is left as the operator set it.
        if xvtr_hz != 0 {
            return;
        }
        let Some(hz) = rig.freq_hz else { return };
        let Some(band) = band_of(hz) else { return };
        // SAFETY INTERLOCK (docs/safety-interlocks.md, #3): not mid-over.
        // The link checks the amplifier again before sending.
        if rig.keyed() || amp.readings.is_some_and(|r| rf_present(&r)) {
            return;
        }
        if reconnected || self.band_told != Some(band) {
            link.set_frequency(hz);
            self.band_told = Some(band);
        }
    }

    /// Keep the radio window's transverter offset and the scope's the same:
    /// whichever changed last wins.
    fn sync_xvtr(&mut self, ctx: &egui::Context, radio_hz: i64) {
        let scope_hz = self.scope.settings().transverter_hz.round() as i64;
        match self.xvtr_seen {
            Some(seen) if radio_hz != seen => {
                self.scope.set_transverter_hz(radio_hz as f64);
                self.saved.scope = self.scope.settings().clone();
                self.xvtr_seen = Some(radio_hz);
            }
            Some(seen) if scope_hz != seen => {
                radio::set_xvtr_hz(ctx, scope_hz);
                self.xvtr_seen = Some(scope_hz);
            }
            None => {
                self.scope.set_transverter_hz(radio_hz as f64);
                self.xvtr_seen = Some(radio_hz);
            }
            _ => {}
        }
    }

    /// The receiver, the outside programs and the decoders, once a frame.
    fn run_receiver(
        &mut self,
        ctx: &egui::Context,
        amp: &AmpStatus,
        rig: Option<&RigStatus>,
        transmitting: bool,
        xvtr: i64,
    ) {
        let now = rx::RadioNow { link: self.rig.as_deref(), rig, transmitting };
        if let Some(at) = self.rx_restart {
            // Opening the ELAD tunes the DUO to the centre it is given: wait
            // a while for the CAT link to read VFO A, so it opens where the
            // radio already is.
            let cat_due = self.saved.rx.source == rx::IqChoice::Elad
                && self.saved.rig.enabled
                && !rig.is_some_and(|r| r.connected && r.freq_hz.is_some())
                && at.elapsed() < CAT_WAIT;
            if !cat_due {
                self.rx_restart = None;
                self.rx.start(&self.saved.rx, &self.scope, now);
            }
        }
        self.rx.frame(&mut self.saved.rx, &self.scope, now);
        // The receiver's mode and the radio's kept as one (decision 116):
        // following the radio, the receiver takes the running decoder's
        // passband when it is that decoder's mode, as a DECODE pick sets it.
        let dial =
            scope::view::FreqMap { ppm: self.scope.settings().ppm, transverter_hz: xvtr as f64 }
                .shown(self.rx.rx_hz)
                .max(0.0)
                .round() as u64;
        let data = self.decode.running_kind().and_then(|k| {
            k.digi().map(|d| (d.sdr_mode(dial), d.passband(dial))).or_else(|| {
                k.audio_passband(self.saved.decode.audio_hz).map(|pb| (sdr::Mode::Usb, pb))
            })
        });
        self.rx.keep_modes(now, data);
        if std::mem::take(&mut self.rx.restart) {
            self.rx.start(&self.saved.rx, &self.scope, now);
        }
        let reqs = self.outside.frame(outside::Frame {
            ctx,
            amp,
            rig,
            link: self.rig.as_ref(),
            rx: &self.rx,
            transmitting,
        });
        for r in reqs {
            self.outside_req(r, rig, transmitting);
        }
        let at = decode::Where {
            dial_hz: self.rx.rx_hz,
            lsb: self.rx.mode.is_lower(),
            xvtr_hz: xvtr as f64,
            ppm: self.scope.settings().ppm,
        };
        // The decoder runs while its window, or TX's, is open.
        self.saved.decode.running = self.show_decode || self.saved.show_tx;
        self.decode.set_view(self.scope.view_hz());
        // Slots heard while the radio was keyed are our own over coming
        // back through the I/Q: DECODE drops them (decision 71).
        let keyed = rig.filter(|r| r.connected).map_or(transmitting, |r| r.transmitting());
        self.decode.note_keyed(decoders::slot::utc_now(), keyed);
        self.tx.occ.set_keyed(keyed);
        self.decode.frame(
            &self.saved.decode,
            &self.saved.station,
            &self.rx,
            at,
            self.outside.wsjtx.as_ref(),
        );
        // Hashed calls naming us or the station in hand resolve (decision 72).
        let mut calls: Vec<String> = self.saved.station.call().into_iter().collect();
        calls.extend(self.tx.qso.dx_call().map(str::to_string));
        self.decode.seed_calls(&calls);
        // The scope's slot lines follow the decoder's slots (decision 51).
        self.scope.decode_slots(self.decode.slots());
        // And its labels, DECODE's decodes (decision 54).
        let mut feed = self.decode.skim_feed(&self.saved.decode);
        let fakes = self.rx.demo_fakes();
        if !fakes.is_empty() {
            let f = feed
                .get_or_insert_with(|| scope::SkimFeed { period_s: 15.0, ..Default::default() });
            let now = decoders::slot::utc_now();
            f.spots.extend(fakes.into_iter().map(|(hz, label)| scope::SkimSpot {
                row: None,
                hz,
                audio_hz: 0.0,
                dial_hz: hz,
                call: label.to_string(),
                text: "Not a real station: one of the demo's made-up fillers".into(),
                snr: 0.0,
                utc: now,
                wsjt: None,
                key: None,
            }));
        }
        self.scope.skim(feed);
    }

    /// Make a change an outside program asked for, as the panel's own
    /// controls would: the receiver and VFO A, the mode, the centre.
    fn outside_req(&mut self, r: outside::Req, rig: Option<&RigStatus>, transmitting: bool) {
        use outside::Req;
        let now = rx::RadioNow { link: self.rig.as_deref(), rig, transmitting };
        let proto = self.saved.rig.model.protocol();
        let cat = |f: Option<rigcat::Frame>| {
            if let (Some(l), Some(f)) = (&self.rig, f) {
                // Read back; refused while transmitting (#5).
                l.set(vec![f]);
            }
        };
        let active = self.rx.active();
        match r {
            Req::FreqA(hz) if active => {
                let rate = self.rx.rate();
                if rate <= 0.0 || (hz - self.rx.centre()).abs() > 0.45 * rate {
                    self.rx.recentre(hz, now);
                }
                self.rx.tune(hz, transmitting);
            }
            Req::FreqA(hz) => cat(proto.set_freq(rigcat::Vfo::A, hz.round() as u64)),
            Req::FreqB(hz) => cat(proto.set_freq(rigcat::Vfo::B, hz.round() as u64)),
            Req::Mode(m, w) if active => self.rx.set_mode(m, w.map(|w| passband_for(m, w)), now),
            Req::Mode(m, _) => cat(proto.set_mode(rx::rig_mode(m))),
            Req::Split(on) => cat(proto.set_split(on)),
            Req::Centre(hz) => self.rx.recentre(hz, now),
        }
    }

    fn scope_window(&mut self, ctx: &egui::Context, rig: Option<&RigStatus>, transmitting: bool) {
        let mut open = true;
        // What the TX window has on the air, by name (decision 83).
        let over = self.tx.over_mode().map(|m| match m {
            tx::TxMode::Ft8 => "FT8 over",
            tx::TxMode::Wspr => "WSPR beacon",
        });
        let tx_why = transmitting.then(|| tx_why(rig, &self.status(), over));
        // Every sample the panel sent since the last frame, for the scope's
        // TX monitor (decision 83): the TX window's over, else TX TUNE's tone.
        let sending = self.tx.sounding(scope::TX_TAP).or_else(|| self.tone.recent(scope::TX_TAP));
        self.scope.set_tx_signal(rig.and_then(|r| scope::TxSignal::from_rig(r, sending, over)));
        let srig = scope::ScopeRig {
            rx_hz: self.rx.rx_hz,
            mode: self.rx.mode,
            passband: self.rx.passband,
            transmitting,
            // The display holds on the radio's own word when it has one;
            // the amplifier's drive reading lingers after the radio unkeys.
            on_air: match rig.filter(|r| r.connected) {
                Some(r) => r.transmitting(),
                None => transmitting,
            },
        };
        let args = scopewin::Args {
            rx: &self.rx,
            settings: &mut self.saved.rx,
            rig: srig,
            cat: rig.is_some_and(|r| r.connected),
            links: self.outside.summary(),
            tx_why,
            sounds: &self.sounds,
        };
        let o =
            scopewin::scope_window(ctx, &mut open, self.saved.scope_popped, &mut self.scope, args);
        if let Some(p) = o.pop {
            self.saved.scope_popped = p;
        }
        if o.reopen {
            self.rx.stop();
            self.rx_restart = Some(Instant::now());
        }
        if o.settings {
            self.show_settings = true;
        }
        if let Some(id) = o.swap_slice {
            let now = rx::RadioNow { link: self.rig.as_deref(), rig, transmitting };
            self.rx.swap_slice(&mut self.saved.rx, id, now);
        }
        if let Some(mut so) = o.scope {
            if so.settings_changed {
                self.saved.scope = self.scope.settings().clone();
            }
            let mut picked = so.data_mode.take();
            if let Some(id) = so.skim_pick.take() {
                self.decode.pick_row(id);
            }
            // A keyboard skimmer's label (decision 56): DECODE reads its mode.
            // The scope sent it only on receive.
            match so.key_pick.take() {
                Some(scope::skim::KeyMode::Cw) => self.saved.decode.kind = decode::Kind::Cw,
                Some(scope::skim::KeyMode::Psk) => self.saved.decode.kind = decode::Kind::Psk31,
                Some(scope::skim::KeyMode::Rtty) => {
                    self.saved.decode.kind = decode::Kind::Rtty;
                    self.saved.decode.reverse = false;
                }
                _ => {}
            }
            // A SKIM WIDE label: its mode on the dial it was heard on, as that
            // mode's DATA key would (sideband, passband, the radio's filter,
            // DECODE and its note); AJ3G found the note still saying FT8
            // after an FT4 label.
            let wide = match so.wide_pick.take() {
                Some(decoders::WsjtMode::Ft8) => scope::digimodes::digi("FT8"),
                Some(decoders::WsjtMode::Ft4) => scope::digimodes::digi("FT4"),
                _ => None,
            };
            if let (Some(digi), Some(dial)) = (wide, so.tune_to) {
                let xvtr = radio::xvtr_hz(ctx) as f64;
                so.tune_to = None;
                picked =
                    Some(scope::DataPick { digi, dial: (dial + xvtr).round() as u64, note: None });
            }
            let now = rx::RadioNow { link: self.rig.as_deref(), rig, transmitting };
            self.rx.apply_scope(so, &mut self.saved.rx, now);
            // A DATA key or a band-plan marker: the radio's filter, the
            // decoder and the note follow, as DECODE's snap does.
            if let Some(p) = picked {
                self.data_pick(p, rig, transmitting, radio::xvtr_hz(ctx));
            }
        }
        self.show_scope = open;
    }

    /// A data mode on a dial, from the scope's DATA keys and markers or
    /// GLOBE's TUNE: the radio, the receiver and DECODE go to it.
    fn data_pick(
        &mut self,
        p: scope::DataPick,
        rig: Option<&RigStatus>,
        transmitting: bool,
        xvtr: i64,
    ) {
        let kind = decode::Kind::of_digi(p.digi);
        let (note, refused) =
            self.snap_to(Some(p.digi), kind, Some(p.dial), rig, transmitting, xvtr);
        let note = match p.note {
            Some(n) if !refused => format!("{n}. {note}"),
            _ => note,
        };
        self.scope.show_data(Some(p.digi), Some(note.clone()));
        self.decode.note = Some((note, refused));
    }

    fn globe_window(
        &mut self,
        ctx: &egui::Context,
        rig: Option<&RigStatus>,
        transmitting: bool,
        xvtr: i64,
    ) {
        let mut open = true;
        let rx_hz = self.heard_hz(rig, xvtr).map(|f| f as f64);
        let o = globe::window(
            ctx,
            &mut open,
            &mut self.globe,
            &mut self.saved.globe,
            &self.saved.station,
            rx_hz,
        );
        if let Some(p) = o.pop {
            self.saved.globe.popped = p;
        }
        if let Some(p) = o.tune {
            self.data_pick(p, rig, transmitting, xvtr);
        }
        if let Some(id) = o.show_row {
            self.show_decode = true;
            self.decode.pick_row(id);
        }
        self.show_globe = open;
    }

    /// The panel's receiver's mode, while it listens to the radio's own I/Q
    /// and so is where an over is worked out from (decision 116).
    fn scope_tx_mode(&self) -> Option<sdr::Mode> {
        (self.rx.active() && self.rx.source() == rx::IqChoice::Elad).then_some(self.rx.mode)
    }

    /// Where the panel is listening, as shown: its receiver when that is
    /// running, or else the connected radio's dial.
    fn heard_hz(&self, rig: Option<&RigStatus>, xvtr: i64) -> Option<u64> {
        use radio::logic;
        if self.rx.active() {
            let map = scope::view::FreqMap {
                ppm: self.scope.settings().ppm,
                transverter_hz: xvtr as f64,
            };
            Some(map.shown(self.rx.rx_hz).max(0.0).round() as u64)
        } else {
            rig.filter(|r| r.connected).and_then(logic::dial_hz).map(|f| logic::shown(f, xvtr))
        }
    }

    /// Take the radio and the panel's receiver to data mode `d` (decision
    /// 39): its frequency on the band they are on (or the nearest), or `at`
    /// (as on the LCD) when a marker was clicked; its sideband; the
    /// receiver's passband and, through the rig link, the radio's filter; and
    /// the decoder `kind` with its audio set for it. Says what it did, and
    /// whether it was refused.
    fn snap_to(
        &mut self,
        d: Option<&'static scope::Digi>,
        kind: Option<decode::Kind>,
        at: Option<u64>,
        rig: Option<&RigStatus>,
        transmitting: bool,
        xvtr: i64,
    ) -> (String, bool) {
        use radio::logic;
        use scope::digimodes::{Pick, mhz};
        let name = d.map(|d| d.name).or(kind.map(|k| k.label())).unwrap_or("That mode");
        // The decoder follows the mode picked even when the radio can't be
        // retuned now: the scope's DATA row and DECODE always agree.
        if let Some(k) = kind {
            self.saved.decode.kind = k;
        }
        // SAFETY INTERLOCK (docs/safety-interlocks.md, #5 and #9): nothing
        // is retuned while the radio transmits; the link refuses it then
        // anyway (#5), and the receiver is where the radio transmits in
        // split (#9).
        if transmitting {
            return (
                format!(
                    "{name}: not while transmitting. The radio refuses changes then \
                     (interlocks #5 and #9)."
                ),
                true,
            );
        }
        let now = rx::RadioNow { link: self.rig.as_deref(), rig, transmitting };
        let cat = rig.filter(|r| r.connected);
        let Some(d) = d else {
            // Olivia, THOR, HFDL: no calling frequency, so only the passband.
            let pb = kind.and_then(|k| k.audio_passband(self.saved.decode.audio_hz));
            if let Some(pb) = pb.filter(|_| self.rx.active()) {
                self.rx.set_mode(sdr::Mode::Usb, Some(pb), now);
                return (
                    format!(
                        "{name}: no calling frequency, so tune to it by hand. Receiver USB {:.0} \
                         to {:.0} Hz.",
                        pb.0, pb.1
                    ),
                    false,
                );
            }
            return (format!("{name}: no calling frequency, so tune to it by hand."), false);
        };
        let map =
            scope::view::FreqMap { ppm: self.scope.settings().ppm, transverter_hz: xvtr as f64 };
        let Some(here) = self.heard_hz(cat, xvtr) else {
            return (
                format!(
                    "{name}: no radio or receiver to tune. Choose the radio or the I/Q source \
                     under SETTINGS."
                ),
                true,
            );
        };
        let pick = match at {
            Some(dial) => Some(Pick { dial, note: None }),
            None => d.pick(here, &logic::pick_bands(xvtr), |f| logic::if_of(f, xvtr).is_some()),
        };
        let Some(pick) = pick else {
            let how = if xvtr == 0 { ": a converter offset brings others in" } else { "" };
            return (format!("No {name} frequency inside the radio's 9 kHz to 54 MHz{how}."), true);
        };
        let rig_mode = d.rig_mode(pick.dial);
        let pb = d.passband(pick.dial);
        let mut did = vec![format!("{name}: {rig_mode} {}", mhz(pick.dial))];
        let proto = self.saved.rig.model.protocol();
        let filter = |w: Option<u32>| match w {
            Some(w) if w >= 1000 => format!("radio filter {:.1} kHz", w as f64 / 1e3),
            Some(w) => format!("radio filter {w} Hz"),
            None => "radio filter left as it is".to_string(),
        };
        if self.rx.active() {
            let hz = map.radio(pick.dial as f64);
            self.rx.set_mode(d.sdr_mode(pick.dial), Some(pb), now);
            // The I/Q centre, and with the ELAD the DUO's VFO A, onto the
            // dial, but only when the dial is not already well inside the
            // span: SDR Console's fixed centre (decision 22) keeps the
            // waterfall's rows lined up. Then the receiver. Both refuse while
            // transmitting (interlocks #9 and #10).
            let span = scope::view::Span { center: self.rx.centre(), width: self.rx.rate() };
            if !scope::view::keeps_centre(span, hz) {
                self.rx.recentre(hz, now);
            }
            self.rx.tune(hz, transmitting);
            did.push(format!("receiver {:.0} to {:.0} Hz", pb.0, pb.1));
            let duo = self.rx.source() == rx::IqChoice::Elad && cat.is_some();
            if duo && self.rx.rate() <= rx::DUO_AUDIO_MAX_RATE as f64 {
                let w = rx::duo_filter(&proto.filters(rig_mode), d.sdr_mode(pick.dial), pb);
                did.push(filter(w.or(Some(d.filter_width()))));
            } else if duo {
                did.push("the DUO's filter is left: above 192 kHz its audio is muted".into());
            }
        } else if let (Some(link), Some(r)) = (&self.rig, cat) {
            // Read back after each; refused while transmitting (#5).
            match logic::digi_frames_at(&*proto, r, d, pick.dial, xvtr, false) {
                Ok(v) => link.set(v),
                Err(e) => return (format!("{name}: {e}"), true),
            }
            did.push(filter(logic::digi_filter(&proto.filters(rig_mode), d.lo, d.filter_width())));
        }
        if let Some(a) = d.audio_hz {
            self.saved.decode.audio_hz = a;
            self.saved.decode.reverse = false;
            did.push(format!("decoder at {a:.0} Hz"));
        }
        let mut note = did.join(" · ");
        if let Some(n) = pick.note {
            note = format!("{n}. {note}");
        }
        (note, false)
    }

    fn settings_window(&mut self, ctx: &egui::Context, st: &AmpStatus, rig: Option<&RigStatus>) {
        let mut open = self.show_settings;
        let mut apply = false;
        egui::Window::new("Settings")
            .open(&mut open)
            .frame(chrome::window_frame())
            // Its edges and corner can be dragged (AJ3G, 21 Oct 2026); its
            // size is kept from run to run with the other windows'.
            .resizable(true)
            .collapsible(false)
            // Below the top bar, so it doesn't cover the SETTINGS chip.
            .default_pos([24.0, 48.0])
            .default_size([484.0, (ctx.content_rect().height() - 72.0).max(320.0)])
            .min_size([420.0, 240.0])
            .show(ctx, |ui| {
                chrome::window_body_bg(ui);
                // The sections are taller than the window: they scroll, in
                // all the room there is, with APPLY kept in view below them.
                let foot = chrome::chip_height(ui, None) + 8.0 + 2.0 * ui.spacing().item_spacing.y;
                let room = (ui.available_height() - foot).max(80.0);
                egui::ScrollArea::vertical().max_height(room).auto_shrink([false, false]).show(
                    ui,
                    |ui| {
                        ui.horizontal(|ui| {
                            if chrome::chip(ui, false, "REFRESH DEVICES")
                                .on_hover_text(
                                    "Look again for serial ports, sound outputs and the ELAD's I/Q \
                                 on USB, after plugging something in. The sound lists also \
                                 look again each time they open.",
                                )
                                .clicked()
                            {
                                self.ports = serial_ports();
                                self.sounds = audio::output_devices();
                                self.elad_devices = elad_iq::list();
                            }
                        });
                        ui.add_space(6.0);
                        settings::settings_station(ui, &mut self.saved.station);
                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(6.0);
                        logbook::settings_ui(ui, &mut self.saved.log);
                        ui.add_space(6.0);
                        let mut on = radio::ref_10mhz(ctx);
                        if chrome::checkbox(ui, &mut on, "10 MHz reference connected to the DUO")
                            .on_hover_text(
                                "Lights REF on the RADIO display. The DUO doesn't report its \
                             reference input, so this is your word for it.",
                            )
                            .changed()
                        {
                            radio::set_ref_10mhz(ctx, on);
                        }
                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(6.0);
                        settings::settings_amp_tab(
                            ui,
                            &mut self.draft,
                            &mut self.saved.swr_warn,
                            st,
                            &self.ports,
                        );
                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(6.0);
                        settings::settings_radio_tab(
                            ui,
                            &mut self.draft_rig,
                            &self.draft.serial.path,
                            rig,
                            &self.ports,
                            &mut self.sounds,
                            &mut self.saved.audio_out,
                        );
                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(6.0);
                        if settings::settings_receiver_tab(
                            ui,
                            &mut self.draft_source,
                            &mut self.saved.rx.audio_out,
                            &mut self.sounds,
                            &self.elad_devices,
                            &self.rx,
                        ) {
                            self.elad_devices = elad_iq::list();
                        }
                        settings::settings_dsp(ui, &mut self.saved.rx.dsp);
                        self.rx.set_dsp(self.saved.rx.dsp);
                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(6.0);
                        outside::settings_section(ui, &mut self.draft_outside, &self.outside);
                    },
                );
                ui.add_space(8.0);
                apply = settings::apply_button(ui);
            });
        if apply {
            self.apply();
        }
        self.show_settings = open;
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // The scope's GPU threads may submit while the app's own code runs,
        // not while eframe paints or reconfigures a window (gpu_gate).
        gpu_gate::open();
        let _shut_after = gpu_gate::Closing;
        let ctx = &ui.ctx().clone();
        // The whole panel wears the chosen radio's look (decision 39). Only
        // here, before anything is painted.
        let look = theme::StationLook::for_radio(&self.saved.rig, self.forced_look);
        theme::set_station_look(ctx, look);
        let mut st = self.status();
        let rig = self.rig.as_ref().map(|l| l.status());
        let xvtr = radio::xvtr_hz(ctx);
        self.follow_band(&st, rig.as_ref(), xvtr);
        self.usb_audio_at_connect(rig.as_ref());
        // The amplifier as it reports itself, before the radio's readings
        // fill it in: what TUNE's interlocks (#6) judge by.
        let amp_raw = st.clone();
        self.sync_xvtr(ctx, xvtr);
        let transmitting =
            rig.as_ref().is_some_and(|r| r.connected && r.transmitting()) || amp_keyed(&amp_raw);
        if let Some(l) = &self.link {
            // The link gives unanswered reads longer while the radio is
            // keyed (#6).
            l.set_transmitting(rig.as_ref().is_some_and(|r| r.connected && r.transmitting()));
        }
        self.log_tx_sources(rig.as_ref(), &amp_raw);
        self.run_receiver(ctx, &amp_raw, rig.as_ref(), transmitting, xvtr);
        self.keep_tune(&amp_raw, rig.as_ref());
        let ptt_missing = self.ptt_missing(&amp_raw, rig.as_ref());
        // In bypass the amplifier reports no FWD, RFL or SWR: the radio's own
        // readings fill them, or with no radio FWD falls back to the
        // amplifier's DRV (see `panel::from_rig`).
        if let Some(r) = st.readings {
            let (r, fill) = fill_from_radio(r, rig.as_ref());
            st.readings = Some(r);
            self.view.set_rig(fill);
        }
        self.view.set_swr_warn(self.saved.swr_warn);
        self.view.set_radio(&self.saved.rig, look);

        egui::Panel::top("bar")
            .frame(
                egui::Frame::new()
                    .fill(theme::BG_DEEP())
                    .inner_margin(egui::Margin::symmetric(11, 6)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    if chrome::chip(ui, self.show_settings, "SETTINGS").clicked() {
                        self.show_settings = !self.show_settings;
                    }
                    if chrome::chip(ui, self.show_amp, "AMP")
                        .on_hover_text(
                            "The amplifier's meters, readings, tuner and keys. Hidden, the \
                             warnings on this bar still show",
                        )
                        .clicked()
                    {
                        self.show_amp = !self.show_amp;
                    }
                    if chrome::chip(ui, self.show_radio, "RADIO")
                        .on_hover_text(
                            "The radio's own readings and menu settings, over its CAT port",
                        )
                        .clicked()
                    {
                        self.show_radio = !self.show_radio;
                    }
                    if chrome::chip(ui, self.show_scope, "SCOPE")
                        .on_hover_text(
                            "The panadapter and the panel's own receiver, from the radio's I/Q",
                        )
                        .clicked()
                    {
                        self.show_scope = !self.show_scope;
                    }
                    if chrome::chip(ui, self.show_decode, "DECODE")
                        .on_hover_text("The panel's decoders: WSJT, keyboard, pictures, data links")
                        .clicked()
                    {
                        self.show_decode = !self.show_decode;
                    }
                    if chrome::chip(ui, self.saved.show_tx, "TX")
                        .on_hover_text("Transmit FT8 and WSPR: the QSO sequencer and its decode list")
                        .clicked()
                    {
                        self.saved.show_tx = !self.saved.show_tx;
                    }
                    if chrome::chip(ui, self.show_log, "LOG")
                        .on_hover_text("The logbook: QSOs, import and export, eQSL and LoTW")
                        .clicked()
                    {
                        self.show_log = !self.show_log;
                    }
                    if chrome::chip(ui, self.show_globe, "GLOBE")
                        .on_hover_text(
                            "Your station, the stations heard, the grey line, propagation and \
                             space weather",
                        )
                        .clicked()
                    {
                        self.show_globe = !self.show_globe;
                    }
                    ui.add_space(8.0);
                    let (text, col) = if let Some(h) = self.outside.holder() {
                        (
                            format!(
                                "Keyed by an outside program ({}). STOP on the TUNE strip unkeys \
                                 it.",
                                match h {
                                    links::PttSource::Tci { .. } => "TCI",
                                    links::PttSource::Rigctld { .. } => "rigctld",
                                }
                            ),
                            theme::CYAN(),
                        )
                    } else if let Some(k) = ptt_missing {
                        let more = if k == Keying::Carrier {
                            " RIG TUNE may not key the radio's PTT output: try TUNE."
                        } else {
                            ""
                        };
                        (
                            format!(
                                "The radio is transmitting and the HR500 isn't keyed: its PTT \
                                 line isn't reaching the amplifier.{more}"
                            ),
                            theme::YELLOW(),
                        )
                    } else if self.band_held && !self.band_dismissed {
                        (
                            "Band held by the radio's cable. To switch it here, set the HR500's \
                             Transceiver menu to None."
                                .to_string(),
                            theme::YELLOW(),
                        )
                    } else {
                        radio_notice(rig.as_ref(), self.saved.rig.follow_band)
                    };
                    let notice = ui.label(RichText::new(text).size(11.0).color(col));
                    if ptt_missing.is_some() {
                        notice.on_hover_text(
                            "The amplifier is in PTT and its TX light is off while the radio \
                             transmits. The HR500 keys only from its PTT input (the RCA jack, \
                             or pin 4 of ACC); there is no serial command to key it. Check \
                             that the cable from the FDM-DUO carries the radio's PTT output to \
                             that pin, and that the radio's PTT output is turned on in its \
                             menu. SDRoxide keyed the radio with an ordinary transmission \
                             (TX), which the panel's TUNE also uses.",
                        );
                    } else if self.band_held && !self.band_dismissed {
                        notice.on_hover_text(
                            "DN or UP was pressed and the amplifier went straight back to its \
                             band. With its Transceiver menu set to a radio (ELAD FDM DUO here), \
                             the HR500 takes its band from the radio's cable and puts back any \
                             other. Its firmware has no command to change that menu, so it is \
                             set on the amplifier: MENU, Transceiver, None. With the FDM-DUO \
                             connected under SETTINGS, Amplifier band can be set to come \
                             from this program instead, as a backup to the cable.",
                        );
                        if chrome::chip(ui, false, "DISMISS")
                            .on_hover_text(
                                "Hide this note. It comes back only after a band press here \
                                 works and a later one is undone again.",
                            )
                            .clicked()
                        {
                            self.band_dismissed = true;
                        }
                    }
                });
            });

        if let Some(r) = rig.as_ref().filter(|r| r.connected) {
            let mut args = self.tune_args(&amp_raw, r);
            let t = egui::Panel::top("tune")
                .frame(
                    egui::Frame::new()
                        // The panel's own fill, on which a slider's empty
                        // track shows.
                        .fill(theme::PANEL())
                        .inner_margin(egui::Margin::symmetric(11, 4)),
                )
                .show(ui, |ui| tune::tune_strip(ui, r, &mut args))
                .inner;
            self.do_tune(t, &amp_raw, r);
        }

        let popped = self.saved.amp_popped;
        let mut out = if !self.show_amp {
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(theme::PANEL()).inner_margin(11))
                .show(ui, |ui| {
                    chrome::window_body_bg(ui);
                    ui.label(
                        RichText::new("The amplifier's panel is hidden: AMP on the bar shows it.")
                            .size(12.0)
                            .color(theme::CYAN()),
                    );
                });
            panel::PanelOut::default()
        } else if popped {
            // Painted in place: no background GPU submit meanwhile (gpu_gate).
            let _shut = gpu_gate::shut();
            let (st, view) = (&st, &mut self.view);
            let mut out = panel::PanelOut::default();
            let closed = ctx.show_viewport_immediate(
                egui::ViewportId::from_hash_of("amp-window"),
                popout::builder(
                    ctx,
                    "amp-window",
                    st.model.label(),
                    [760.0, 720.0],
                    [520.0, 420.0],
                ),
                |ui, _class| {
                    popout::track(ui.ctx(), "amp-window");
                    out = egui::CentralPanel::default()
                        .frame(egui::Frame::new().fill(theme::PANEL()).inner_margin(11))
                        .show(ui, |ui| {
                            chrome::window_body_bg(ui);
                            panel::fit_panel(ui, |ui, scale| {
                                panel::amp_panel(ui, st, view, Some(true), Some(scale))
                            })
                        })
                        .inner;
                    ui.ctx().input(|i| i.viewport().close_requested())
                },
            );
            // Closing its window hides it, as the other windows do; AMP on
            // the bar brings it back where it was.
            if closed {
                self.show_amp = false;
            }
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(theme::PANEL()).inner_margin(11))
                .show(ui, |ui| {
                    chrome::window_body_bg(ui);
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("The amplifier is in a window of its own.")
                                .size(12.0)
                                .color(theme::CYAN()),
                        );
                        if chrome::chip(ui, false, "DOCK")
                            .on_hover_text("Put the AMP window back inside the main window")
                            .clicked()
                        {
                            out.pop = true;
                        }
                    });
                });
            out
        } else {
            egui::CentralPanel::default()
                .frame(egui::Frame::new().fill(theme::PANEL()).inner_margin(11))
                .show(ui, |ui| {
                    chrome::window_body_bg(ui);
                    panel::fit_panel(ui, |ui, scale| {
                        panel::amp_panel(ui, &st, &mut self.view, Some(false), Some(scale))
                    })
                })
                .inner
        };
        if std::mem::take(&mut out.pop) {
            self.saved.amp_popped = !popped;
        }
        if let Some(link) = &self.link {
            for &a in &out.actions {
                link.action(a);
            }
            // Drawn again straight away, to show the click as asked.
            if !out.actions.is_empty() {
                ctx.request_repaint();
            }
        }
        self.watch_band(&st, &out.actions);
        if out.settings {
            self.show_settings = true;
        }
        if self.show_settings {
            self.settings_window(ctx, &st, rig.as_ref());
        }
        if self.show_radio {
            let mut open = true;
            let popped = self.saved.radio_popped;
            let none = RigStatus::default();
            let args = self.tune_args(&amp_raw, rig.as_ref().unwrap_or(&none));
            let o = radio::radio_window(ctx, &mut open, popped, rig.as_ref(), args);
            if o.refresh
                && let Some(r) = &self.rig
            {
                r.read_settings();
            }
            if let Some(p) = o.pop {
                self.saved.radio_popped = p;
            }
            if let Some(on) = o.tx_follows_rx {
                self.saved.rx.tx_follows_rx = on;
            }
            if let Some(r) = &rig {
                self.do_tune(o.tune, &amp_raw, r);
            }
            // E1's VOL and NR are the DUO's (VM, NR, NC), heard only on the
            // DUO's own audio. AJ3G (31 Oct 2026): "the outcome is on ELAD
            // Stand Alone only". So they set the panel's Sound and NR too,
            // and turn what is heard whichever audio that is.
            if !transmitting {
                follow_e1(&o.frames, &mut self.saved.rx);
            }
            if let Some(v) = o.panel_volume {
                self.saved.rx.volume = v;
            }
            if let Some(link) = &self.rig {
                // Read back after each; refused while transmitting (#5).
                link.set(o.frames);
                // SAFETY INTERLOCK (docs/safety-interlocks.md, #6): a stored
                // CW message transmits through the amplifier at the full
                // power setting, so only when RIG TUNE could be keyed.
                if let (Some(f), Some(r)) = (o.cw_send, &rig)
                    && tune::key_blocked(&amp_raw, r, tune::planned_watts(r, Keying::Carrier, 1.0))
                        .is_none()
                {
                    link.set(vec![f]);
                }
            }
            self.show_radio = open;
        }
        if self.show_scope {
            self.scope_window(ctx, rig.as_ref(), transmitting);
        }
        if self.show_decode {
            let mut open = true;
            let o = decode::window(
                ctx,
                &mut open,
                self.saved.decode_popped,
                &mut self.decode,
                &mut self.saved.decode,
                &self.saved.station,
                &self.rx,
                self.outside.wsjtx.is_some(),
            );
            if let Some(p) = o.pop {
                self.saved.decode_popped = p;
            }
            if self.saved.decode.skim_log {
                skimlog::window(ctx, &mut self.saved.decode.skim_log, &mut self.decode.skim_log);
            }
            if let Some(k) = o.identified {
                self.scope.show_data(k.digi().filter(|d| d.key), None);
            }
            if let Some(k) = o.picked {
                // The scope's DATA row shows the mode picked here, whether
                // or not the radio is taken to it.
                let d = k.digi().filter(|d| d.key);
                if self.saved.decode.snap {
                    let (note, refused) =
                        self.snap_to(k.digi(), Some(k), None, rig.as_ref(), transmitting, xvtr);
                    self.scope.show_data(d, Some(note.clone()));
                    self.decode.note = Some((note, refused));
                } else {
                    self.scope.show_data(d, None);
                }
            }
            self.show_decode = open;
        }
        // DECODE's and the skimmer's stations, for GLOBE, kept while it is
        // shut too so it opens with the last hour on it.
        for r in self.decode.globe_feed.drain(..) {
            self.globe.add(r);
        }
        // GLOBE shows the mode in use: the TX window's while it is open,
        // else DECODE's running mode (AJ3G, 11 Nov 2026).
        self.globe.set_mode(if self.saved.show_tx {
            Some(match self.saved.tx.mode {
                tx::TxMode::Ft8 => "FT8",
                tx::TxMode::Wspr => "WSPR",
            })
        } else {
            self.decode.running_kind().map(|k| k.label())
        });
        // Beaconing WSPR: GLOBE asks who heard it every cycle (decision 93).
        self.globe.set_beacon(
            self.saved.show_tx && self.saved.tx.mode == tx::TxMode::Wspr && self.tx.enabled,
        );
        // A line picked in DECODE: that station and its path on GLOBE.
        if let Some((call, hz)) = self.decode.globe_pick.take() {
            self.globe.select_call(&call, hz);
        }
        let dial =
            self.heard_hz(rig.as_ref(), xvtr).map(|f| (f, rig.as_ref().and_then(|r| r.mode)));
        let o = logbook::show(
            ctx,
            &mut self.show_log,
            &mut self.logbook,
            &mut self.log_ui,
            &mut self.saved.log,
            &self.saved.station,
            dial,
            // Grids the log lacks, from stations decoded (decision 75).
            &self.decode.heard,
        );
        if let Some(p) = o.pop {
            self.saved.log.popped = p;
        }
        self.run_tx(ctx, &amp_raw, rig.as_ref(), xvtr);
        if self.show_globe {
            self.globe_window(ctx, rig.as_ref(), transmitting, xvtr);
        } else {
            // Shut, GLOBE still fetches who heard the station (decision 93).
            self.globe.idle(&mut self.saved.globe, &self.saved.station);
        }

        self.saved.meters = Some(self.view.saved());
        // The link updates on its own thread: look again a few times a second.
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, STORAGE_KEY, &self.saved);
    }

    /// Let the radio's USB I/Q go, stop the decoders and the servers (which
    /// unkeys any outside program), and free the scope's GPU textures.
    fn on_exit(&mut self) {
        // The DUO goes back to stand-alone 192 kHz as its stream closes, so
        // other programs can use it (AJ3G, 11 Nov 2026).
        elad_iq::set_leaving();
        self.decode.stop(&self.rx.taps);
        self.outside.apply(&outside::OutsideSettings::default(), &self.rx.taps);
        self.rx.stop();
        self.scope.retire(self.render_state.as_ref());
    }
}

/// `--port COM5` and `--baud 115200` on the command line choose the
/// amplifier's port and turn its link on, ahead of the saved settings, for a
/// shortcut that opens straight onto one amplifier. `--radio COM3` and
/// `--radio-baud 38400` do the same for the radio.
fn apply_args(cfg: &mut AmpConfig, rig: &mut RigConfig, mut args: impl Iterator<Item = String>) {
    while let Some(a) = args.next() {
        match a.as_str() {
            "--port" => {
                if let Some(p) = args.next() {
                    cfg.serial.path = p;
                    cfg.enabled = true;
                }
            }
            "--baud" => {
                if let Some(b) = args.next().and_then(|b| b.parse().ok()) {
                    cfg.serial.baud = b;
                }
            }
            "--radio" => {
                if let Some(p) = args.next() {
                    rig.path = p;
                    rig.enabled = true;
                }
            }
            "--radio-baud" => {
                if let Some(b) = args.next().and_then(|b| b.parse().ok()) {
                    rig.baud = b;
                }
            }
            _ => {}
        }
    }
}

/// How long a starting ELAD waits for the CAT link to read VFO A.
const CAT_WAIT: Duration = Duration::from_secs(10);

/// What the command line asks of the windows: `--demo` (the scope on the
/// demo band, in the ELAD look), `--elad` (the scope on the ELAD's I/Q),
/// `--scope`, `--decode`, `--theme elad|sdroxide`, and `--size 1200x900`.
#[derive(Debug, Default, PartialEq)]
struct ViewArgs {
    demo: bool,
    elad: bool,
    scope: bool,
    decode: bool,
    size: Option<[f32; 2]>,
    theme: Option<theme::StationLook>,
}

impl ViewArgs {
    fn parse(mut args: impl Iterator<Item = String>) -> ViewArgs {
        let mut v = ViewArgs::default();
        while let Some(a) = args.next() {
            match a.as_str() {
                "--demo" => v.demo = true,
                "--elad" => v.elad = true,
                "--scope" => v.scope = true,
                "--decode" => v.decode = true,
                "--theme" => v.theme = args.next().and_then(|t| theme::StationLook::parse(&t)),
                "--size" => {
                    v.size = args.next().and_then(|s| {
                        let (w, h) = s.split_once('x')?;
                        Some([w.parse().ok()?, h.parse().ok()?])
                    })
                }
                _ => {}
            }
        }
        v
    }

    /// The look asked for: `--theme`, or with `--demo` ELAD's, which is what
    /// the panel wears beside the FDM-DUO; otherwise the chosen radio's.
    fn look(&self) -> Option<theme::StationLook> {
        self.theme.or(self.demo.then_some(theme::StationLook::Elad))
    }
}

/// The receiver passband for a width an outside program asks for, as the
/// panel draws each mode's.
fn passband_for(m: sdr::Mode, width: u32) -> (f32, f32) {
    let w = (width as f32).clamp(50.0, 12_000.0);
    match m {
        sdr::Mode::Usb => (100.0, 100.0 + w),
        sdr::Mode::Lsb => (-100.0 - w, -100.0),
        sdr::Mode::Cw => (600.0 - w / 2.0, 600.0 + w / 2.0),
        sdr::Mode::Cwr => (-600.0 - w / 2.0, -600.0 + w / 2.0),
        sdr::Mode::Am | sdr::Mode::Fm => (-w / 2.0, w / 2.0),
    }
}

/// The amplifier says it is keyed, or shows RF (as interlocks 1 and 2).
/// Why the panel takes the radio to be transmitting, for the operator.
fn tx_why(rig: Option<&RigStatus>, amp: &AmpStatus, over: Option<&str>) -> String {
    // A TUNE of the panel's: what is on the air, by name (decision 83).
    if let Some(t) = rig.filter(|r| r.connected).and_then(|r| r.tuning) {
        return format!("{} from the panel", scope::txsig::on_air(t.keying, over));
    }
    if let Some(r) = rig.filter(|r| r.connected).and_then(RigStatus::tx_reason) {
        return r;
    }
    if amp.connected && amp.transmitting == Some(true) {
        return "the amplifier reports TX".into();
    }
    match amp.readings.as_ref().filter(|r| rf_present(r)) {
        Some(r) => format!("the amplifier reads {:.1} W drive, {:.1} W out", r.drive_w, r.fwd_w),
        None => "unknown".into(),
    }
}

fn amp_keyed(amp: &AmpStatus) -> bool {
    amp.connected
        && (amp.transmitting == Some(true) || amp.readings.as_ref().is_some_and(rf_present))
}

/// Everything that says the station is transmitting, in a few words: the
/// scope holds and the controls lock while any of them does.
/// Unkey an over the panel judged must stop, and log why, once an over
/// (AJ3G, 11 Nov 2026: a TUNE ended after 1.3 s with no reason in the log).
fn stop_over(
    link: &RigLink,
    logged: &mut Option<Instant>,
    t: &rigcat::Tuning,
    what: &str,
    why: Option<String>,
) {
    if *logged != Some(t.since) {
        *logged = Some(t.since);
        let after = t.since.elapsed().as_secs_f32();
        let why = why.as_deref().unwrap_or("its message ended");
        tracing::info!("transmit: {what} stopped after {after:.1} s: {why}");
    }
    link.tune_stop(why);
}

fn tx_sources(rig: Option<&RigStatus>, amp: &AmpStatus) -> String {
    let mut v = Vec::new();
    if let Some(r) = rig.filter(|r| r.connected) {
        if r.tuning.is_some() {
            v.push("panel TUNE".to_string());
        }
        if let Some(w) = r.fwd.filter(|p| p.valid && p.w > 0.0) {
            v.push(format!("radio FP {:.1} W", w.w));
        }
        if let Some(t) = r.tx_state.filter(|t| *t != 0) {
            v.push(format!("radio GI {t}"));
        }
        // The SWR the TUNE stop judges (decision 78), while there is RF.
        if !v.is_empty()
            && let Some(swr) = r.swr_ratio()
        {
            v.push(format!("radio SWR {swr:.1}"));
        }
    }
    if amp.connected {
        if amp.transmitting == Some(true) {
            v.push("amp TX light".into());
        }
        if !v.is_empty() && amp.atu.as_ref().is_some_and(|a| a.tuning) {
            v.push("amp ATU tuning".into());
        }
        if let Some(a) = amp.readings {
            if a.drive_w >= 0.1 {
                v.push(format!("amp DRV {:.1} W", a.drive_w));
            }
            if a.fwd_w >= 1.0 {
                v.push(format!("amp FWD {:.0} W", a.fwd_w));
                // The heat a WSPR beacon is judged by (interlock #12), in
                // whole °F so the line is written once a degree.
                if let Some(c) = a.known_temp_c() {
                    v.push(format!("amp {:.0} °F", c * 1.8 + 32.0));
                }
            }
        }
    }
    v.join(", ")
}

/// The amplifier's drive limit while it is in line (PTT), or `None` in OFF.
fn limit_in_line(amp: &AmpStatus) -> Option<f32> {
    amp.readings.filter(|r| r.ptt).map(|_| amp.model.limits().drive_warn_w)
}

/// The amplifier shows RF: keyed, drive or output (as interlocks 1 and 2).
fn rf_present(r: &AmpReadings) -> bool {
    r.drive_w >= 0.1 || r.fwd_w >= 1.0
}

/// The amateur band `hz` is in, for the amplifier's band switching.
fn band_of(hz: u64) -> Option<Band> {
    const BANDS: [(u64, u64, Band); 10] = [
        (1_800_000, 2_000_000, Band::M160),
        (3_500_000, 4_000_000, Band::M80),
        (5_250_000, 5_450_000, Band::M60),
        (7_000_000, 7_300_000, Band::M40),
        (10_100_000, 10_150_000, Band::M30),
        (14_000_000, 14_350_000, Band::M20),
        (18_068_000, 18_168_000, Band::M17),
        (21_000_000, 21_450_000, Band::M15),
        (24_890_000, 24_990_000, Band::M12),
        (28_000_000, 29_700_000, Band::M10),
    ];
    BANDS.iter().find(|(lo, hi, _)| (*lo..=*hi).contains(&hz)).map(|b| b.2)
}

/// The radio's readings into the amplifier's, for bypass (`panel::from_rig`).
///
/// With the radio connected its meters are passed only while it transmits,
/// as SDRoxide passed its own: when the radio trusts its forward power, or
/// the amplifier shows the radio's drive (below 0.5 W, and in a fold-back,
/// the DUO flags its reading). A radio that measures its own watts never
/// borrows DRV. With no radio, FWD falls back to DRV.
fn fill_from_radio(r: AmpReadings, rig: Option<&RigStatus>) -> (AmpReadings, panel::RigFill) {
    match rig.filter(|s| s.connected) {
        Some(s) => {
            let keyed = s.keyed() || r.drive_w >= 0.1;
            let tx = keyed.then(|| TxMeters {
                fwd_w: s.fwd_w(),
                swr: s.swr_ratio(),
                rfl_w: s.rfl_w(),
                ..TxMeters::default()
            });
            panel::from_rig(r, tx.as_ref(), true)
        }
        None => panel::from_rig(r, Some(&TxMeters::default()), false),
    }
}

/// The line along the top: where the band comes from, and the radio's state.
fn radio_notice(rig: Option<&RigStatus>, follow: bool) -> (String, egui::Color32) {
    match rig {
        Some(s) if s.connected => {
            let band = if follow {
                "The amplifier's band follows it between transmissions."
            } else {
                "Switch bands with DN and UP, or over the amplifier's own CAT or ACC input."
            };
            (format!("{} connected. {band}", s.model.label()), theme::CYAN())
        }
        Some(s) => (
            format!("Radio: {}", s.error.clone().unwrap_or_else(|| "connecting…".to_string())),
            theme::YELLOW(),
        ),
        None => (
            "No radio connected: switch bands with DN and UP, or let the radio set them over \
             the amplifier's CAT or ACC input."
                .to_string(),
            theme::CYAN(),
        ),
    }
}

/// The serial ports on this machine, by name.
fn serial_ports() -> Vec<String> {
    let mut ports: Vec<String> = serialport::available_ports()
        .map(|v| v.into_iter().map(|p| p.port_name).collect())
        .unwrap_or_default();
    ports.sort();
    ports
}

fn main() -> eframe::Result {
    // The log goes beside the saved settings (eframe's folder for the app).
    logfile::init(eframe::storage_dir(APP_NAME));
    let size = ViewArgs::parse(std::env::args().skip(1)).size;
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Hardrock Panel")
            .with_inner_size(size.unwrap_or([760.0, 820.0]))
            .with_min_inner_size([520.0, 420.0]),
        wgpu_options: scope::gpu::wgpu_options(),
        ..Default::default()
    };
    eframe::run_native(APP_NAME, options, Box::new(|cc| Ok(Box::new(App::new(cc)))))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_transmit_line_carries_the_radios_swr_and_the_tuner() {
        let mut r = RigStatus {
            connected: true,
            tx_state: Some(1),
            swr: rigcat::elad::parse_swr("0 03.20"),
            ..RigStatus::default()
        };
        let mut amp = AmpStatus { connected: true, ..AmpStatus::default() };
        amp.atu = Some(hardrock::AtuReadout { tuning: true, ..Default::default() });
        assert_eq!(tx_sources(Some(&r), &amp), "radio GI 1, radio SWR 3.2, amp ATU tuning");
        // Receiving, neither says it is transmitting.
        r.tx_state = None;
        assert_eq!(tx_sources(Some(&r), &amp), "");
    }

    #[test]
    fn a_first_run_warns_at_sdroxides_tune_limit() {
        let s = Saved::default();
        assert_eq!(s.swr_warn, 5.0);
        assert!(!s.amp.enabled);
        assert_eq!(s.amp.serial.baud, 115_200);
    }

    #[test]
    fn settings_from_an_older_file_fill_in_what_is_missing() {
        let s: Saved = serde_json::from_str(r#"{"swr_warn": 3.0}"#).unwrap();
        assert_eq!(s.swr_warn, 3.0);
        assert_eq!(s.amp, AmpConfig::default());
    }

    #[test]
    fn the_command_line_chooses_the_port_and_turns_the_link_on() {
        let (mut c, mut r) = (AmpConfig::default(), RigConfig::default());
        let args =
            ["--port", "COM5", "--baud", "38400", "--radio", "COM3", "--radio-baud", "57600"]
                .map(String::from);
        apply_args(&mut c, &mut r, args.into_iter());
        assert!(c.enabled);
        assert_eq!(c.serial.path, "COM5");
        assert_eq!(c.serial.baud, 38_400);
        assert!(r.enabled);
        assert_eq!(r.path, "COM3");
        assert_eq!(r.baud, 57_600);
    }

    #[test]
    fn with_no_radio_bypass_fwd_comes_from_drv() {
        // In OFF the amplifier reports no FWD; its DRV is the radio's power
        // passing straight through.
        let r = AmpReadings { ptt: false, drive_w: 6.0, ..AmpReadings::default() };
        let (out, _) = panel::from_rig(r, Some(&TxMeters::default()), false);
        assert_eq!(out.fwd_w, 6.0);
        // In line, the amplifier's own FWD is left alone.
        let keyed = AmpReadings { ptt: true, fwd_w: 400.0, drive_w: 6.0, ..AmpReadings::default() };
        assert_eq!(panel::from_rig(keyed, Some(&TxMeters::default()), false).0.fwd_w, 400.0);
    }

    fn duo(fwd: &str, rfl: &str, swr: &str) -> RigStatus {
        RigStatus {
            connected: true,
            fwd: rigcat::elad::parse_watts(fwd),
            rfl: rigcat::elad::parse_watts(rfl),
            swr: rigcat::elad::parse_swr(swr),
            ..RigStatus::default()
        }
    }

    #[test]
    fn with_the_duo_bypass_readings_are_the_radios_own() {
        // In OFF, 6.09 W through the bypassed amp: FWD, RFL and SWR from the DUO.
        let r = AmpReadings { ptt: false, drive_w: 6.5, ..AmpReadings::default() };
        let (out, fill) = fill_from_radio(r, Some(&duo(" 006091", " 000120", "0 01.32")));
        assert_eq!(out.fwd_w, 6.091);
        assert_eq!(out.rfl_w, 0.12);
        assert_eq!(out.swr, 1.32);
        assert_eq!(fill.fwd, Some("rig"));
    }

    #[test]
    fn a_flagged_duo_reading_is_never_replaced_by_drv() {
        // Keyed into an open the DUO folds back and flags its power; DRV
        // over-reads into the mismatch and must not stand in for it.
        let r = AmpReadings { ptt: false, drive_w: 9.8, ..AmpReadings::default() };
        let (out, fill) = fill_from_radio(r, Some(&duo("!002940", "!000000", "1!00.00")));
        assert_eq!(out.fwd_w, 0.0);
        assert_eq!(fill.fwd, Some("flagged"));
    }

    #[test]
    fn in_receive_the_duo_fills_nothing() {
        let r = AmpReadings { ptt: false, ..AmpReadings::default() };
        let (out, _) = fill_from_radio(r, Some(&duo("!000000", "!000000", "0!00.00")));
        assert_eq!(out, r);
    }

    #[test]
    fn in_line_the_amplifiers_own_readings_stand() {
        let r = AmpReadings { ptt: true, fwd_w: 400.0, drive_w: 6.0, ..AmpReadings::default() };
        let (out, _) = fill_from_radio(r, Some(&duo(" 006091", " 000001", "0 01.02")));
        assert_eq!(out.fwd_w, 400.0);
    }

    #[test]
    fn the_band_comes_from_the_dial() {
        assert_eq!(band_of(14_074_000), Some(Band::M20));
        assert_eq!(band_of(1_840_000), Some(Band::M160));
        assert_eq!(band_of(28_074_000), Some(Band::M10));
        assert_eq!(band_of(50_313_000), None);
        assert_eq!(band_of(15_000_000), None);
    }

    #[test]
    fn the_command_line_opens_the_scope_on_the_demo_band() {
        let args = ["--port", "COM5", "--demo", "--decode", "--size", "1200x900"].map(String::from);
        let v = ViewArgs::parse(args.into_iter());
        assert!(v.demo && v.decode && !v.scope);
        assert_eq!(v.size, Some([1200.0, 900.0]));
        // The demo wears the ELAD look unless told otherwise.
        assert_eq!(v.look(), Some(theme::StationLook::Elad));
        let args = ["--demo", "--theme", "sdroxide"].map(String::from);
        assert_eq!(ViewArgs::parse(args.into_iter()).look(), Some(theme::StationLook::Panel));
        assert_eq!(ViewArgs::parse(std::iter::empty()).look(), None);
    }

    #[test]
    fn older_settings_leave_the_scope_and_the_outside_links_off() {
        let s: Saved = serde_json::from_str(r#"{"swr_warn": 3.0}"#).unwrap();
        assert_eq!(s.rx.source, rx::IqChoice::Off);
        assert!(s.rx.tx_follows_rx);
        assert!(!s.outside.tci && !s.outside.rigctld && !s.outside.allow_tx);
        assert_eq!((s.outside.tci_port, s.outside.rigctld_port), (50001, 4532));
    }

    #[test]
    fn an_outside_programs_width_becomes_a_passband() {
        assert_eq!(passband_for(sdr::Mode::Usb, 3000), (100.0, 3100.0));
        assert_eq!(passband_for(sdr::Mode::Lsb, 2400), (-2500.0, -100.0));
        assert_eq!(passband_for(sdr::Mode::Cw, 500), (350.0, 850.0));
    }

    #[test]
    fn older_settings_get_a_radio_at_the_duos_own_rate() {
        let s: Saved = serde_json::from_str(r#"{"swr_warn": 3.0}"#).unwrap();
        assert!(!s.rig.enabled);
        assert_eq!(s.rig.baud, 38_400);
    }
}

/// The panel's NR following E1's NR (`NR` on or off, `NC` level 1 to 10,
/// taken to the panel's 1 to 4). E1's VOL turns the panel's Sound itself
/// when an outside speaker is picked (`TuneArgs::panel_vol`).
fn follow_e1(frames: &[rigcat::Frame], s: &mut rx::RxSettings) {
    for f in frames {
        let Ok(t) = std::str::from_utf8(&f.bytes) else { continue };
        let Some(body) = t.strip_suffix(';') else { continue };
        let (k, v) = body.split_at(body.len().min(2));
        let n: Option<u32> = v.trim().parse().ok();
        match (k, n) {
            ("NR", Some(0)) => s.nr = 0,
            ("NR", Some(_)) if s.nr == 0 => s.nr = 1,
            ("NC", Some(0)) => s.nr = 0,
            ("NC", Some(n)) => s.nr = nr_level(n),
            _ => {}
        }
    }
}

/// The DUO's NR level, 1 to 10, as the panel's 1 to 4.
fn nr_level(duo: u32) -> u8 {
    (duo.min(10) * 4).div_ceil(10).max(1) as u8
}

#[cfg(test)]
mod e1_tests {
    use super::*;

    #[test]
    fn e1s_nr_sets_the_panels_nr() {
        let mut s = rx::RxSettings::default();
        let fr = |t: &str| rigcat::Frame::new(t.to_string(), rigcat::FrameKind::SetSetting);
        follow_e1(&[fr("NR1;"), fr("NC003;")], &mut s);
        assert_eq!(s.nr, 2);
        follow_e1(&[fr("NC010;")], &mut s);
        assert_eq!(s.nr, 4);
        follow_e1(&[fr("NC001;")], &mut s);
        assert_eq!(s.nr, 1);
        follow_e1(&[fr("NR0;")], &mut s);
        assert_eq!(s.nr, 0);
        // Nothing else moves them.
        follow_e1(&[fr("FA00014074000;")], &mut s);
        assert_eq!(s.nr, 0);
    }
}
