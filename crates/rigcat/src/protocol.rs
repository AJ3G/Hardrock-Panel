//! What every radio's CAT dialect provides, so the link and the panel need
//! not know which radio it is. Each radio is a module that implements
//! [`Protocol`]: the FDM-DUO's is [`crate::elad`]. Kenwood, Yaesu and Icom
//! would each be one more module and one more [`crate::RigModel`].
//!
//! Frames are bytes, not text, so a binary dialect (Icom's CI-V) fits as well
//! as the `;`-terminated ASCII ones.

use crate::RigStatus;

/// What a frame does to the radio. The link decides by this what it may send
/// and when (safety interlocks #5 and #6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind {
    /// Asks for something; changes nothing.
    Read,
    /// Sets the radio's transmit power (the FDM-DUO's menu 34), when the
    /// operator picks a step.
    SetPower,
    /// Keys the radio's own TUNE carrier. Sent only by the TUNE routine.
    Key,
    /// Keys an ordinary transmission of whatever audio is on the radio's
    /// transmit input: the panel's tone, for TUNE with drive. Sent only by
    /// the TUNE routine, and only once interlock #7's reads have passed.
    KeyAudio,
    /// Puts the radio's transmit audio on its USB input (the FDM-DUO's menu
    /// 32), when the operator asks.
    SetInput,
    /// Unkeys. Always safe to send.
    Unkey,
    /// Changes one of the radio's settings: the dial, the mode, a menu. Never
    /// sent while the radio transmits (interlock #5), and read back after.
    SetSetting,
    /// Starts one of the radio's stored CW messages, which transmits. Sent
    /// only when the panel's TUNE checks pass (interlock #6), never in TX.
    KeyCw,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    pub bytes: Vec<u8>,
    pub kind: FrameKind,
}

impl Frame {
    pub fn new(bytes: impl Into<Vec<u8>>, kind: FrameKind) -> Frame {
        Frame { bytes: bytes.into(), kind }
    }

    pub fn read(bytes: impl Into<Vec<u8>>) -> Frame {
        Frame::new(bytes, FrameKind::Read)
    }
}

/// One answer from the radio: which command it answers (`key`) and what it
/// says (`body`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reply {
    pub key: String,
    pub body: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Incoming {
    Reply(Reply),
    /// The radio didn't know the command.
    Refused,
}

/// A setting the radio answers a READ for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Item {
    /// The protocol's own name for the READ (the FDM-DUO's command letters).
    pub cmd: &'static str,
    /// The radio's own menu number, where the setting is one of its menus.
    pub menu: Option<u8>,
    pub name: &'static str,
}

/// One of the transmit power settings the radio offers over CAT.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PowerStep {
    pub label: &'static str,
    /// What it puts out, as near as is known, for the amplifier's drive
    /// limit (interlock #6).
    pub watts: f32,
    /// The radio's own code for it.
    pub code: u8,
}

/// Which pop-out or window a setting belongs in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Group {
    /// On the radio face: dial, mode, split, RIT, ATT, antennas.
    Radio,
    /// The radio's own receiver and audio.
    Receive,
    Cw,
    /// Transmit audio and the transmitter.
    TxAudio,
    /// TUNE and PTT.
    Tune,
    /// Everything else in MENU: display, keys, offsets.
    Menu,
    /// Read only: model, firmware, serial number, CAT baud.
    Info,
}

/// What values a setting takes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// Off (0) and on (1).
    Toggle,
    /// Exactly one of these (value sent, label shown).
    Choice(&'static [(&'static str, &'static str)]),
    /// A whole number in `lo..=hi` in steps of `step`, zero-padded to the
    /// width the radio last answered with (or `width`).
    Range { lo: i32, hi: i32, step: i32, width: usize, unit: &'static str },
    /// Text of at most `len` characters from the radio's set.
    Text { len: usize },
    /// Shown as the radio answers it and not changed from the panel: the
    /// format isn't known well enough to send, or changing it would cut the
    /// link.
    ReadOnly,
}

/// A setting the panel can show, and change unless it is read-only.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Control {
    /// The command letters.
    pub cmd: &'static str,
    /// The whole READ frame.
    pub read: &'static str,
    /// Fixed parameters between the letters and the value (`0` for the
    /// FDM-DUO's `CG0nnn;`, for example).
    pub prefix: &'static str,
    pub menu: Option<u8>,
    pub name: &'static str,
    /// What it does, in a line.
    pub what: &'static str,
    pub group: Group,
    pub kind: Kind,
}

impl Control {
    pub fn editable(&self) -> bool {
        !matches!(self.kind, Kind::ReadOnly)
    }

    /// The value part of an answer: the body without the fixed prefix.
    ///
    /// The prefix is taken off only when what is left is at least as wide
    /// as the value itself, so an answer that leaves the prefix out still
    /// reads right: `TB005` and `TB05` are both TX BW 05, and `FD0` is
    /// FM DEV 0, not an empty value (AJ3G saw TX BW stay blank, 14 Nov 2026).
    pub fn value_of<'a>(&self, body: &'a str) -> &'a str {
        match body.strip_prefix(self.prefix) {
            Some(v) if !self.prefix.is_empty() && v.trim_end().len() >= self.value_width() => v,
            _ => body,
        }
    }

    /// The narrowest a value of this control is written.
    fn value_width(&self) -> usize {
        match self.kind {
            Kind::Range { width, .. } => width,
            Kind::Choice(opts) => opts.iter().map(|(o, _)| o.len()).min().unwrap_or(1),
            Kind::Text { len } => len,
            Kind::Toggle | Kind::ReadOnly => 1,
        }
        .max(1)
    }

    /// The choice `value` stands for: the same code, or for numeric codes
    /// the same number however it is padded (`5`, `05` and `005` are all
    /// `05`).
    pub fn choice_of(&self, value: &str) -> Option<&'static (&'static str, &'static str)> {
        let Kind::Choice(opts) = self.kind else { return None };
        let v = value.trim();
        opts.iter().find(|(o, _)| *o == v).or_else(|| {
            let n: i64 = v.parse().ok()?;
            opts.iter().find(|(o, _)| o.parse::<i64>().ok() == Some(n))
        })
    }

    /// The value in words.
    pub fn label(&self, value: &str) -> String {
        match self.kind {
            Kind::Toggle => match value.trim() {
                "0" => "Off".into(),
                "1" => "On".into(),
                v => v.into(),
            },
            Kind::Choice(_) => {
                self.choice_of(value).map(|(_, l)| l.to_string()).unwrap_or_else(|| value.into())
            }
            Kind::Range { unit, .. } => match value.trim().parse::<i32>() {
                Ok(n) if unit.is_empty() => n.to_string(),
                Ok(n) => format!("{n} {unit}"),
                Err(_) => value.into(),
            },
            Kind::Text { .. } => value.trim_end().into(),
            Kind::ReadOnly => value.into(),
        }
    }

    /// The SET frame's body for `value`, or why not. `last` is the value
    /// the radio last answered with, whose width a number is padded to.
    pub fn encode(&self, value: &str, last: Option<&str>) -> Result<String, String> {
        let v = value.trim();
        let body = match self.kind {
            Kind::Toggle if v == "0" || v == "1" => v.to_string(),
            Kind::Toggle => return Err(format!("{}: {v} isn't 0 or 1", self.name)),
            Kind::Choice(_) => match self.choice_of(v) {
                Some((o, _)) => o.to_string(),
                None => return Err(format!("{}: {v} isn't one of its choices", self.name)),
            },
            Kind::Range { lo, hi, step, width, .. } => {
                let n: i32 = v.parse().map_err(|_| format!("{}: {v} isn't a number", self.name))?;
                if n < lo || n > hi || (n - lo) % step.max(1) != 0 {
                    return Err(format!("{}: {n} is outside {lo} to {hi}", self.name));
                }
                let w = last.map(|l| l.trim_end().len()).filter(|w| *w > 0).unwrap_or(width);
                format!("{n:0w$}")
            }
            Kind::Text { len } => {
                if v.chars().count() > len || !v.chars().all(cw_char) {
                    return Err(format!("{}: only {len} of A-Z 0-9 space / ? . , = -", self.name));
                }
                format!("{:<len$}", v.to_ascii_uppercase())
            }
            Kind::ReadOnly => return Err(format!("{} is read only", self.name)),
        };
        Ok(format!("{}{}", self.prefix, body))
    }

    /// Whether `body` (after the letters) is a SET this control would send.
    pub fn accepts(&self, body: &str) -> bool {
        let Some(v) = body.strip_prefix(self.prefix) else { return false };
        match self.kind {
            Kind::Toggle => v == "0" || v == "1",
            Kind::Choice(opts) => opts.iter().any(|(o, _)| *o == v),
            Kind::Range { lo, hi, step, .. } => {
                !v.is_empty()
                    && v.len() <= 6
                    && v.bytes().all(|b| b.is_ascii_digit())
                    && v.parse::<i32>()
                        .is_ok_and(|n| n >= lo && n <= hi && (n - lo) % step.max(1) == 0)
            }
            Kind::Text { len } => v.chars().count() == len && v.chars().all(cw_char),
            Kind::ReadOnly => false,
        }
    }
}

/// The characters the FDM-DUO's CW messages take.
pub fn cw_char(c: char) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit() || " /?.,=-".contains(c) || c.is_ascii_lowercase()
}

/// Which VFO.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vfo {
    A,
    B,
}

pub trait Protocol: Send {
    /// Asked to see whether the radio is there.
    fn ping(&self) -> Frame;
    /// Forward power, reflected power and SWR, read several times a second.
    fn telemetry(&self) -> Vec<Frame>;
    /// The receive S-meter, read with the telemetry while receiving.
    fn meter(&self) -> Vec<Frame> {
        Vec::new()
    }
    /// The dial and mode, read once a second.
    fn dial(&self) -> Vec<Frame>;
    /// Every setting the radio answers a READ for.
    fn settings(&self) -> &'static [Item];
    fn read(&self, item: &Item) -> Frame;
    /// Which reply answers `frame`; `None` where none comes.
    fn reply_key(&self, frame: &Frame) -> Option<String>;
    /// Bytes from the port in, whole answers out. Keeps a partial one.
    fn deframe(&mut self, bytes: &[u8]) -> Vec<Incoming>;
    /// Fold an answer into the status.
    fn apply(&self, s: &mut RigStatus, r: &Reply);
    /// An answer in words, or `None` where it doesn't parse.
    fn describe(&self, r: &Reply) -> Option<String>;
    /// An answer as it came, for the settings table.
    fn raw(&self, r: &Reply) -> String;

    /// Whether `frame` is one this radio's link may ever send: its reads,
    /// its power steps, its TUNE carrier and unkey, and nothing else
    /// (interlock #5).
    fn allowed(&self, frame: &Frame) -> bool;

    /// The transmit power steps, lowest first, and what the setting is
    /// called on the radio.
    fn power_steps(&self) -> &'static [PowerStep];
    fn power_name(&self) -> &'static str;
    fn set_power(&self, code: u8) -> Frame;
    /// The READ for the power setting, asked straight after a change so the
    /// panel shows what the radio took at once, not at the next sweep.
    fn read_power(&self) -> Option<Frame> {
        None
    }
    /// The radio's TUNE carrier, at its power setting.
    fn key_tune(&self) -> Frame;
    fn unkey(&self) -> Frame;

    /// An ordinary transmission of the audio on the radio's transmit input,
    /// which keys the radio's PTT output as its PTT does; `None` where the
    /// radio has no way to take its transmit audio from the computer.
    fn key_audio(&self) -> Option<Frame>;
    /// What is read just before [`Protocol::key_audio`], for
    /// [`Protocol::audio_ready`].
    fn audio_reads(&self) -> Vec<Frame>;
    /// Whether, as last read, an ordinary transmission would send the
    /// computer's audio and nothing else (interlock #7). The reason if not.
    fn audio_ready(&self, s: &RigStatus) -> Result<(), String>;
    /// Put the radio's transmit audio on its USB input.
    fn audio_input(&self) -> Option<Frame>;
    /// The share of full audio, as a power fraction, that Drive 100% sends:
    /// where the radio stops putting out more power for more audio.
    fn audio_ceiling(&self) -> f32;

    /// Every setting the panel shows, in the order it shows them. Empty for
    /// a radio that has no table yet.
    fn controls(&self) -> &'static [Control] {
        &[]
    }
    /// Set a control, given the value the radio last answered with.
    fn set_control(&self, c: &Control, value: &str, last: Option<&str>) -> Result<Frame, String> {
        Ok(Frame::new(format!("{}{};", c.cmd, c.encode(value, last)?), FrameKind::SetSetting))
    }
    fn set_freq(&self, _vfo: Vfo, _hz: u64) -> Option<Frame> {
        None
    }
    /// `mode` as the radio names it (see [`Protocol::modes`]).
    fn set_mode(&self, _mode: &str) -> Option<Frame> {
        None
    }
    /// The modes the radio has, as named in [`crate::RigStatus::mode`].
    fn modes(&self) -> &'static [&'static str] {
        &[]
    }
    fn select_vfo(&self, _vfo: Vfo) -> Option<Frame> {
        None
    }
    fn set_split(&self, _on: bool) -> Option<Frame> {
        None
    }
    /// RIT offset in Hz, positive or negative; 0 clears it.
    fn set_rit(&self, _hz: i32) -> Vec<Frame> {
        Vec::new()
    }
    fn set_rit_on(&self, _on: bool) -> Option<Frame> {
        None
    }
    /// The receive filters the radio has for `mode`, in Hz, narrowest first.
    fn filters(&self, _mode: &str) -> Vec<u32> {
        Vec::new()
    }
    /// The nearest receive filter to `hz` for `mode`.
    fn set_filter(&self, _mode: &str, _hz: u32) -> Option<Frame> {
        None
    }
    /// Recall memory channel `n`.
    fn recall_memory(&self, _n: u16) -> Option<Frame> {
        None
    }
    /// Store the dial in memory channel `n`. `None` where unknown.
    fn store_memory(&self, _n: u16, _hz: u64, _mode: &str) -> Option<Frame> {
        None
    }
    /// Send stored CW message `n` (1 to 10), which transmits.
    fn cw_send(&self, _n: u8) -> Option<Frame> {
        None
    }
    /// Stop a stored CW message.
    fn cw_stop(&self) -> Option<Frame> {
        None
    }
    /// Store `text` as CW message `n`.
    fn cw_store(&self, _n: u8, _text: &str) -> Option<Frame> {
        None
    }
}
