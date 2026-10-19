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
    /// Unkeys. Always safe to send.
    Unkey,
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

pub trait Protocol: Send {
    /// Asked to see whether the radio is there.
    fn ping(&self) -> Frame;
    /// Forward power, reflected power and SWR, read several times a second.
    fn telemetry(&self) -> Vec<Frame>;
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
    /// The radio's TUNE carrier, at its power setting.
    fn key_tune(&self) -> Frame;
    fn unkey(&self) -> Frame;
}
