//! TUNE, to watch what the amplifier does, laid out as SDRoxide's TX box: the
//! FDM-DUO's own power buttons (1W, 3W, 5W and MAX: menu 34), and the Tune
//! slider, which sets the power within the button's setting.
//!
//! **TUNE** keys an ordinary transmission (`TX;`) of a 1 kHz tone from the
//! panel through the radio's USB audio, at the slider's level. It is what
//! SDRoxide's TUNE did, and it keys the radio's PTT output to the amplifier
//! as its PTT does. **RIG TUNE** keys the radio's own carrier (`TX2;`) at the
//! full menu 34 setting, with no tone and no slider.
//!
//! The checks here are safety interlock #6, with the link's own time limit,
//! watchdog and SWR stop behind them, and #7 for TUNE's tone.

use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use hardrock::{AmpAlerts, AmpReadings, AmpStatus, Band};
use rigcat::{Keying, RigStatus};

use crate::radio::z;
use crate::{chrome, theme};

/// The shortest and longest TUNE the panel offers, in seconds. The link
/// refuses more than a minute whatever it is asked.
pub(crate) const TUNE_SECS: std::ops::RangeInclusive<u32> = 1..=60;
pub(crate) const DEFAULT_TUNE_SECS: u32 = 10;
/// Where the Tune slider starts on a first run: a quarter of the setting.
pub(crate) const DEFAULT_DRIVE: f32 = 0.25;

/// TX TUNE's key: the audio path, as FT8 and WSPR go.
pub(crate) const TONE_LABEL: &str = "TX TUNE (USB audio)";

/// RIG TUNE's key, with the power it keys at (menu 34's, `None` until read).
pub(crate) fn carrier_label(watts: Option<f32>) -> String {
    match watts {
        Some(w) => format!("RIG TUNE (carrier {})", fmt_watts(w)),
        None => "RIG TUNE (carrier, menu 34)".to_string(),
    }
}

/// Watts as the keys say them: tenths under 10 W.
fn fmt_watts(w: f32) -> String {
    if w < 9.95 { format!("{w:.1} W") } else { format!("{w:.0} W") }
}

/// The power buttons, as in SDRoxide (`duo_power::PRESETS` there): label and
/// menu 34 setting.
pub(crate) const PRESETS: [(&str, u8); 4] = [("1W", 2), ("3W", 6), ("5W", 8), ("MAX", 9)];
const PRESETS_CAPTION: &str = "ELAD FDM-DUO TX PWR";
/// How long a power key shows as clicked while the radio's read-back is
/// awaited, s.
const POWER_WAIT_S: f64 = 3.0;

/// How old the amplifier's readings may be for TUNE or an over to start:
/// about how long the link takes to give up on unanswered reads at rest
/// (four status reads of 400 ms, another read between each, about 3.2 s),
/// so a start needs readings as live as it always did, even while the link
/// is giving a transmission more time.
pub(crate) const LIVE_MAX: Duration = Duration::from_millis(3500);
/// How old the last readings may be and still stand for an over already
/// keyed (see [`AmpHold`]).
pub(crate) const HOLD_MAX: Duration = Duration::from_secs(5);

/// The hover for the button that sets `code` (from SDRoxide's).
fn preset_hover(code: u8) -> String {
    let base = "Sets the FDM-DUO's own power setting (menu 34 TX POWER). The Tune slider then \
                sets TUNE's power within it. It stays set after the panel closes, as on the \
                radio's menu, and is the ceiling for every program that transmits.";
    match code {
        9 => format!(
            "MAX: the FDM-DUO's full output, about 8 W. The setting to use with the HR500 in \
             line, with the Tune slider to bring it down.\n\n{base}"
        ),
        8 => format!(
            "5 W. With the HR500 in line it will not reach full output on most bands at this \
             setting: use MAX, and the slider to bring it down.\n\n{base}"
        ),
        c => format!(
            "{}, for running barefoot or QRP.\n\n{base}",
            rigcat::elad::POWER_STEPS.get(c as usize).map_or("?", |p| p.label)
        ),
    }
}

/// The power TUNE would key at, as near as is known: the menu 34 setting for
/// the carrier; that times the Tune slider for the tone, which the slider
/// spans in a straight line (`rigcat::elad::AUDIO_CEILING`).
pub(crate) fn planned_watts(rig: &RigStatus, keying: Keying, drive: f32) -> Option<f32> {
    let step = rig.power()?;
    Some(match keying {
        Keying::Carrier => step.watts,
        Keying::Audio | Keying::Data => step.watts * drive.clamp(0.0, 1.0),
    })
}

/// Why TUNE may not be keyed now, or `None` if it may. `watts` is the power
/// it would key at ([`planned_watts`]).
///
/// SAFETY INTERLOCK (docs/safety-interlocks.md, #6): TUNE goes through the
/// amplifier, so it is keyed only when the amplifier's state is known and
/// safe for it: connected and, in line (PTT), on the radio's band (a carrier
/// into the wrong low-pass filter meets a mismatch at full gain) and within
/// its drive limit (as #4: 8 W on the HR500).
///
/// A trip the amplifier still shows does not refuse TUNE: the HR500 clears
/// it only the next time it is keyed (its manual: trip conditions "do not
/// need to be manually reset"), so refusing left no way to clear it (AJ3G,
/// 25 Oct 2026). To clear one, TUNE runs at no more than the drive limit
/// whether the amplifier is in line or not, and a new trip stops it
/// ([`tune_must_stop`]). Outside programs and stored CW messages are still
/// refused while it shows ([`key_blocked`]).
///
/// The readings must be live (no older than [`LIVE_MAX`]): only an over
/// already keyed may go on with held ones ([`AmpHold`]).
pub(crate) fn tune_blocked(amp: &AmpStatus, rig: &RigStatus, watts: Option<f32>) -> Option<String> {
    if let Some(age) = amp.readings_at.map(|t| t.elapsed())
        && amp.readings.is_some()
        && age > LIVE_MAX
    {
        return Some(format!(
            "The amplifier hasn't answered for {:.0} s, so its state can't be checked.",
            age.as_secs_f32()
        ));
    }
    amp_blocked(amp, rig, watts)
}

/// [`tune_blocked`] without its age check, for an over in progress, which
/// judges by [`AmpHold`]'s readings.
fn amp_blocked(amp: &AmpStatus, rig: &RigStatus, watts: Option<f32>) -> Option<String> {
    if !rig.connected {
        return Some("The radio isn't connected.".into());
    }
    let Some(r) = amp.readings.filter(|_| amp.connected) else {
        return Some("The amplifier isn't connected, so its state can't be checked.".into());
    };
    if amp.alerts.tripped {
        let limit = amp.model.limits().drive_warn_w;
        match watts {
            None => return Some("The radio's power setting hasn't been read yet.".into()),
            Some(w) if w > limit => {
                return Some(format!(
                    "The amplifier has tripped. TUNE clears it, at {limit:.0} W or less: lower \
                     the power or the Tune slider (about {w:.1} W now)."
                ));
            }
            Some(_) => {}
        }
    }
    if !r.ptt {
        return None;
    }
    let Some(watts) = watts else {
        return Some("The radio's power setting hasn't been read yet.".into());
    };
    let limit = amp.model.limits().drive_warn_w;
    if watts > limit {
        return Some(format!(
            "About {watts:.1} W is over the amplifier's {limit:.0} W drive limit while it is in \
             line (PTT). Lower the power, or put the amplifier in OFF."
        ));
    }
    let radio = rig.freq_hz.and_then(crate::band_of);
    let amp_band = r.band.and_then(Band::from_code);
    match (radio, amp_band) {
        (Some(a), Some(b)) if a == b => None,
        (None, _) => Some(
            "The radio isn't on a band the amplifier covers, so the amplifier's filter can't \
             be checked."
                .into(),
        ),
        (Some(a), b) => Some(format!(
            "The radio is on {a} and the amplifier on {}: the carrier would meet the wrong \
             low-pass filter. Change band first.",
            b.map_or("no band".to_string(), |b| b.to_string())
        )),
    }
}

/// Why an outside program (TCI, rigctld) may not key the radio now, or
/// `None` if it may.
///
/// SAFETY INTERLOCK (docs/safety-interlocks.md, #8 with #6): an outside
/// program's transmission goes through the amplifier as TUNE's does, so it
/// is judged by [`tune_blocked`] at the radio's full power setting (the
/// program's audio level can bring the power up to it, as for a stored CW
/// message), and refused while anything else keys the radio. For an ordinary
/// transmission the link also reads the radio's input and mode again just
/// before it keys (#7).
pub(crate) fn outside_ptt_blocked(amp: &AmpStatus, rig: &RigStatus) -> Option<String> {
    if amp.alerts.tripped {
        return Some("The amplifier has tripped. TUNE clears it.".into());
    }
    if rig.tuning.is_some() {
        return Some("TUNE is running.".into());
    }
    if rig.transmitting() {
        return Some("The radio is already transmitting.".into());
    }
    key_blocked(amp, rig, planned_watts(rig, Keying::Carrier, 1.0))
}

/// [`tune_blocked`] for anything but TUNE: a stored CW message or an outside
/// program. A trip the amplifier shows refuses it; TUNE is what clears one.
pub(crate) fn key_blocked(amp: &AmpStatus, rig: &RigStatus, watts: Option<f32>) -> Option<String> {
    if amp.alerts.tripped {
        return Some("The amplifier has tripped. TUNE clears it.".into());
    }
    tune_blocked(amp, rig, watts)
}

/// Why TUNE's tone may not be keyed, beyond [`tune_blocked`]: no sound
/// output for it, or the radio not set to send it.
///
/// SAFETY INTERLOCK (docs/safety-interlocks.md, #7): an ordinary transmission
/// sends whatever audio is on the radio's input, so it is keyed only with the
/// input on USB audio (not the microphone) and the radio on USB or LSB. The
/// link reads both again just before it keys.
pub(crate) fn tone_blocked(rig: &RigStatus, device: Option<&str>) -> Option<String> {
    if device.is_none() {
        return Some(
            "No sound output for the radio. Choose its USB audio under SETTINGS, Radio.".into(),
        );
    }
    match rig.tx_input.as_deref() {
        Some("USB audio") => {}
        Some(other) => {
            return Some(format!(
                "The radio's TX IN (menu 32) is on {other}, so TUNE would send that. USB AUDIO \
                 sets it to the radio's USB audio."
            ));
        }
        None => return Some("The radio's TX IN (menu 32) hasn't been read yet.".into()),
    }
    match rig.mode {
        Some("USB" | "LSB") => None,
        Some(m) => Some(format!("The radio is in {m}. TUNE's tone needs USB or LSB.")),
        None => Some("The radio's mode hasn't been read yet.".into()),
    }
}

/// Why a TUNE in progress must stop now, or `None`. Everything that would
/// have refused it, and also: drive past the limit in line, or SWR at or past
/// the warning on the amplifier (in line) or the radio.
///
/// `amp` is the amplifier as [`AmpHold::over_view`] gives it: during a
/// few seconds of unanswered reads, the last readings it gave.
///
/// `carried_trip` is a trip the amplifier already showed when this TUNE
/// started and hasn't cleared since: that one doesn't stop it, since keying
/// is what clears it. Any other trip does.
///
/// TX TUNE and RIG TUNE judge SWR by [`TuneSwr`] instead, and a TX window
/// over or an outside program's by [`OverSwr`]; this one-reading rule is
/// kept for the tests that compare against it.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn tune_must_stop(
    amp: &AmpStatus,
    rig: &RigStatus,
    swr_warn: f32,
    watts: Option<f32>,
    carried_trip: bool,
) -> Option<String> {
    let r = match stop_but_swr(amp, rig, watts, carried_trip) {
        Err(why) => return Some(why),
        Ok(r) => r?,
    };
    if r.ptt && r.swr >= swr_warn {
        return Some(format!("The amplifier's SWR reached {:.1}:1.", r.swr));
    }
    if let Some(s) = rig.swr_ratio().filter(|s| *s >= swr_warn) {
        return Some(format!("The radio's SWR reached {s:.1}:1."));
    }
    None
}

/// [`tune_must_stop`] without its SWR checks: `Err` with why the over must
/// stop, else the amplifier's readings (`None` without them, which
/// [`amp_blocked`] has already refused when they matter).
fn stop_but_swr(
    amp: &AmpStatus,
    rig: &RigStatus,
    watts: Option<f32>,
    carried_trip: bool,
) -> Result<Option<AmpReadings>, String> {
    if amp.alerts.tripped && !carried_trip {
        return Err("The amplifier tripped.".into());
    }
    if let Some(why) = amp_blocked(amp, rig, watts) {
        return Err(why);
    }
    let Some(r) = amp.readings else { return Ok(None) };
    let limit = amp.model.limits().drive_warn_w;
    if r.ptt && r.drive_w > limit {
        return Err(format!("Drive {:.1} W went over the {limit:.0} W limit.", r.drive_w));
    }
    Ok(Some(r))
}

/// How far a TUNE raises the SWR limit: twice the warning, as SDRoxide's
/// `swr_tune_limit`, but never past [`TUNE_SWR_MAX`].
const TUNE_SWR_SCALE: f32 = 2.0;
/// Just past where the HR500's tuner gives up, and so where a TUNE's limit
/// and an over's single-reading stop sit at most. WA2EUJ (14 Nov 2026,
/// docs/atu-values.md): under 2.5 dB return loss, about 6:1, the ATU-500
/// doesn't try to tune; it stays bypassed and reports an error. Past that a
/// high SWR is never a tune still working, only a fault. 6.1 rather than 6.0
/// so that at 6:1 the tuner bypasses by itself first (AJ3G). It was 10:1,
/// the top of the scale the radios report, so with the 5.0 default warning
/// the single-reading stop sat at 10:1.
const TUNE_SWR_MAX: f32 = 6.1;
/// How long after keying a TUNE's SWR is not believed at all: SDRoxide's
/// 150 meter samples (about 5 s), the time a tuner takes to sweep.
pub(crate) const TUNE_SWR_SETTLE: Duration = Duration::from_secs(5);
/// How long the SWR must stay at or over a TUNE's limit before it stops it:
/// SDRoxide's six readings in a row (about a fifth of a second), here as a
/// time, a little longer since the radio is read every 50 to 150 ms while
/// keyed, so one spike never covers it alone.
pub(crate) const TUNE_SWR_DWELL: Duration = Duration::from_millis(300);

/// The SWR limit while tuning: the warning doubled, capped at 6.1:1, just
/// past the tuner's 6:1, and never under the warning itself.
// SAFETY INTERLOCK (docs/safety-interlocks.md, #6)
pub(crate) fn swr_tune_limit(swr_warn: f32) -> f32 {
    (swr_warn * TUNE_SWR_SCALE).min(TUNE_SWR_MAX).max(swr_warn)
}

/// The SWR rule for TX TUNE and RIG TUNE, ported from SDRoxide's
/// (`swr_rails` and `SWR_TRIP_SAMPLES` in its engine).
///
/// SAFETY INTERLOCK (docs/safety-interlocks.md, #6): a tune is the
/// deliberate act of keying into a load that is not yet matched, and a tuner
/// sweeping its relays makes the SWR jump for a moment. Stopped on one
/// reading, the carrier dropped mid-cycle, the HR500's tuner then reported
/// "Lo power" and the TUNE light went red (AJ3G, 11 Nov 2026). So, during a
/// TUNE:
/// - The radio's SWR is judged against the warning doubled (6.1:1 at most,
///   just past where the HR500's tuner gives up, [`swr_tune_limit`]), not at all for the first [`TUNE_SWR_SETTLE`], and
///   stops it only after staying at or over that for [`TUNE_SWR_DWELL`].
///   While the amplifier says its tuner is tuning, the radio's SWR doesn't
///   stop it at all: the amplifier protects itself then.
/// - The amplifier's own SWR in line stops it at the warning on one reading,
///   as before, except while its tuner is tuning: then it is judged as the
///   radio's is above.
///
/// Everything else in [`tune_must_stop`] still stops it at once.
#[derive(Default)]
pub(crate) struct TuneSwr {
    /// When the SWR first read at or over the tune limit, in this run of
    /// such readings.
    over_since: Option<Instant>,
}

impl TuneSwr {
    /// Forget the last run of high readings: when no TUNE is keyed.
    pub(crate) fn reset(&mut self) {
        self.over_since = None;
    }

    /// Why a TUNE keyed for `keyed_for` must stop now, or `None`; once a
    /// frame while it is keyed. The arguments are [`tune_must_stop`]'s.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn must_stop(
        &mut self,
        amp: &AmpStatus,
        rig: &RigStatus,
        swr_warn: f32,
        watts: Option<f32>,
        carried_trip: bool,
        keyed_for: Duration,
        now: Instant,
    ) -> Option<String> {
        let r = match stop_but_swr(amp, rig, watts, carried_trip) {
            Err(why) => return Some(why),
            Ok(r) => r?,
        };
        let atu_tuning = amp.atu.as_ref().is_some_and(|a| a.tuning);
        let limit = swr_tune_limit(swr_warn);
        let settled = keyed_for >= TUNE_SWR_SETTLE;
        if r.ptt && !atu_tuning && r.swr >= swr_warn {
            return Some(format!("The amplifier's SWR reached {:.1}:1.", r.swr));
        }
        let mut over = None;
        if settled && r.ptt && atu_tuning && r.swr >= limit {
            over = Some(format!(
                "The amplifier's SWR stayed at {:.1}:1 while its tuner tuned (limit {limit:.1}:1 \
                 while tuning).",
                r.swr
            ));
        }
        if settled
            && !atu_tuning
            && over.is_none()
            && let Some(s) = rig.swr_ratio().filter(|s| *s >= limit)
        {
            over = Some(format!(
                "The radio's SWR stayed at {s:.1}:1 (limit {limit:.1}:1 while tuning)."
            ));
        }
        let Some(why) = over else {
            self.over_since = None;
            return None;
        };
        let since = *self.over_since.get_or_insert(now);
        (now.saturating_duration_since(since) >= TUNE_SWR_DWELL).then_some(why)
    }
}

/// How long after keying an ordinary over's SWR is not believed: the radio's
/// key-down and the audio's ramp, when the meters read SWR off almost no
/// power.
pub(crate) const OVER_SWR_SETTLE: Duration = Duration::from_secs(1);
/// How long the SWR must stay at or over the warning before it stops an
/// ordinary over: several of the radio's readings (50 to 150 ms apart keyed)
/// and more than one of the amplifier's.
pub(crate) const OVER_SWR_DWELL: Duration = Duration::from_millis(500);
/// Below this forward power from the amplifier in line, its SWR is not
/// believed: the ratio of two readings near zero.
pub(crate) const OVER_SWR_MIN_AMP_W: f32 = 5.0;
/// Below this forward power from the radio, its SWR is not believed.
pub(crate) const OVER_SWR_MIN_RIG_W: f32 = 0.5;

/// The SWR rule for an ordinary over: a TX window over, or an outside
/// program's.
///
/// SAFETY INTERLOCK (docs/safety-interlocks.md, #6): one reading at the
/// warning stopped the over, and FT8 overs were cut short for a high SWR the
/// antenna didn't have (AJ3G, 14 Nov 2026). A reading taken at key-down, as
/// the audio ramps, or off a fraction of a watt is the ratio of two numbers
/// near zero. So, for an ordinary over:
/// - SWR is not judged for the first [`OVER_SWR_SETTLE`] after keying, nor
///   while the amplifier says its tuner is tuning and for
///   [`OVER_SWR_SETTLE`] after (it protects itself then, and a tuner
///   switching makes the SWR jump; AJ3G: "its not getting enough time to let
///   the Tuner run"), nor
///   while the forward power it is read from is under [`OVER_SWR_MIN_AMP_W`]
///   (amplifier in line) or [`OVER_SWR_MIN_RIG_W`] (radio).
/// - After that, SWR at or over the warning stops the over once it has stayed
///   there for [`OVER_SWR_DWELL`].
/// - SWR at or over twice the warning, 6.1:1 at most ([`swr_tune_limit`]), on the amplifier
///   in line, with its power above the floor and past the settle, still stops
///   it on one reading.
///
/// Everything else in [`tune_must_stop`] still stops it at once.
#[derive(Default)]
pub(crate) struct OverSwr {
    over_since: Option<Instant>,
    /// When the amplifier's tuner last said it was tuning in this over.
    atu_at: Option<Instant>,
}

impl OverSwr {
    /// Forget the last run of high readings: when no ordinary over is keyed.
    pub(crate) fn reset(&mut self) {
        self.over_since = None;
        self.atu_at = None;
    }

    /// Why an ordinary over keyed for `keyed_for` must stop now, or `None`;
    /// once a frame while it is keyed. The other arguments are
    /// [`tune_must_stop`]'s.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn must_stop(
        &mut self,
        amp: &AmpStatus,
        rig: &RigStatus,
        swr_warn: f32,
        watts: Option<f32>,
        carried_trip: bool,
        keyed_for: Duration,
        now: Instant,
    ) -> Option<String> {
        let r = match stop_but_swr(amp, rig, watts, carried_trip) {
            Err(why) => return Some(why),
            Ok(r) => r?,
        };
        if amp.atu.as_ref().is_some_and(|a| a.tuning) {
            self.atu_at = Some(now);
        }
        let tuner_settling =
            self.atu_at.is_some_and(|t| now.saturating_duration_since(t) < OVER_SWR_SETTLE);
        if keyed_for < OVER_SWR_SETTLE || tuner_settling {
            self.over_since = None;
            return None;
        }
        let mut over = None;
        if r.ptt && r.fwd_w >= OVER_SWR_MIN_AMP_W && r.swr >= swr_warn {
            let at =
                format!("{:.1}:1 (forward {:.0} W, reflected {:.0} W)", r.swr, r.fwd_w, r.rfl_w);
            if r.swr >= swr_tune_limit(swr_warn) {
                return Some(format!("The amplifier's SWR reached {at}."));
            }
            over = Some(format!("The amplifier's SWR stayed at {at}."));
        }
        let rig_fwd = rig.fwd_w();
        if over.is_none()
            && rig_fwd.is_none_or(|w| w >= OVER_SWR_MIN_RIG_W)
            && let Some(s) = rig.swr_ratio().filter(|s| *s >= swr_warn)
        {
            over = Some(match (rig_fwd, rig.rfl_w()) {
                (Some(f), Some(rf)) => format!(
                    "The radio's SWR stayed at {s:.1}:1 (forward {f:.1} W, reflected {rf:.1} W)."
                ),
                _ => format!("The radio's SWR stayed at {s:.1}:1."),
            });
        }
        let Some(why) = over else {
            self.over_since = None;
            return None;
        };
        let since = *self.over_since.get_or_insert(now);
        (now.saturating_duration_since(since) >= OVER_SWR_DWELL).then_some(why)
    }
}

/// The amplifier's last good readings, for an over already keyed.
///
/// SAFETY INTERLOCK (docs/safety-interlocks.md, #6): the HR500 often leaves
/// its queries unanswered while it transmits, and the link may drop and
/// reopen. An over (TUNE, RIG TUNE, a TX window over or an outside
/// program's) in progress is then not stopped for the missing readings
/// alone: it is judged by the last readings the amplifier gave, for up to
/// [`HOLD_MAX`] from when it gave them. A trip, SWR at or past the warning,
/// over-drive or a band mismatch that the amplifier reports still stops it
/// at once, held readings being judged as live ones are. After
/// [`HOLD_MAX`] with no answer the over stops, as it did at once before
/// (AJ3G, 11 Nov 2026: a link hiccup was cutting his QSOs and CQs).
/// Starting one still needs live readings ([`tune_blocked`]).
#[derive(Default)]
pub(crate) struct AmpHold {
    last: Option<(AmpReadings, AmpAlerts, Instant)>,
}

impl AmpHold {
    /// Keep the amplifier's readings when it is connected and has them.
    /// Once a frame, keyed or not, so an over starts with them in hand.
    pub(crate) fn note(&mut self, amp: &AmpStatus, now: Instant) {
        if let Some(r) = amp.readings.filter(|_| amp.connected) {
            self.last = Some((r, amp.alerts, amp.readings_at.unwrap_or(now)));
        }
    }

    /// The amplifier as an over in progress judges it: as reported while
    /// it is connected with readings, else with the held readings while they
    /// are no older than [`HOLD_MAX`]. `Err` with why the over must stop
    /// once they are older.
    pub(crate) fn over_view(&self, amp: &AmpStatus, now: Instant) -> Result<AmpStatus, String> {
        let Some((r, alerts, at)) = self.last else {
            // Never had readings: judged as reported (and refused there).
            return Ok(amp.clone());
        };
        let age = now.saturating_duration_since(at);
        if age > HOLD_MAX {
            return Err(format!(
                "The amplifier hasn't answered for {:.0} s, so its state can't be checked.",
                age.as_secs_f32()
            ));
        }
        if amp.connected && amp.readings.is_some() {
            return Ok(amp.clone());
        }
        Ok(AmpStatus {
            connected: true,
            readings: Some(r),
            readings_at: Some(at),
            alerts,
            ..amp.clone()
        })
    }
}

/// What the strip needs that the radio's status doesn't carry.
pub(crate) struct TuneArgs<'a> {
    /// Why TUNE (the tone) may not be keyed.
    pub tone_blocked: Option<String>,
    /// Why RIG TUNE (the carrier) may not be keyed.
    pub carrier_blocked: Option<String>,
    pub limit_in_line: Option<f32>,
    pub secs: &'a mut u32,
    /// The Tune slider, 0 to 1 of the menu 34 setting.
    pub drive: &'a mut f32,
    /// The last tone that wouldn't play, and why.
    pub tone_error: Option<String>,
    /// The panel's Sound, 0 to 1, when it plays on a speaker other than the
    /// DUO's (the DUO's own is muted then): RADIO's E1 VOL turns this.
    pub panel_vol: Option<f32>,
    /// The panel's receiver's level, dBm, when it is not where the DUO's own
    /// receiver (VFO A) listens: RADIO's S-meter reads this then.
    pub rx_dbm: Option<f32>,
}

/// What the operator did on the TUNE strip.
#[derive(Default)]
pub(crate) struct TuneOut {
    pub start: Option<Keying>,
    pub stop: bool,
    /// A new menu 34 setting, by its code.
    pub power: Option<u8>,
    /// Put menu 32 on USB audio.
    pub usb_audio: bool,
}

/// The TUNE and RIG TUNE buttons, the radio's power buttons and the Tune
/// slider, at the radio window's scale where it is drawn there.
pub(crate) fn tune_strip(ui: &mut egui::Ui, rig: &RigStatus, args: &mut TuneArgs<'_>) -> TuneOut {
    let mut out = TuneOut::default();
    let tuning = rig.tuning;
    ui.horizontal_wrapped(|ui| {
        let keyed = |k: Keying| tuning.is_some_and(|t| t.keying == k);
        // Which path each takes, said on the key (AJ3G, 11 Nov 2026: the two
        // read alike): TX TUNE is the audio path FT8 and WSPR use; RIG TUNE
        // is the radio's own carrier at the full menu 34 power, which it has
        // no slider to lower, so its key says that power.
        for (k, label, blocked, hover) in [
            (
                Keying::Audio,
                TONE_LABEL.to_string(),
                args.tone_blocked.as_deref(),
                "A 1 kHz tone from the panel through the radio's USB audio, at the TX TUNE \
                 level slider's share of menu 34: the same path as FT8 and WSPR from the TX \
                 window. It keys the radio as its PTT does, and so the amplifier's PTT line \
                 too (SDRoxide's TUNE). Click again to unkey. It also unkeys by itself at the \
                 time limit, and on a trip or a high SWR.",
            ),
            (
                Keying::Carrier,
                carrier_label(planned_watts(rig, Keying::Carrier, 1.0)),
                args.carrier_blocked.as_deref(),
                "The DUO's own carrier (TX2;) at the menu 34 power, set with the power keys; \
                 no slider, no audio. The FDM-DUO may not key its PTT output for this: if the \
                 amplifier stays in receive, use TX TUNE. Click again to unkey.",
            ),
        ] {
            let on = keyed(k);
            let can = on || (tuning.is_none() && blocked.is_none());
            let resp = ui.add_enabled_ui(can, |ui| chrome::chip(ui, on, &label)).inner;
            let resp = match blocked.filter(|_| tuning.is_none()) {
                Some(why) => resp.on_disabled_hover_text(why),
                None => resp.on_hover_text(hover),
            };
            if resp.clicked() {
                if tuning.is_some() {
                    out.stop = true;
                } else {
                    out.start = Some(k);
                }
            }
        }

        ui.add_space(z(6.0));
        ui.label(RichText::new("for").size(z(11.0)));
        ui.add(egui::DragValue::new(args.secs).range(TUNE_SECS).suffix(" s"))
            .on_hover_text("TUNE unkeys by itself after this long.");

        if let Some(t) = tuning {
            let left = t.until.saturating_duration_since(Instant::now()).as_secs_f32();
            ui.label(
                RichText::new(format!("keyed · {left:.0} s left"))
                    .size(z(11.0))
                    .color(theme::CYAN()),
            );
        } else if let Some(n) = &rig.tune_note {
            // Yellow only for a stop that is a warning; the time limit is not.
            let col = if n.starts_with("time limit") { theme::TEXT() } else { theme::YELLOW() };
            ui.label(RichText::new(format!("Stopped: {n}")).size(z(11.0)).color(col));
        }
    });

    let mut refused = None;
    ui.horizontal(|ui| {
        // The power buttons, captioned underneath so they read as the
        // radio's own and not the amplifier's.
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = z(2.0);
            // A click shows at once, as asked, until the radio's read-back
            // agrees or POWER_WAIT_S passes (AJ3G, 24 Oct 2026: the keys
            // seemed not to respond while the read-back was slow).
            let key = egui::Id::new("duo-power-asked");
            let t = ui.ctx().input(|i| i.time);
            let mut asked = ui.ctx().data(|d| d.get_temp::<(u8, f64)>(key));
            // Taken, or long enough ago that the note has been read.
            if asked.is_some_and(|(c, at)| rig.power_code == Some(c) || t - at > 15.0) {
                asked = None;
            }
            let pending = asked.filter(|(_, at)| t - at < POWER_WAIT_S).map(|(c, _)| c);
            let row = ui.horizontal(|ui| {
                for (label, code) in PRESETS {
                    let lit = pending.map_or(rig.power_code == Some(code), |c| c == code);
                    let resp = ui
                        .add_enabled_ui(rig.connected, |ui| chrome::chip(ui, lit, label))
                        .inner
                        .on_hover_text(preset_hover(code));
                    if resp.clicked() && rig.power_code != Some(code) {
                        out.power = Some(code);
                        asked = Some((code, t));
                    }
                }
            });
            if pending.is_some() {
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
            }
            ui.ctx().data_mut(|d| match asked {
                Some(a) => {
                    d.insert_temp(key, a);
                }
                None => d.remove::<(u8, f64)>(key),
            });
            refused = asked.filter(|(_, at)| t - at >= POWER_WAIT_S).map(|(c, _)| c);
            let ink = ui.visuals().widgets.inactive.fg_stroke.color;
            let w = row.response.rect.width();
            ui.allocate_ui_with_layout(
                egui::vec2(w, 0.0),
                egui::Layout::top_down(egui::Align::Center),
                |ui| {
                    ui.set_min_width(w);
                    let caption = match rig.power() {
                        Some(p) if !PRESETS.iter().any(|x| x.1 == p.code) => {
                            format!("{PRESETS_CAPTION} · {}", p.label)
                        }
                        _ => PRESETS_CAPTION.to_string(),
                    };
                    ui.label(RichText::new(caption).size(z(9.5)).strong().color(ink));
                },
            );
        });
        ui.add_space(z(10.0));
        ui.label("TX TUNE level").on_hover_text(
            "TX TUNE's share of menu 34 (the power keys): the tone's level into the radio's \
             USB audio.",
        );
        let est = planned_watts(rig, Keying::Audio, *args.drive);
        let over = matches!((est, args.limit_in_line), (Some(w), Some(l)) if w > l);
        let text = est.map_or("≈ — W".to_string(), |w| format!("≈ {w:.1} W"));
        ui.label(RichText::new(text).monospace().size(z(11.0)).color(if over {
            theme::YELLOW()
        } else {
            theme::TEXT()
        }))
        .on_hover_text(
            "What TX TUNE should put out at this level, from AJ3G's measurements: the \
             slider spans menu 34's power in a straight line. The radio's and the \
             amplifier's own meters show what it really does.",
        );
        // A fixed rail: `slider_vertical` is the skin without `fit_rail`'s
        // measuring, which the chips before it on this row throw off.
        ui.spacing_mut().slider_width = z(180.0);
        chrome::slider_vertical(
            ui,
            egui::Slider::new(args.drive, 0.0..=1.0)
                .show_value(true)
                .custom_formatter(|v, _| format!("{:.0}% of menu 34", v * 100.0)),
        )
        .on_hover_text(
            "TX TUNE's level as a share of menu 34 (the power keys): the tone's volume into \
             the radio's USB audio. It can be moved while TX TUNE is keyed. RIG TUNE does not \
             use it.",
        );
    });

    if rig.tx_input.as_deref().is_some_and(|i| i != "USB audio") && rig.connected {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!(
                    "The radio's TX IN (menu 32) is on {}: TUNE's tone can't reach it.",
                    rig.tx_input.as_deref().unwrap_or("?")
                ))
                .size(z(11.0))
                .color(theme::YELLOW()),
            );
            if chrome::chip(ui, false, "USB AUDIO")
                .on_hover_text(
                    "Set menu 32 TX IN to USB audio, so the radio transmits the computer's \
                     audio (the panel's tone, or a digital-mode program's) and not the \
                     microphone. It stays set after the panel closes.",
                )
                .clicked()
            {
                out.usb_audio = true;
            }
        });
    }
    if let Some(c) = refused {
        let name = |c: Option<u8>| {
            c.and_then(|c| rigcat::elad::POWER_STEPS.get(c as usize)).map_or("?", |p| p.label)
        };
        ui.label(
            RichText::new(format!(
                "The radio still reads {}: it didn't take {}. Another program setting its power \
                 over CAT (SDR Console, SDRoxide) puts it back; close it, or click again.",
                name(rig.power_code),
                name(Some(c))
            ))
            .size(z(11.0))
            .color(theme::YELLOW()),
        );
    }
    if let Some(e) = &args.tone_error {
        ui.label(RichText::new(format!("Tone: {e}")).size(z(11.0)).color(theme::YELLOW()));
    }
    if let Some(l) = args.limit_in_line
        && rig.power().is_some_and(|p| p.watts > l)
    {
        ui.label(
            RichText::new(format!(
                "Menu 34 is over the amplifier's {l:.0} W drive limit for PTT: keep the Tune \
                 slider, and other programs' audio, below it."
            ))
            .size(z(11.0))
            .color(theme::YELLOW()),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use hardrock::AmpModel;
    use rigcat::elad;

    fn amp(ptt: bool, band: u8) -> AmpStatus {
        AmpStatus {
            configured: true,
            connected: true,
            model: AmpModel::default(),
            readings: Some(AmpReadings { ptt, band: Some(band), ..AmpReadings::default() }),
            ..AmpStatus::default()
        }
    }

    fn duo(hz: u64, code: u8) -> RigStatus {
        RigStatus {
            connected: true,
            freq_hz: Some(hz),
            power_code: Some(code),
            power_steps: &elad::POWER_STEPS,
            tx_input: Some("USB audio".into()),
            mode: Some("USB"),
            ..RigStatus::default()
        }
    }

    fn w(r: &RigStatus) -> Option<f32> {
        planned_watts(r, Keying::Carrier, 1.0)
    }

    #[test]
    fn the_keys_say_their_path_and_rig_tune_its_power() {
        assert!(TONE_LABEL.contains("USB audio"));
        // MAX on the DUO: menu 34's watts, as the carrier keys at.
        let r = duo(14_074_000, 9);
        let w = planned_watts(&r, Keying::Carrier, 1.0).unwrap();
        assert_eq!(carrier_label(Some(w)), format!("RIG TUNE (carrier {w:.1} W)"));
        assert_eq!(carrier_label(Some(1.0)), "RIG TUNE (carrier 1.0 W)");
        assert_eq!(carrier_label(None), "RIG TUNE (carrier, menu 34)");
        // Not read yet: no figure.
        let unread = RigStatus { power_code: None, ..duo(14_074_000, 9) };
        assert_eq!(planned_watts(&unread, Keying::Carrier, 1.0), None);
    }

    #[test]
    fn an_outside_program_keys_only_when_tune_could_at_the_full_setting() {
        // In bypass with both links: allowed.
        let r = duo(14_074_000, 9);
        assert_eq!(outside_ptt_blocked(&amp(false, 7), &r), None);
        // In line at MAX (8.3 W): over the HR500's drive limit (#6).
        assert!(outside_ptt_blocked(&amp(true, 5), &r).unwrap().contains("drive limit"));
        // At 5 W on the amplifier's band: allowed; on the wrong band: not.
        let five = duo(14_074_000, 8);
        assert_eq!(outside_ptt_blocked(&amp(true, 5), &five), None);
        assert!(outside_ptt_blocked(&amp(true, 7), &five).is_some());
        // Not while TUNE runs or the radio transmits on its own.
        let now = Instant::now();
        let tuning = RigStatus {
            tuning: Some(rigcat::Tuning { since: now, until: now, keying: Keying::Audio }),
            ..duo(14_074_000, 8)
        };
        assert!(outside_ptt_blocked(&amp(false, 5), &tuning).unwrap().contains("TUNE"));
        let keyed = RigStatus { tx_state: Some(1), ..duo(14_074_000, 8) };
        assert!(outside_ptt_blocked(&amp(false, 5), &keyed).is_some());
        // A tripped or absent amplifier refuses it, as it refuses TUNE.
        let mut tripped = amp(false, 5);
        tripped.alerts.tripped = true;
        assert!(outside_ptt_blocked(&tripped, &five).is_some());
        let gone = AmpStatus { connected: false, ..amp(false, 5) };
        assert!(outside_ptt_blocked(&gone, &five).is_some());
    }

    #[test]
    fn in_bypass_tune_needs_only_both_links() {
        let r = duo(14_074_000, 9);
        assert_eq!(tune_blocked(&amp(false, 7), &r, w(&r)), None);
        let off = RigStatus { connected: false, ..duo(14_074_000, 2) };
        assert!(tune_blocked(&amp(false, 5), &off, w(&off)).is_some());
        let no_amp = AmpStatus { connected: false, ..amp(false, 5) };
        assert!(tune_blocked(&no_amp, &r, w(&r)).is_some());
    }

    #[test]
    fn in_line_the_bands_must_match() {
        // 20 m is HRBN 5, 40 m is 7.
        let r = duo(14_074_000, 2);
        assert_eq!(tune_blocked(&amp(true, 5), &r, w(&r)), None);
        let why = tune_blocked(&amp(true, 7), &r, w(&r)).unwrap();
        assert!(why.contains("wrong low-pass filter"), "{why}");
        let six = duo(50_313_000, 2);
        assert!(tune_blocked(&amp(true, 5), &six, w(&six)).is_some());
    }

    #[test]
    fn in_line_tune_stays_within_the_drive_limit() {
        // MAX is taken as 8.3 W, over the HR500's 8 W; 5 W is within it.
        let max = duo(14_074_000, 9);
        assert!(tune_blocked(&amp(true, 5), &max, w(&max)).is_some());
        let five = duo(14_074_000, 8);
        assert_eq!(tune_blocked(&amp(true, 5), &five, w(&five)), None);
        // The tone at 90% of MAX is about 7.5 W: within it. At 100%, not.
        let tone = |d| planned_watts(&max, Keying::Audio, d);
        assert_eq!(tune_blocked(&amp(true, 5), &max, tone(0.9)), None);
        assert!(tune_blocked(&amp(true, 5), &max, tone(1.0)).is_some());
    }

    #[test]
    fn the_tune_slider_is_a_share_of_the_power_setting() {
        let r = duo(14_074_000, 8);
        assert_eq!(planned_watts(&r, Keying::Audio, 0.5), Some(2.5));
        assert_eq!(planned_watts(&r, Keying::Carrier, 0.5), Some(5.0));
        assert_eq!(planned_watts(&RigStatus::default(), Keying::Audio, 0.5), None);
    }

    #[test]
    fn the_tone_needs_its_sound_output_usb_audio_and_a_sideband() {
        let r = duo(14_074_000, 9);
        assert_eq!(tone_blocked(&r, Some("FDM-DUO Audio")), None);
        assert!(tone_blocked(&r, None).unwrap().contains("sound output"));
        let mic = RigStatus { tx_input: Some("Microphone".into()), ..r.clone() };
        assert!(tone_blocked(&mic, Some("FDM-DUO Audio")).unwrap().contains("menu 32"));
        let cw = RigStatus { mode: Some("CW"), ..r };
        assert!(tone_blocked(&cw, Some("FDM-DUO Audio")).unwrap().contains("CW"));
    }

    #[test]
    fn a_trip_lets_tune_clear_it_but_a_new_one_stops_it() {
        let mut a = amp(false, 5);
        a.alerts = AmpAlerts { tripped: true, ..AmpAlerts::default() };
        let r = duo(14_074_000, 2);
        // The HR500 clears a trip only when keyed: TUNE may, at low power.
        assert_eq!(tune_blocked(&a, &r, w(&r)), None);
        assert!(tune_blocked(&a, &r, Some(20.0)).unwrap().contains("tripped"));
        // Carried in from before the TUNE: it runs on. A new one stops it.
        assert_eq!(tune_must_stop(&a, &r, 5.0, w(&r), true), None);
        assert!(tune_must_stop(&a, &r, 5.0, w(&r), false).is_some());
        // Outside programs and CW messages are still refused.
        assert!(key_blocked(&a, &r, w(&r)).is_some());
        assert!(outside_ptt_blocked(&a, &r).is_some());
    }

    #[test]
    fn a_high_swr_stops_tune() {
        let mut a = amp(true, 5);
        let r = duo(14_074_000, 2);
        assert_eq!(tune_must_stop(&a, &r, 3.0, w(&r), false), None);
        a.readings.as_mut().unwrap().swr = 3.2;
        assert!(tune_must_stop(&a, &r, 3.0, w(&r), false).unwrap().contains("SWR"));
        let mut r = duo(14_074_000, 2);
        r.swr = elad::parse_swr("0 04.00");
        assert!(
            tune_must_stop(&amp(false, 5), &r, 3.0, w(&r), false).unwrap().contains("radio's SWR")
        );
    }

    #[test]
    fn an_over_rides_out_a_short_silence_on_the_last_readings() {
        let t0 = Instant::now();
        let r = duo(14_074_000, 2);
        let mut hold = AmpHold::default();
        let live = AmpStatus { readings_at: Some(t0), ..amp(true, 5) };
        hold.note(&live, t0);
        // The link gives up: no readings, not connected.
        let gone =
            AmpStatus { connected: false, readings: None, readings_at: None, ..live.clone() };
        let mut stop = |at: Duration| {
            hold.note(&gone, t0 + at);
            match hold.over_view(&gone, t0 + at) {
                Ok(v) => tune_must_stop(&v, &r, 3.0, w(&r), false),
                Err(why) => Some(why),
            }
        };
        // Within 5 s of the last answer the over goes on.
        assert_eq!(stop(Duration::from_secs(2)), None);
        assert_eq!(stop(Duration::from_secs(5)), None);
        // After that it stops, as before.
        let why = stop(Duration::from_millis(5100)).unwrap();
        assert!(why.contains("hasn't answered"), "{why}");
    }

    #[test]
    fn a_link_holding_old_readings_is_judged_by_their_age() {
        // Connected, the link still showing readings that are 6 s old.
        let t0 = Instant::now();
        let r = duo(14_074_000, 2);
        let mut hold = AmpHold::default();
        let held = AmpStatus { readings_at: Some(t0), ..amp(true, 5) };
        hold.note(&held, t0 + Duration::from_secs(6));
        assert!(hold.over_view(&held, t0 + Duration::from_secs(6)).is_err());
        assert!(hold.over_view(&held, t0 + Duration::from_secs(3)).is_ok());
        // And a start needs them live, whatever an over may do.
        let stale = AmpStatus {
            readings_at: Instant::now().checked_sub(Duration::from_secs(4)),
            ..amp(true, 5)
        };
        assert!(tune_blocked(&stale, &r, w(&r)).unwrap().contains("hasn't answered"));
        let fresh = AmpStatus { readings_at: Some(Instant::now()), ..amp(true, 5) };
        assert_eq!(tune_blocked(&fresh, &r, w(&r)), None);
        // With no readings at all, a start is refused as before.
        let gone = AmpStatus { connected: false, readings: None, ..amp(true, 5) };
        assert!(tune_blocked(&gone, &r, w(&r)).is_some());
    }

    #[test]
    fn a_held_over_still_stops_at_once_for_what_the_amplifier_reports() {
        let t0 = Instant::now();
        let r = duo(14_074_000, 2);
        let mut hold = AmpHold::default();
        // The last answer before the silence reports trouble: each still
        // stops the over while the readings are held.
        let cases: [(&str, fn(&mut AmpStatus)); 4] = [
            ("tripped", |a| a.alerts.tripped = true),
            ("SWR", |a| a.readings.as_mut().unwrap().swr = 3.5),
            ("Drive", |a| a.readings.as_mut().unwrap().drive_w = 9.0),
            ("low-pass", |a| a.readings.as_mut().unwrap().band = Some(7)),
        ];
        for (word, f) in cases {
            let mut last = AmpStatus { readings_at: Some(t0), ..amp(true, 5) };
            f(&mut last);
            hold.note(&last, t0);
            let gone = AmpStatus { connected: false, readings: None, ..amp(true, 5) };
            let v = hold.over_view(&gone, t0 + Duration::from_secs(2)).unwrap();
            let why = tune_must_stop(&v, &r, 3.0, w(&r), false).unwrap();
            assert!(why.contains(word), "{word}: {why}");
        }
    }

    #[test]
    fn drive_past_the_limit_in_line_stops_tune() {
        let mut a = amp(true, 5);
        a.readings.as_mut().unwrap().drive_w = 9.0;
        let r = duo(14_074_000, 2);
        assert!(tune_must_stop(&a, &r, 5.0, w(&r), false).unwrap().contains("Drive"));
    }

    /// A TUNE's SWR rule, read once at each of `steps` (time since keying,
    /// the amplifier, the radio's SWR).
    fn tune_run(steps: &[(u64, &AmpStatus, f32)]) -> Vec<Option<String>> {
        let t0 = Instant::now();
        let mut g = TuneSwr::default();
        steps
            .iter()
            .map(|(ms, a, swr)| {
                let mut r = duo(14_074_000, 2);
                r.swr = elad::parse_swr(&format!("0 {swr:05.2}"));
                let at = Duration::from_millis(*ms);
                g.must_stop(a, &r, 3.0, w(&r), false, at, t0 + at)
            })
            .collect()
    }

    fn atu_tuning(mut a: AmpStatus) -> AmpStatus {
        a.atu = Some(hardrock::AtuReadout { active: true, tuning: true, ..Default::default() });
        a
    }

    #[test]
    fn the_tune_limit_is_twice_the_warning_up_to_just_past_the_tuners_six() {
        assert_eq!(swr_tune_limit(2.5), 5.0);
        assert_eq!(swr_tune_limit(3.0), 6.0);
        // The 5.0 default: 6.1:1, just past where the ATU-500 gives up at
        // 6:1, not 10:1.
        assert_eq!(swr_tune_limit(5.0), 6.1);
        // Never under the warning itself.
        assert_eq!(swr_tune_limit(8.0), 8.0);
    }

    #[test]
    fn a_tune_ignores_the_radios_swr_for_its_first_five_seconds() {
        let a = amp(false, 5);
        let out = tune_run(&[(0, &a, 9.9), (2000, &a, 9.9), (4900, &a, 9.9)]);
        assert!(out.iter().all(Option::is_none), "{out:?}");
    }

    #[test]
    fn after_that_a_tune_stops_only_for_swr_held_over_twice_the_warning() {
        let a = amp(false, 5);
        // 5:1 is over the 3:1 warning but under the 6:1 tune limit.
        let out = tune_run(&[(6000, &a, 5.0), (7000, &a, 5.0), (8000, &a, 5.0)]);
        assert!(out.iter().all(Option::is_none), "{out:?}");
        // A spike over 6:1 for less than the dwell, then back down: runs on.
        let out = tune_run(&[(6000, &a, 8.0), (6200, &a, 8.0), (6300, &a, 2.0), (6400, &a, 8.0)]);
        assert!(out.iter().all(Option::is_none), "{out:?}");
        // Held for the dwell: stops.
        let out = tune_run(&[(6000, &a, 8.0), (6100, &a, 8.0), (6300, &a, 8.0)]);
        assert_eq!(out[..2], [None, None]);
        assert!(out[2].as_deref().unwrap().contains("radio's SWR"), "{out:?}");
    }

    #[test]
    fn the_radios_swr_never_stops_a_tune_while_the_amplifiers_tuner_tunes() {
        let a = atu_tuning(amp(false, 5));
        let out = tune_run(&[(6000, &a, 9.9), (7000, &a, 9.9), (9000, &a, 9.9)]);
        assert!(out.iter().all(Option::is_none), "{out:?}");
    }

    #[test]
    fn in_line_the_amplifiers_swr_stops_a_tune_at_once_unless_its_tuner_tunes() {
        let mut a = amp(true, 5);
        a.readings.as_mut().unwrap().swr = 3.2;
        let out = tune_run(&[(100, &a, 1.0)]);
        assert!(out[0].as_deref().unwrap().contains("amplifier's SWR"), "{out:?}");
        // Its tuner tuning: the doubled limit, the settle time and the dwell.
        let mut t = atu_tuning(a.clone());
        t.readings.as_mut().unwrap().swr = 7.0;
        let out = tune_run(&[(100, &t, 1.0), (5000, &t, 1.0), (5200, &t, 1.0), (5300, &t, 1.0)]);
        assert_eq!(out[..3], [None, None, None]);
        assert!(out[3].as_deref().unwrap().contains("amplifier's SWR"), "{out:?}");
        t.readings.as_mut().unwrap().swr = 5.0;
        assert!(tune_run(&[(6000, &t, 1.0), (7000, &t, 1.0)]).iter().all(Option::is_none));
    }

    #[test]
    fn a_tune_still_stops_at_once_for_everything_but_swr() {
        let mut a = amp(true, 5);
        a.readings.as_mut().unwrap().drive_w = 9.0;
        assert!(tune_run(&[(100, &a, 1.0)])[0].as_deref().unwrap().contains("Drive"));
        let mut a = amp(false, 5);
        a.alerts.tripped = true;
        assert!(tune_run(&[(100, &a, 1.0)])[0].as_deref().unwrap().contains("tripped"));
    }

    #[test]
    fn an_ordinary_over_keeps_its_one_reading_swr_stop() {
        let mut r = duo(14_074_000, 2);
        r.swr = elad::parse_swr("0 03.50");
        assert!(tune_must_stop(&amp(false, 5), &r, 3.0, w(&r), false).is_some());
    }

    /// An ordinary over's SWR rule, read once at each of `steps` (time since
    /// keying, the amplifier, the radio's SWR, the radio's forward watts).
    fn over_run(steps: &[(u64, &AmpStatus, f32, Option<f32>)]) -> Vec<Option<String>> {
        let t0 = Instant::now();
        let mut g = OverSwr::default();
        steps
            .iter()
            .map(|(ms, a, swr, fwd)| {
                let mut r = duo(14_074_000, 2);
                r.swr = elad::parse_swr(&format!("0 {swr:05.2}"));
                r.fwd = fwd.map(|w| rigcat::Watts { w, valid: true });
                r.rfl = fwd.map(|_| rigcat::Watts { w: 0.5, valid: true });
                let at = Duration::from_millis(*ms);
                g.must_stop(a, &r, 3.0, w(&r), false, at, t0 + at)
            })
            .collect()
    }

    fn amp_swr(swr: f32, fwd_w: f32) -> AmpStatus {
        let mut a = amp(true, 5);
        let r = a.readings.as_mut().unwrap();
        (r.swr, r.fwd_w, r.rfl_w) = (swr, fwd_w, fwd_w / 10.0);
        a
    }

    #[test]
    fn an_over_ignores_swr_at_key_down() {
        let a = amp_swr(9.9, 400.0);
        let out = over_run(&[
            (0, &a, 9.9, Some(4.0)),
            (500, &a, 9.9, Some(4.0)),
            (900, &a, 9.9, Some(4.0)),
        ]);
        assert!(out.iter().all(Option::is_none), "{out:?}");
    }

    #[test]
    fn an_over_ignores_swr_read_off_almost_no_power() {
        // The radio's ratio with 0.1 W forward, the amplifier's with 2 W.
        let quiet = amp_swr(9.9, 2.0);
        let out = over_run(&[
            (1000, &quiet, 9.9, Some(0.1)),
            (2000, &quiet, 9.9, Some(0.1)),
            (3000, &quiet, 9.9, Some(0.1)),
        ]);
        assert!(out.iter().all(Option::is_none), "{out:?}");
    }

    #[test]
    fn a_one_reading_spike_no_longer_stops_an_over() {
        let a = amp(false, 5);
        let out = over_run(&[
            (1000, &a, 3.5, Some(4.0)),
            (1100, &a, 1.2, Some(4.0)),
            (1200, &a, 3.5, Some(4.0)),
            (1300, &a, 1.2, Some(4.0)),
        ]);
        assert!(out.iter().all(Option::is_none), "{out:?}");
    }

    #[test]
    fn swr_held_over_the_warning_still_stops_an_over() {
        let a = amp(false, 5);
        let out = over_run(&[
            (1000, &a, 3.5, Some(4.0)),
            (1200, &a, 3.5, Some(4.0)),
            (1500, &a, 3.5, Some(4.0)),
        ]);
        assert_eq!(out[..2], [None, None]);
        let why = out[2].as_deref().unwrap();
        assert!(
            why.contains("radio's SWR stayed at 3.5:1") && why.contains("forward 4.0 W"),
            "{why}"
        );
        // With no forward reading from the radio its SWR is still believed.
        let out = over_run(&[(1000, &a, 3.5, None), (1600, &a, 3.5, None)]);
        assert!(out[1].is_some(), "{out:?}");
        // The amplifier's own, held, in line with real power.
        let a = amp_swr(3.5, 400.0);
        let out = over_run(&[(1000, &a, 1.1, Some(4.0)), (1500, &a, 1.1, Some(4.0))]);
        assert!(out[1].as_deref().unwrap().contains("amplifier's SWR stayed at 3.5:1"), "{out:?}");
    }

    #[test]
    fn twice_the_warning_on_the_amplifier_stops_an_over_at_once() {
        let a = amp_swr(6.5, 400.0);
        let out = over_run(&[(1000, &a, 1.1, Some(4.0))]);
        assert!(out[0].as_deref().unwrap().contains("amplifier's SWR reached 6.5:1"), "{out:?}");
    }

    #[test]
    fn past_the_tuners_six_to_one_an_over_stops_at_once() {
        // At the 5.0 default warning twice the warning is 10:1; the ATU-500
        // gives up at about 6:1 (WA2EUJ), so past 6.1:1, 7:1 is a fault,
        // stopped on one reading.
        let a = amp_swr(7.0, 400.0);
        let mut r = duo(14_074_000, 2);
        r.swr = elad::parse_swr("0 01.10");
        let t0 = Instant::now();
        let at = Duration::from_millis(1000);
        let why = OverSwr::default().must_stop(&a, &r, 5.0, w(&r), false, at, t0 + at);
        assert!(why.as_deref().unwrap().contains("amplifier's SWR reached 7.0:1"), "{why:?}");
    }

    #[test]
    fn an_over_waits_for_the_amplifiers_tuner() {
        let tuning = atu_tuning(amp_swr(9.9, 400.0));
        let done = amp_swr(1.2, 400.0);
        let out = over_run(&[
            (1000, &tuning, 9.9, Some(4.0)),
            (3000, &tuning, 9.9, Some(4.0)),
            (3500, &done, 3.5, Some(4.0)),
            (3900, &done, 3.5, Some(4.0)),
        ]);
        assert!(out.iter().all(Option::is_none), "{out:?}");
        // A second after the tuner finished, SWR is judged again.
        let out = over_run(&[
            (1000, &tuning, 9.9, Some(4.0)),
            (4100, &done, 3.5, Some(4.0)),
            (4700, &done, 3.5, Some(4.0)),
        ]);
        assert!(out[2].is_some(), "{out:?}");
    }

    #[test]
    fn a_trip_still_stops_an_over_at_once() {
        let mut a = amp(true, 5);
        a.alerts.tripped = true;
        assert!(over_run(&[(0, &a, 1.0, Some(4.0))])[0].as_deref().unwrap().contains("tripped"));
    }
}
