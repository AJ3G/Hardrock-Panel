//! A data link on its own thread.
//!
//! The panel's I/Q thread hands blocks over through a bounded queue that
//! drops (and counts) blocks rather than wait when the decoder falls behind;
//! inside, the channels run in parallel on rayon's pool. Dropping the worker
//! stops it.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded, unbounded};
use sdr::Complex32;

use crate::link::{Decoded, LinkConfig, LinkDecoder};

/// Blocks held before dropping.
const QUEUE: usize = 256;

enum Input {
    Iq(Vec<Complex32>, f64),
    Config(LinkConfig),
}

/// What the worker reports.
#[derive(Debug, Clone)]
pub enum LinkEvent {
    Decoded(Box<Decoded>),
    /// A new configuration could not be used; the old one carries on.
    Refused(String),
}

pub struct LinkWorker {
    tx: Option<Sender<Input>>,
    events: Receiver<LinkEvent>,
    dropped: Arc<AtomicU64>,
    thread: Option<JoinHandle<()>>,
}

impl LinkWorker {
    pub fn spawn(cfg: LinkConfig) -> Result<Self, String> {
        let mut dec = LinkDecoder::new(cfg)?;
        let (tx, input) = bounded(QUEUE);
        let (etx, events) = unbounded();
        let name = format!("link-{}", dec.config().mode.label());
        let thread = std::thread::Builder::new()
            .name(name)
            .spawn(move || {
                for msg in input.iter() {
                    match msg {
                        Input::Iq(iq, utc) => {
                            for d in dec.process(&iq, utc) {
                                if etx.send(LinkEvent::Decoded(Box::new(d))).is_err() {
                                    return;
                                }
                            }
                        }
                        Input::Config(c) => match LinkDecoder::new(c) {
                            Ok(d) => dec = d,
                            Err(e) => {
                                let _ = etx.send(LinkEvent::Refused(e));
                            }
                        },
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(LinkWorker {
            tx: Some(tx),
            events,
            dropped: Arc::new(AtomicU64::new(0)),
            thread: Some(thread),
        })
    }

    /// Interleaved `I, Q, …` whose first sample was heard at `utc` (Unix
    /// seconds, system clock).
    pub fn push_interleaved(&self, iq: &[f32], utc: f64) {
        let mut v = Vec::with_capacity(iq.len() / 2);
        decoders::resample::deinterleave(iq, &mut v);
        self.send(v, utc);
    }

    pub fn push(&self, iq: &[Complex32], utc: f64) {
        self.send(iq.to_vec(), utc);
    }

    fn send(&self, iq: Vec<Complex32>, utc: f64) {
        if let Some(tx) = &self.tx
            && let Err(TrySendError::Full(_)) = tx.try_send(Input::Iq(iq, utc))
        {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Retune, change rate or channels. Blocks queued before it are decoded
    /// with the old configuration.
    pub fn reconfigure(&self, cfg: LinkConfig) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Input::Config(cfg));
        }
    }

    pub fn events(&self) -> &Receiver<LinkEvent> {
        &self.events
    }

    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Drop for LinkWorker {
    fn drop(&mut self) {
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
