//! SPOTS: telling the world what this station hears. FT8 and FT4 go to PSK
//! Reporter in batches ([`pskreporter`]), WSPR to WSPRnet after each slot
//! ([`wsprnet`]). The CW skimmer's stations go to the Reverse Beacon Network
//! through RBN Aggregator, which reads them from a telnet port here ([`rbn`]).
//!
//! Both are off until ticked, and can only be ticked once SETTINGS → Station
//! has a callsign and a grid. Nothing is sent, or kept to send, while a tick
//! is clear: clearing one stops its uploader and drops what it held.
//!
//! Everything here is apart from DECODE, which only hands it each slot
//! ([`Spots::ft_slot`], [`Spots::wspr_slot`]) and draws [`spots_box`], so the
//! box can move to another window.

pub(crate) mod pskreporter;
pub(crate) mod rbn;
pub(crate) mod wsprnet;

use crossbeam_channel::{Receiver, Sender};
use eframe::egui::{self, RichText};
use serde::{Deserialize, Serialize};

use crate::{chrome, theme};

/// The program as the services are told it.
pub(crate) fn program() -> String {
    format!("Hardrock Panel {}", env!("CARGO_PKG_VERSION"))
}

/// The ticks, kept between runs. Both off by default.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct SpotSettings {
    pub psk_reporter: bool,
    pub wsprnet: bool,
    /// The CW skimmer's stations on the telnet port for RBN Aggregator.
    pub rbn: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Service {
    PskReporter,
    Wsprnet,
}

/// What an uploader last did.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Status {
    pub service: Service,
    pub text: String,
    /// It worked (else `text` is the error).
    pub ok: bool,
    pub sent: usize,
}

/// Who is spotting: the operator's callsign and grid, both valid.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Me {
    pub call: String,
    pub grid: String,
}

/// The uploaders running, and what they last said.
pub(crate) struct Spots {
    psk: Option<(Me, pskreporter::Uploader)>,
    wspr: Option<(Me, wsprnet::Uploader)>,
    status_tx: Sender<Status>,
    status_rx: Receiver<Status>,
    /// Each service's last word, with the UTC it came.
    last: [Option<(Status, f64)>; 2],
    /// Reports handed to PSK Reporter since its last batch went.
    psk_queued: usize,
    /// Why an uploader could not start.
    error: Option<String>,
    /// The telnet port for RBN Aggregator, and who it spots as.
    rbn: Option<(Me, rbn::Server)>,
    rbn_pick: rbn::Picker,
    /// Spots sent to RBN this run, and the UTC of the last.
    rbn_sent: (usize, Option<f64>),
    rbn_error: Option<String>,
}

impl Default for Spots {
    fn default() -> Self {
        let (status_tx, status_rx) = crossbeam_channel::unbounded();
        Spots {
            psk: None,
            wspr: None,
            status_tx,
            status_rx,
            last: [None, None],
            psk_queued: 0,
            error: None,
            rbn: None,
            rbn_pick: rbn::Picker::default(),
            rbn_sent: (0, None),
            rbn_error: None,
        }
    }
}

impl Spots {
    /// Once a frame: start or stop the uploaders by the ticks and the
    /// station, and take what they said.
    pub(crate) fn frame(&mut self, s: &SpotSettings, me: Option<&Me>) {
        // Nothing runs unless its tick is set and the station is known; a
        // change of either stops the uploader (dropping its handle ends its
        // thread before it sends again, and what it held goes with it).
        let want_psk = me.filter(|_| s.psk_reporter);
        if self.psk.as_ref().map(|(m, _)| m) != want_psk {
            self.psk = want_psk.and_then(|m| {
                let l = pskreporter::Listener {
                    call: m.call.clone(),
                    grid: m.grid.clone(),
                    software: program(),
                };
                let tx = self.status_tx.clone();
                let host = pskreporter::HOST.to_string();
                match pskreporter::Uploader::spawn(l, host, pskreporter::PORT, tx) {
                    Ok(u) => Some((m.clone(), u)),
                    Err(e) => {
                        self.error = Some(format!("PSK Reporter: {e}"));
                        None
                    }
                }
            });
            self.psk_queued = 0;
            self.last[0] = None;
        }
        let want_wspr = me.filter(|_| s.wsprnet);
        if self.wspr.as_ref().map(|(m, _)| m) != want_wspr {
            self.wspr = want_wspr.and_then(|m| {
                let l = wsprnet::Listener {
                    call: m.call.clone(),
                    grid: m.grid.clone(),
                    version: program(),
                };
                match wsprnet::Uploader::spawn(l, self.status_tx.clone()) {
                    Ok(u) => Some((m.clone(), u)),
                    Err(e) => {
                        self.error = Some(format!("WSPRnet: {e}"));
                        None
                    }
                }
            });
            self.last[1] = None;
        }
        let want_rbn = me.filter(|_| s.rbn);
        if self.rbn.as_ref().map(|(m, _)| m) != want_rbn {
            self.rbn = None;
            self.rbn_error = None;
            self.rbn = want_rbn.and_then(|m| match rbn::Server::spawn(&m.call, rbn::PORT) {
                Ok(srv) => Some((m.clone(), srv)),
                Err(e) => {
                    self.rbn_error = Some(format!(
                        "RBN: port {} is taken ({e}); is CW Skimmer running?",
                        rbn::PORT
                    ));
                    None
                }
            });
            self.rbn_sent = (0, None);
        }
        let now = decoders::slot::utc_now();
        for st in self.status_rx.try_iter() {
            let i = match st.service {
                Service::PskReporter => {
                    if self.psk.is_none() {
                        continue;
                    }
                    if st.ok {
                        self.psk_queued = 0;
                    }
                    0
                }
                Service::Wsprnet => {
                    if self.wspr.is_none() {
                        continue;
                    }
                    1
                }
            };
            self.last[i] = Some((st, now));
        }
    }

    /// An FT8 or FT4 slot's stations, for PSK Reporter.
    pub(crate) fn ft_slot(&mut self, reports: Vec<pskreporter::Report>) {
        if let Some((_, u)) = &self.psk {
            self.psk_queued += reports.len();
            for r in reports {
                u.report(r);
            }
        }
    }

    /// A WSPR slot, for WSPRnet.
    pub(crate) fn wspr_slot(&mut self, slot: wsprnet::Slot) {
        if let Some((_, u)) = &self.wspr {
            u.slot(slot);
        }
    }

    /// A CW signal the skimmer reads: spotted to RBN when it should be
    /// ([`rbn::Picker`]) and RBN is ticked.
    pub(crate) fn cw_skim(
        &mut self,
        call: &str,
        text: &str,
        rf_hz: f64,
        snr: f32,
        wpm: u16,
        utc: f64,
    ) {
        let Some((me, srv)) = &self.rbn else { return };
        if let Some(spot) = self.rbn_pick.offer(&me.call, call, text, rf_hz, snr, wpm, utc) {
            srv.send(&rbn::line(&me.call, &spot));
            self.rbn_sent = (self.rbn_sent.0 + 1, Some(utc));
        }
    }

    pub(crate) fn psk_on(&self) -> bool {
        self.psk.is_some()
    }

    pub(crate) fn wspr_on(&self) -> bool {
        self.wspr.is_some()
    }
}

/// The SPOTS box: the two ticks, greyed until the station is set (`me`), and
/// what each last sent or why it failed.
pub(crate) fn spots_box(ui: &mut egui::Ui, s: &mut SpotSettings, spots: &Spots, me: Option<&Me>) {
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0, theme::LINE()))
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("SPOTS").size(10.5).color(theme::CYAN_DIM()).strong());
                let ready = me.is_some();
                let why = "Set My Callsign and My Grid under SETTINGS → Station first";
                ui.add_enabled_ui(ready, |ui| {
                    chrome::checkbox(ui, &mut s.psk_reporter, "PSK Reporter")
                        .on_hover_text(
                            "Send the FT8 and FT4 stations heard to pskreporter.info, a batch \
                             every five minutes or so, as WSJT-X does",
                        )
                        .on_disabled_hover_text(why);
                    chrome::checkbox(ui, &mut s.wsprnet, "WSPRnet")
                        .on_hover_text(
                            "Send each WSPR slot's spots to wsprnet.org as it is decoded",
                        )
                        .on_disabled_hover_text(why);
                    chrome::checkbox(ui, &mut s.rbn, "RBN")
                        .on_hover_text(format!(
                            "The CW skimmer's stations for the Reverse Beacon Network, as CW \
                             Skimmer gives them: a telnet port on this computer (port {}) that \
                             RBN Aggregator reads and passes on. Only stations calling CQ or \
                             TEST, their call read twice, once a band in ten minutes. Needs \
                             SKIM CW/PSK/RTTY on, and RBN Aggregator running",
                            rbn::PORT
                        ))
                        .on_disabled_hover_text(why);
                });
                if !ready {
                    ui.label(RichText::new(why).size(11.0).weak());
                }
            });
            let line = |ui: &mut egui::Ui,
                        name: &str,
                        on: bool,
                        last: &Option<(Status, f64)>,
                        wait: String| {
                if !on {
                    return;
                }
                let (text, col) = match last {
                    Some((st, utc)) if st.ok => (
                        format!("{name}: {} {} UTC", st.text, &crate::decode::hms(*utc)[..5]),
                        theme::CYAN(),
                    ),
                    Some((st, utc)) => (
                        format!("{name}: {} ({} UTC)", st.text, &crate::decode::hms(*utc)[..5]),
                        theme::YELLOW(),
                    ),
                    None => (format!("{name}: {wait}"), theme::TEXT()),
                };
                ui.label(RichText::new(text).size(11.5).color(col));
            };
            line(
                ui,
                "PSK Reporter",
                spots.psk_on(),
                &spots.last[0],
                format!("{} waiting for the next batch", spots.psk_queued),
            );
            if spots.psk_on() && spots.last[0].is_some() && spots.psk_queued > 0 {
                ui.label(
                    RichText::new(format!("{} more waiting for the next batch", spots.psk_queued))
                        .size(11.0)
                        .weak(),
                );
            }
            line(ui, "WSPRnet", spots.wspr_on(), &spots.last[1], "nothing sent yet".into());
            if let Some((_, srv)) = &spots.rbn {
                let n = srv.connected();
                let who = match n {
                    0 => "waiting for RBN Aggregator".to_string(),
                    1 => "Aggregator connected".to_string(),
                    n => format!("{n} connected"),
                };
                let sent = match spots.rbn_sent {
                    (0, _) | (_, None) => "no CW spots yet".to_string(),
                    (k, Some(t)) => {
                        format!("{k} CW spots, last {} UTC", &crate::decode::hms(t)[..5])
                    }
                };
                let col = if n == 0 { theme::TEXT() } else { theme::CYAN() };
                ui.label(
                    RichText::new(format!("RBN: port {}, {who}, {sent}", srv.port))
                        .size(11.5)
                        .color(col),
                );
            }
            if let Some(e) = &spots.rbn_error {
                ui.label(RichText::new(e).size(11.5).color(theme::YELLOW()));
            }
            if let Some(e) = &spots.error {
                ui.label(RichText::new(e).size(11.5).color(theme::YELLOW()));
            }
        });
}
