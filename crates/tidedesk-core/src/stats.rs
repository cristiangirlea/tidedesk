//! Lightweight throughput meter and network path lines for the `--stats`
//! output.

use std::net::SocketAddr;
use std::time::{Duration, Instant};

pub struct Meter {
    label: &'static str,
    window_start: Instant,
    frames: u32,
    bytes: u64,
    work: Duration,
}

impl Meter {
    const WINDOW: Duration = Duration::from_secs(2);

    pub fn new(label: &'static str) -> Self {
        Self {
            label,
            window_start: Instant::now(),
            frames: 0,
            bytes: 0,
            work: Duration::ZERO,
        }
    }

    /// Records one frame of `bytes` that took `work` to process, and returns a
    /// summary line whenever a reporting window has elapsed.
    pub fn record(&mut self, bytes: usize, work: Duration) -> Option<String> {
        self.frames += 1;
        self.bytes += bytes as u64;
        self.work += work;
        let elapsed = self.window_start.elapsed();
        if elapsed < Self::WINDOW {
            return None;
        }
        let secs = elapsed.as_secs_f64();
        let line = format!(
            "{}: {:.1} fps, {:.2} Mbit/s, {:.1} ms/frame",
            self.label,
            self.frames as f64 / secs,
            self.bytes as f64 * 8.0 / secs / 1e6,
            self.work.as_secs_f64() * 1000.0 / self.frames as f64,
        );
        *self = Self::new(self.label);
        Some(line)
    }
}

/// How long nothing has to come from a peer for it to count as silent. A
/// connection with nothing else to send sends a keep-alive every second,
/// which the peer answers, so this is three of them in a row.
pub const SILENT_AFTER: Duration = Duration::from_secs(3);

/// How often a connection is looked at for whether its peer answers.
const LOOK: Duration = Duration::from_millis(250);

/// Looks between two lines of [`log_path`].
const LOOKS_A_LINE: u32 = (Meter::WINDOW.as_millis() / LOOK.as_millis()) as u32;

/// For how long nothing has come from a connection's peer, from the number
/// of datagrams received from it.
pub struct Silence {
    received: u64,
    last: Instant,
}

impl Silence {
    pub fn new(now: Instant) -> Self {
        Self {
            received: 0,
            last: now,
        }
    }

    /// Takes note of the datagrams `received` so far: for how long none has
    /// come, once that is [`SILENT_AFTER`] or longer.
    pub fn observe(&mut self, received: u64, now: Instant) -> Option<Duration> {
        if received != self.received {
            self.received = received;
            self.last = now;
        }
        let silent = now.saturating_duration_since(self.last);
        (silent >= SILENT_AFTER).then_some(silent)
    }
}

/// One `--stats` line about a connection's network path. A peer that has
/// been `silent` has no round-trip time: the last one measured says nothing
/// about it.
pub fn path_line(
    label: &str,
    remote: SocketAddr,
    rtt: Duration,
    lost_packets: u64,
    silent: Option<Duration>,
) -> String {
    let answer = match silent {
        Some(silent) => format!("no answer for {:.1} s", silent.as_secs_f64()),
        None => format!("rtt {:.1} ms", rtt.as_secs_f64() * 1000.0),
    };
    format!("{label}: {remote}, {answer}, {lost_packets} packets lost")
}

/// Logs [`path_line`] every two seconds until the connection closes.
pub async fn log_path(conn: quinn::Connection, label: &'static str) {
    let mut silence = Silence::new(Instant::now());
    let mut every = tokio::time::interval_at(tokio::time::Instant::now() + LOOK, LOOK);
    let mut looks = 0;
    loop {
        tokio::select! {
            _ = conn.closed() => return,
            _ = every.tick() => {
                let stats = conn.stats();
                let silent = silence.observe(stats.udp_rx.datagrams, Instant::now());
                looks += 1;
                if looks % LOOKS_A_LINE != 0 {
                    continue;
                }
                let (remote, path) = (conn.remote_address(), stats.path);
                let line = path_line(label, remote, path.rtt, path.lost_packets, silent);
                tracing::info!("{line}");
            }
        }
    }
}

/// Tells when the peer of `conn` stops answering: for how long it has been
/// silent, every second while that lasts, and `None` once it answers again.
/// Ends with the connection, which gives a silent peer up much later: a
/// network that fails for a moment does not end a session.
pub async fn watch_silence(conn: quinn::Connection, mut tell: impl FnMut(Option<Duration>)) {
    let mut silence = Silence::new(Instant::now());
    let mut every = tokio::time::interval(LOOK);
    let mut told: Option<Duration> = None;
    loop {
        tokio::select! {
            _ = conn.closed() => return,
            _ = every.tick() => {
                let silent = silence.observe(conn.stats().udp_rx.datagrams, Instant::now());
                let seconds = |silent: Option<Duration>| silent.map(|s| s.as_secs());
                if seconds(silent) != seconds(told) {
                    told = silent;
                    tell(silent);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;
    use crate::identity::test_identity;
    use crate::nat::SharedSocket;
    use crate::net;

    #[test]
    fn path_line_names_the_peer_rtt_and_losses() {
        let line = path_line(
            "viewer path (direct)",
            "203.0.113.5:40000".parse().unwrap(),
            Duration::from_micros(23_400),
            3,
            None,
        );
        assert_eq!(
            line,
            "viewer path (direct): 203.0.113.5:40000, rtt 23.4 ms, 3 packets lost"
        );
    }

    /// The last round-trip time says nothing about a peer that is silent.
    #[test]
    fn path_line_says_when_the_peer_does_not_answer() {
        let line = path_line(
            "path to host",
            "203.0.113.5:40000".parse().unwrap(),
            Duration::from_micros(23_400),
            3,
            Some(Duration::from_millis(4200)),
        );
        assert_eq!(
            line,
            "path to host: 203.0.113.5:40000, no answer for 4.2 s, 3 packets lost"
        );
    }

    #[test]
    fn a_peer_is_silent_after_three_seconds_without_a_datagram() {
        let start = Instant::now();
        let at = |ms| start + Duration::from_millis(ms);
        let mut silence = Silence::new(at(0));
        assert_eq!(silence.observe(10, at(500)), None);
        assert_eq!(silence.observe(10, at(3400)), None);
        assert_eq!(silence.observe(10, at(3500)), Some(Duration::from_secs(3)));
        assert_eq!(silence.observe(10, at(9500)), Some(Duration::from_secs(9)));
        // It answers again.
        assert_eq!(silence.observe(11, at(9600)), None);
        assert_eq!(silence.observe(11, at(12_500)), None);
        assert_eq!(
            silence.observe(11, at(12_600)),
            Some(Duration::from_secs(3))
        );
    }

    /// Passes datagrams between a viewer and `host` while it is open: a
    /// network that can fail, or a host that can stop without a word.
    async fn valve(host: SocketAddr, open: Arc<AtomicBool>) -> SocketAddr {
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        tokio::spawn(async move {
            let mut viewer = None;
            let mut datagram = [0; 2048];
            loop {
                let Ok((len, from)) = socket.recv_from(&mut datagram).await else {
                    continue;
                };
                let to = if from == host {
                    viewer
                } else {
                    viewer = Some(from);
                    Some(host)
                };
                if let Some(to) = to
                    && open.load(Ordering::Relaxed)
                {
                    let _ = socket.send_to(&datagram[..len], to).await;
                }
            }
        });
        addr
    }

    #[tokio::test]
    async fn a_silent_peer_is_noticed_long_before_the_connection_ends() {
        let identity = test_identity("stats-silence");
        let loopback: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let (host_socket, _) = SharedSocket::bind(loopback).unwrap();
        let (viewer_socket, _) = SharedSocket::bind(loopback).unwrap();
        let host_addr = host_socket.local_addr().unwrap();
        let host = net::server_endpoint_on(host_socket, &identity).unwrap();
        let viewer = net::client_endpoint_on(viewer_socket).unwrap();
        tokio::spawn(async move {
            let incoming = net::accept_validated(&host).await.unwrap();
            incoming.await.unwrap().closed().await;
        });
        let open = Arc::new(AtomicBool::new(true));
        let through = valve(host_addr, open.clone()).await;
        let conn = viewer
            .connect(through, "tidedesk-host")
            .unwrap()
            .await
            .unwrap();

        let (told, mut tells) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(watch_silence(conn.clone(), move |silent| {
            let _ = told.send(silent);
        }));
        let within = |seconds| Duration::from_secs(seconds);

        // Nothing to send either way: the connection's own keep-alives are
        // answered often enough.
        let early = tokio::time::timeout(within(4), tells.recv()).await;
        assert!(early.is_err(), "{early:?} from a peer that answers");

        open.store(false, Ordering::Relaxed);
        let began = Instant::now();
        let silent = tokio::time::timeout(within(6), tells.recv()).await;
        let silent = silent.expect("noticed within seconds").unwrap();
        assert!(silent.is_some_and(|s| s >= SILENT_AFTER), "{silent:?}");
        assert!(began.elapsed() < within(5), "{:?}", began.elapsed());
        // For how long is told as it goes on.
        let longer = tokio::time::timeout(within(3), tells.recv()).await;
        assert!(longer.unwrap().unwrap() > silent);
        assert!(conn.close_reason().is_none());

        open.store(true, Ordering::Relaxed);
        let back = tokio::time::timeout(within(10), async {
            while tells.recv().await.unwrap().is_some() {}
        });
        back.await.expect("answers again");
        conn.close(0u32.into(), b"done");
    }
}
