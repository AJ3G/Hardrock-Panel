// Ported from SDRoxide (https://github.com/dividebysandwich/sdroxide),
// crates/sdroxide-rigctld/src/server.rs and lib.rs, GPL-3.0-or-later.

//! The Hamlib **rigctld server**: the panel's radio as Hamlib's "NET rigctl"
//! rig (model 2), by convention on TCP port 4532.
//!
//! WSJT-X, fldigi, JS8Call, N1MM, Log4OM and CQRLOG all reach a radio this
//! way, so none of them needs its own serial cable or a separate `rigctld`.
//! The protocol is strictly request and response, so there is an acceptor
//! thread and one thread per client, and every answer comes from the [`Rig`]
//! at the moment it is asked.

pub mod proto;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

use tracing::{debug, info, warn};

use crate::{PttSource, Rig};
use proto::{Reply, Session};

/// The conventional rigctld port.
pub const DEFAULT_PORT: u16 = 4532;

/// How often the acceptor wakes to take a connection or notice a stop.
const ACCEPT_POLL: Duration = Duration::from_millis(50);
/// Client read timeout, so an idle connection still notices a stop.
const READ_TIMEOUT: Duration = Duration::from_millis(250);
/// Longest command line buffered; real ones are a few dozen bytes.
const MAX_LINE: u64 = 4096;

/// How the server is set up.
#[derive(Debug, Clone, PartialEq)]
pub struct RigctldConfig {
    /// The address to listen on. `127.0.0.1` keeps it to this computer.
    pub host: String,
    /// 0 asks the system for a free port.
    pub port: u16,
    // SAFETY INTERLOCK: off unless the operator turns it on. With it off,
    // `T 1` and `G TUNE` are refused without asking the panel, and
    // `\dump_state` advertises no transmit ranges, so Hamlib itself will not
    // try to key (safety interlock #8).
    /// Whether clients may ask to transmit at all.
    pub allow_tx: bool,
    pub max_clients: usize,
    /// The rig name `\get_info` reports.
    pub rig_name: String,
}

impl Default for RigctldConfig {
    fn default() -> Self {
        RigctldConfig {
            host: "127.0.0.1".into(),
            port: DEFAULT_PORT,
            allow_tx: false,
            max_clients: 4,
            rig_name: "hardrock-panel".into(),
        }
    }
}

struct Shared {
    cfg: RwLock<RigctldConfig>,
    clients: AtomicUsize,
    next_id: AtomicU64,
    stop: AtomicBool,
}

/// The running server. Dropping it stops it, unkeying any client that keyed.
pub struct RigctldServer {
    shared: Arc<Shared>,
    addr: String,
    acceptor: Option<JoinHandle<()>>,
}

impl RigctldServer {
    /// Bind and start serving. The listener is bound before the thread starts,
    /// so a taken port (a real `rigctld` on 4532) comes back here as an error.
    pub fn start(cfg: &RigctldConfig, rig: Arc<dyn Rig>) -> Result<Self, String> {
        let want = crate::bind_addr(&cfg.host, cfg.port);
        let listener = TcpListener::bind(&want).map_err(|e| format!("bind {want}: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| format!("{want}: {e}"))?;
        let addr = listener.local_addr().map(|a| a.to_string()).unwrap_or(want);

        let shared = Arc::new(Shared {
            cfg: RwLock::new(cfg.clone()),
            clients: AtomicUsize::new(0),
            next_id: AtomicU64::new(1),
            stop: AtomicBool::new(false),
        });
        let acceptor = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("rigctld".into())
                .spawn(move || accept_loop(listener, shared, rig))
                .map_err(|e| format!("spawn rigctld thread: {e}"))?
        };
        info!("rigctld server listening on {addr}");
        Ok(RigctldServer { shared, addr, acceptor: Some(acceptor) })
    }

    /// The `host:port` it is bound to.
    pub fn addr(&self) -> &str {
        &self.addr
    }

    /// Clients connected now.
    pub fn clients(&self) -> usize {
        self.shared.clients.load(Ordering::Relaxed)
    }

    /// Change the settings that do not move the socket: transmit permission,
    /// client limit, name. Turning `allow_tx` off does not unkey a client
    /// already transmitting; the panel does that with its own PTT.
    pub fn set_config(&self, cfg: RigctldConfig) {
        if let Ok(mut c) = self.shared.cfg.write() {
            *c = cfg;
        }
    }
}

impl Drop for RigctldServer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(h) = self.acceptor.take() {
            let _ = h.join();
        }
    }
}

fn accept_loop(listener: TcpListener, shared: Arc<Shared>, rig: Arc<dyn Rig>) {
    let mut clients: Vec<JoinHandle<()>> = Vec::new();
    while !shared.stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, peer)) => {
                clients.retain(|h| !h.is_finished());
                let max = shared.cfg.read().map(|c| c.max_clients).unwrap_or(4).max(1);
                if clients.len() >= max {
                    debug!("rigctld: refusing {peer}, {max} clients already connected");
                    continue;
                }
                let shared = Arc::clone(&shared);
                let rig = Arc::clone(&rig);
                let id = shared.next_id.fetch_add(1, Ordering::Relaxed);
                if let Ok(h) =
                    std::thread::Builder::new().name(format!("rigctld-{id}")).spawn(move || {
                        shared.clients.fetch_add(1, Ordering::Relaxed);
                        let keyed = client(stream, &shared, rig.as_ref(), id);
                        if keyed {
                            // SAFETY INTERLOCK: a client that leaves (or a
                            // server that stops) while it has the radio keyed
                            // unkeys it (safety interlock #8).
                            info!(id, "rigctld client left while keyed; unkeying");
                            rig.release_ptt(PttSource::Rigctld { client: id });
                        }
                        shared.clients.fetch_sub(1, Ordering::Relaxed);
                    })
                {
                    clients.push(h);
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(ACCEPT_POLL);
            }
            Err(e) => {
                warn!("rigctld accept: {e}");
                std::thread::sleep(ACCEPT_POLL);
            }
        }
    }
    drop(listener);
    for h in clients {
        let _ = h.join();
    }
}

/// Serve one client until it leaves or the server stops. Returns whether it
/// still had the radio keyed.
fn client(stream: TcpStream, shared: &Shared, rig: &dyn Rig, id: u64) -> bool {
    let mut sess = Session { client: id, ..Session::default() };
    if stream.set_nodelay(true).is_err() {
        debug!("rigctld client: could not disable Nagle");
    }
    // Accepted sockets can inherit the listener's non-blocking mode on some
    // systems; this loop wants blocking reads with a timeout.
    if stream.set_nonblocking(false).is_err()
        || stream.set_read_timeout(Some(READ_TIMEOUT)).is_err()
    {
        warn!("rigctld client: could not set read timeout");
        return false;
    }
    let Ok(mut writer) = stream.try_clone() else { return false };
    let mut reader = BufReader::new(stream);
    let mut line = String::new();

    while !shared.stop.load(Ordering::Relaxed) {
        line.clear();
        match (&mut reader).take(MAX_LINE).read_line(&mut line) {
            Ok(0) => break,
            Ok(_) => {}
            Err(ref e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(e) => {
                debug!("rigctld client read: {e}");
                break;
            }
        }
        let cfg = match shared.cfg.read() {
            Ok(c) => c.clone(),
            Err(_) => break,
        };
        match proto::handle(&line, rig, &cfg, &mut sess) {
            Reply::Close => break,
            Reply::Send(text) if text.is_empty() => {}
            Reply::Send(text) => {
                if writer.write_all(text.as_bytes()).is_err() || writer.flush().is_err() {
                    break;
                }
            }
        }
    }
    sess.keyed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testrig::TestRig;
    use crate::{Mode, RigState};

    fn loopback() -> RigctldConfig {
        RigctldConfig { port: 0, allow_tx: true, ..RigctldConfig::default() }
    }

    fn read_some(sock: &mut TcpStream) -> String {
        let mut buf = [0u8; 4096];
        let n = sock.read(&mut buf).unwrap();
        String::from_utf8_lossy(&buf[..n]).into_owned()
    }

    #[test]
    fn answers_a_real_client_and_a_taken_port_is_an_error() {
        let rig = Arc::new(TestRig::new(RigState { can_tx: true, ..RigState::default() }));
        let srv = RigctldServer::start(&loopback(), rig.clone()).unwrap();
        assert!(srv.addr().starts_with("127.0.0.1:"));

        let port: u16 = srv.addr().rsplit(':').next().unwrap().parse().unwrap();
        let taken = RigctldConfig { port, ..loopback() };
        let err = RigctldServer::start(&taken, rig.clone()).err().expect("second bind fails");
        assert!(err.contains("bind"), "{err}");

        let mut sock = TcpStream::connect(srv.addr()).unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        sock.write_all(b"f\n").unwrap();
        assert_eq!(read_some(&mut sock), "14074000\n");
        sock.write_all(b"M LSB 0\n").unwrap();
        assert_eq!(read_some(&mut sock), "RPRT 0\n");
        assert_eq!(rig.state().mode, Mode::Lsb);

        sock.write_all(b"\\dump_state\n").unwrap();
        let mut got = String::new();
        while !got.contains("done\n") {
            got.push_str(&read_some(&mut sock));
        }
        assert!(got.starts_with("1\n"));
    }

    /// A program that keys the radio and then dies must not leave it keyed.
    #[test]
    fn a_client_that_disconnects_while_keyed_unkeys_the_radio() {
        let rig = Arc::new(TestRig::new(RigState { can_tx: true, ..RigState::default() }));
        rig.allow_ptt.store(true, Ordering::SeqCst);
        let srv = RigctldServer::start(&loopback(), rig.clone()).unwrap();
        let mut sock = TcpStream::connect(srv.addr()).unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        sock.write_all(b"T 1\n").unwrap();
        assert_eq!(read_some(&mut sock), "RPRT 0\n");
        assert!(rig.state().ptt);
        drop(sock);
        for _ in 0..200 {
            if !rig.state().ptt {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!rig.state().ptt, "still keyed after the client left");
        assert!(rig.log().iter().any(|l| l.starts_with("unptt Rigctld")));
    }

    #[test]
    fn quit_closes_the_connection() {
        let rig = Arc::new(TestRig::new(RigState::default()));
        let srv = RigctldServer::start(&loopback(), rig).unwrap();
        let mut sock = TcpStream::connect(srv.addr()).unwrap();
        sock.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        sock.write_all(b"q\n").unwrap();
        let mut buf = [0u8; 16];
        assert_eq!(sock.read(&mut buf).unwrap(), 0);
    }
}
