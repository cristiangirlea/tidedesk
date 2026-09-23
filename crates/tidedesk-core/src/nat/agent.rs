//! The side-channel actor: owns the shared socket's tap and runs STUN and
//! hole punching on the port QUIC uses, so the address it learns is the one
//! peers must reach and the paths it opens are the ones QUIC will use.

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::runtime::Handle;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use super::punch::{Exchange, Punched, SessionId, State};
use super::signal::{Credentials, Event, Registration, RendezvousStatus, resolve_service};
use super::socket::{RawDatagram, SharedSocket};
use super::stun::{Discovery, NatKind, PublicEndpoint, resolve_servers};

/// Side-channel datagrams buffered per listener before the oldest are dropped.
const FAN_OUT_CAPACITY: usize = 64;

/// How long a host punches towards a viewer the rendezvous service introduced.
const INCOMING_WINDOW: Duration = Duration::from_secs(30);

/// Introductions a host acts on per minute, whatever the service sends.
const MAX_INCOMING_PER_MINUTE: usize = 10;

/// How long to wait before looking a rendezvous service's name up again.
const RESOLVE_RETRY: Duration = Duration::from_secs(30);

type Exchanges = Arc<Mutex<HashMap<SocketAddr, JoinHandle<()>>>>;

/// What is known about this computer's internet address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublicStatus {
    /// Discovery is turned off.
    Disabled,
    /// The first round of asking STUN servers has not finished yet.
    Discovering,
    Ready(PublicEndpoint),
    /// No server answered; the reason names them.
    Unavailable(String),
}

impl fmt::Display for PublicStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disabled => f.write_str("not looked up (turned off)"),
            Self::Discovering => f.write_str("being looked up"),
            Self::Ready(p) => write!(f, "{} via {} (NAT: {})", p.addr, p.via, p.nat),
            Self::Unavailable(reason) => write!(f, "unavailable ({reason})"),
        }
    }
}

/// Why [`Agent::punch`] did not open a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PunchError {
    /// The window passed without an answer from the peer.
    NoReply,
    /// Punching towards this peer was stopped or started again.
    Stopped,
}

impl fmt::Display for PunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoReply => "no reply from the other computer",
            Self::Stopped => "stopped before the other computer replied",
        })
    }
}

impl std::error::Error for PunchError {}

/// Everything the agent reports. Punch results come from [`Agent::punch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStatus {
    pub public: PublicStatus,
    pub rendezvous: RendezvousStatus,
}

pub struct Agent {
    link: Link,
    dispatcher: JoinHandle<()>,
    refresh: Mutex<Option<JoinHandle<()>>>,
    rendezvous: Mutex<Option<JoinHandle<()>>>,
}

/// What a task working for the agent needs; cheap to clone into one.
#[derive(Clone)]
struct Link {
    socket: Arc<SharedSocket>,
    /// Every side-channel datagram, for whichever task is waiting for one.
    datagrams: broadcast::Sender<RawDatagram>,
    status: watch::Sender<AgentStatus>,
    runtime: Handle,
    /// Punch exchanges by the peer address they were started with.
    exchanges: Exchanges,
}

impl Agent {
    /// Starts the agent on a socket and its tap. Must be called inside a
    /// tokio runtime; the agent keeps using that runtime, so its methods can
    /// be called from any thread.
    pub fn spawn(
        socket: Arc<SharedSocket>,
        mut tap: mpsc::Receiver<RawDatagram>,
    ) -> io::Result<Arc<Self>> {
        let runtime = Handle::try_current()
            .map_err(|_| io::Error::other("no tokio runtime to run the NAT agent"))?;
        let (datagrams, _) = broadcast::channel(FAN_OUT_CAPACITY);
        let fan_out = datagrams.clone();
        let dispatcher = runtime.spawn(async move {
            while let Some(datagram) = tap.recv().await {
                // An error only means nobody is waiting for side-channel traffic.
                let _ = fan_out.send(datagram);
            }
        });
        let status = watch::Sender::new(AgentStatus {
            public: PublicStatus::Disabled,
            rendezvous: RendezvousStatus::Off,
        });
        Ok(Arc::new(Self {
            link: Link {
                socket,
                datagrams,
                status,
                runtime,
                exchanges: Arc::default(),
            },
            dispatcher,
            refresh: Mutex::new(None),
            rendezvous: Mutex::new(None),
        }))
    }

    /// Watches the agent's status; see [`Agent::public`] for a one-off read.
    pub fn status(&self) -> watch::Receiver<AgentStatus> {
        self.link.status.subscribe()
    }

    pub fn public(&self) -> PublicStatus {
        self.link.status.borrow().public.clone()
    }

    /// Asks the given servers once and publishes the outcome.
    pub async fn discover(
        &self,
        servers: Vec<(String, SocketAddr)>,
    ) -> Result<PublicEndpoint, String> {
        self.link.begin();
        self.link.discover(servers).await
    }

    /// Asks the named servers now and then every `every`, replacing any
    /// earlier refresh. Asking again notices a changed address and keeps the
    /// router's mapping for this port alive.
    pub fn start_refresh(&self, servers: Vec<String>, every: Duration) {
        // Right away, not when the task first runs: whoever reads the status
        // next must not see "turned off".
        self.link.begin();
        let link = self.link.clone();
        let task = self.link.runtime.spawn(async move {
            let mut resolved = Vec::new();
            loop {
                // Looked up again after a failed round: a computer that starts
                // offline cannot resolve anything until its network is up.
                if resolved.is_empty() {
                    resolved = resolve_servers(&servers).await;
                }
                if link.discover(resolved.clone()).await.is_err() {
                    resolved.clear();
                }
                tokio::time::sleep(every).await;
            }
        });
        if let Some(earlier) = self.refresh.lock().unwrap().replace(task) {
            earlier.abort();
        }
    }

    /// Punches towards `peer` until it answers or `window` passes, and
    /// resolves once the path is open. Keepalives then continue for a while
    /// (see [`super::punch`]) unless [`Agent::stop_punching`] ends them.
    /// Punching again towards the same `peer` replaces the earlier exchange.
    ///
    /// The side that starts a connection passes a new session ID; the other
    /// side may pass `None` to take the first session it sees.
    pub async fn punch(
        &self,
        peer: SocketAddr,
        session: Option<SessionId>,
        window: Duration,
    ) -> Result<Punched, PunchError> {
        let (opened, result) = oneshot::channel();
        self.link
            .start_exchange(peer, session, window, Some(opened));
        // A dropped sender means the exchange was aborted.
        result.await.unwrap_or(Err(PunchError::Stopped))
    }

    /// Stops punching and keepalives towards `peer` (as passed to
    /// [`Agent::punch`]).
    pub fn stop_punching(&self, peer: SocketAddr) {
        if let Some(exchange) = self.link.exchanges.lock().unwrap().remove(&peer) {
            exchange.abort();
        }
    }

    /// Registers this host with a rendezvous service (`host[:port]`) and
    /// keeps the registration alive; viewers the service introduces are
    /// punched towards automatically. Replaces an earlier registration.
    pub fn start_rendezvous(&self, service: String, credentials: Arc<Credentials>) {
        self.link.status.send_if_modified(|s| {
            let changed = s.rendezvous != RendezvousStatus::Connecting;
            s.rendezvous = RendezvousStatus::Connecting;
            changed
        });
        let link = self.link.clone();
        let task = self
            .link
            .runtime
            .spawn(async move { link.run_rendezvous(service, credentials).await });
        if let Some(earlier) = self.rendezvous.lock().unwrap().replace(task) {
            earlier.abort();
        }
    }

    /// Ends the registration; the service forgets this host within a minute.
    pub fn stop_rendezvous(&self) {
        if let Some(task) = self.rendezvous.lock().unwrap().take() {
            task.abort();
        }
        // As with discovery, a late update from the task cannot undo this.
        self.link.status.send_if_modified(|s| {
            let changed = s.rendezvous != RendezvousStatus::Off;
            s.rendezvous = RendezvousStatus::Off;
            changed
        });
    }

    pub fn rendezvous(&self) -> RendezvousStatus {
        self.link.status.borrow().rendezvous.clone()
    }

    /// Stops asking and marks discovery as turned off.
    pub fn stop_refresh(&self) {
        if let Some(task) = self.refresh.lock().unwrap().take() {
            task.abort();
        }
        // A task already past its last await may still publish; see `publish`.
        self.link.status.send_if_modified(|s| {
            let changed = s.public != PublicStatus::Disabled;
            s.public = PublicStatus::Disabled;
            changed
        });
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        self.dispatcher.abort();
        for task in [self.refresh.get_mut(), self.rendezvous.get_mut()] {
            if let Some(task) = task.unwrap().take() {
                task.abort();
            }
        }
        for exchange in self.link.exchanges.lock().unwrap().values() {
            exchange.abort();
        }
    }
}

impl Link {
    /// One round of asking `servers`, publishing the outcome.
    async fn discover(&self, servers: Vec<(String, SocketAddr)>) -> Result<PublicEndpoint, String> {
        let outcome = self.ask(servers).await;
        self.publish(outcome.clone());
        outcome
    }

    async fn ask(&self, servers: Vec<(String, SocketAddr)>) -> Result<PublicEndpoint, String> {
        // Subscribe before sending, so no early reply is missed.
        let mut datagrams = self.datagrams.subscribe();
        let mut discovery = Discovery::new(servers, Instant::now());
        let mut send_error = None;
        loop {
            let now = Instant::now();
            for (to, request) in discovery.poll(now) {
                if let Err(e) = self.socket.send_raw(to, &request).await {
                    send_error = Some(format!("cannot send to {to}: {e}"));
                }
            }
            if let Some(outcome) = discovery.outcome(now) {
                // A socket that cannot reach the servers must not pass for
                // servers that do not answer.
                return outcome.map_err(|reason| match send_error {
                    Some(error) => format!("{reason}; {error}"),
                    None => reason,
                });
            }
            let wake = discovery.next_deadline().unwrap_or(now);
            tokio::select! {
                _ = tokio::time::sleep_until(wake.into()) => {}
                received = datagrams.recv() => {
                    // Lagged: lost replies are retransmitted. Closed cannot
                    // happen while `self` holds a sender.
                    if let Ok(datagram) = received {
                        discovery.on_datagram(&datagram.data, Instant::now());
                    }
                }
            }
        }
    }

    /// Starts a punch exchange, replacing one towards the same peer. With
    /// `opened`, the exchange reports there and stops if nobody waits for it
    /// any more; without, it runs on its own (a host answering a viewer the
    /// rendezvous service introduced).
    fn start_exchange(
        &self,
        peer: SocketAddr,
        session: Option<SessionId>,
        window: Duration,
        opened: Option<oneshot::Sender<Result<Punched, PunchError>>>,
    ) {
        let mut exchanges = self.exchanges.lock().unwrap();
        exchanges.retain(|_, task| !task.is_finished());
        // The earlier exchange goes first, so the two never answer together.
        if let Some(earlier) = exchanges.remove(&peer) {
            earlier.abort();
        }
        let link = self.clone();
        let exchange = self.runtime.spawn(async move {
            link.run_exchange(peer, session, window, opened).await;
        });
        exchanges.insert(peer, exchange);
    }

    /// Runs one punch exchange, reporting to `opened` once it opens or
    /// expires; keepalives then go on until the exchange finishes. Stops
    /// early if the caller stops waiting before the path opened.
    async fn run_exchange(
        &self,
        peer: SocketAddr,
        session: Option<SessionId>,
        window: Duration,
        opened: Option<oneshot::Sender<Result<Punched, PunchError>>>,
    ) {
        let mut datagrams = self.datagrams.subscribe();
        let mut exchange = Exchange::new(peer, session, window, Instant::now());
        let mut waiting = opened;
        loop {
            if let Some((to, punch)) = exchange.poll(Instant::now()) {
                self.send(to, &punch).await;
            }
            match exchange.state() {
                State::Open(path) => {
                    if let Some(caller) = waiting.take() {
                        let _ = caller.send(Ok(path.clone()));
                    }
                }
                State::Expired => {
                    if let Some(caller) = waiting.take() {
                        let _ = caller.send(Err(PunchError::NoReply));
                    }
                    return;
                }
                State::Punching => {}
            }
            let Some(wake) = exchange.next_deadline() else {
                return; // keepalives are over
            };
            tokio::select! {
                _ = tokio::time::sleep_until(wake.into()) => {}
                () = caller_gone(&mut waiting) => return,
                received = datagrams.recv() => {
                    if let Ok(datagram) = received
                        && let Some((to, ack)) =
                            exchange.on_datagram(datagram.from, &datagram.data, Instant::now())
                    {
                        self.send(to, &ack).await;
                    }
                }
            }
        }
    }

    async fn send(&self, to: SocketAddr, datagram: &[u8]) {
        if let Err(e) = self.socket.send_raw(to, datagram).await {
            tracing::debug!("cannot send to {to}: {e}");
        }
    }

    /// Keeps a registration with a rendezvous service and punches towards
    /// the viewers it introduces.
    async fn run_rendezvous(&self, service: String, credentials: Arc<Credentials>) {
        let mut main = loop {
            if let Some(main) = resolve_service(&service).await {
                break main;
            }
            let reason = format!("cannot find the rendezvous service {service}");
            self.set_rendezvous(RendezvousStatus::Unreachable(reason));
            tokio::time::sleep(RESOLVE_RETRY).await;
        };
        let mut resolved_at = Instant::now();
        let mut datagrams = self.datagrams.subscribe();
        let mut registration =
            Registration::new(service.clone(), main, credentials.clone(), Instant::now());
        let mut introductions = IntroductionLimit::default();
        loop {
            // A service that stopped answering may have moved: look it up again.
            let now = Instant::now();
            if matches!(registration.status(), RendezvousStatus::Unreachable(_))
                && now.saturating_duration_since(resolved_at) >= RESOLVE_RETRY
            {
                resolved_at = now;
                if let Some(moved) = resolve_service(&service).await
                    && moved != main
                {
                    main = moved;
                    registration =
                        Registration::new(service.clone(), main, credentials.clone(), now);
                }
            }
            for (to, datagram) in registration.poll(Instant::now()) {
                self.send(to, &datagram).await;
            }
            self.set_rendezvous(registration.status().clone());
            let wake = registration.next_deadline();
            tokio::select! {
                _ = tokio::time::sleep_until(wake.into()) => {}
                received = datagrams.recv() => {
                    let Ok(datagram) = received else { continue };
                    let now = Instant::now();
                    let event = registration.on_datagram(datagram.from, &datagram.data, now);
                    if let Some(Event::Incoming { session, peer }) = event {
                        if introductions.allow(now) {
                            self.start_exchange(peer, Some(session), INCOMING_WINDOW, None);
                        } else {
                            tracing::debug!("too many introductions; ignoring one");
                        }
                    }
                }
            }
        }
    }

    /// Shows the registration's state, unless it was turned off meanwhile.
    fn set_rendezvous(&self, status: RendezvousStatus) {
        let mut shown = None;
        self.status.send_if_modified(|s| {
            if s.rendezvous == RendezvousStatus::Off || s.rendezvous == status {
                return false;
            }
            shown = Some(status.to_string());
            s.rendezvous = status;
            true
        });
        if let Some(shown) = shown {
            tracing::info!("rendezvous: {shown}");
        }
    }

    /// Marks the first round as under way; later rounds keep showing the
    /// last result until they have a new one.
    fn begin(&self) {
        self.status.send_if_modified(|s| {
            let first_round = s.public == PublicStatus::Disabled;
            if first_round {
                s.public = PublicStatus::Discovering;
            }
            first_round
        });
    }

    /// Shows a round's outcome, unless discovery was turned off meanwhile.
    fn publish(&self, outcome: Result<PublicEndpoint, String>) {
        let mut shown = None;
        self.status.send_if_modified(|s| {
            let public = match outcome {
                _ if s.public == PublicStatus::Disabled => return false,
                Ok(public) => PublicStatus::Ready(merge(&s.public, public)),
                Err(reason) => PublicStatus::Unavailable(reason),
            };
            if s.public == public {
                return false;
            }
            shown = Some(public.to_string());
            s.public = public;
            true
        });
        // Logged outside the lock: a blocked console must not stall readers.
        if let Some(shown) = shown {
            tracing::info!("internet address: {shown}");
        }
    }
}

/// How many introduced viewers a host punches towards: whatever a service
/// sends, at most [`MAX_INCOMING_PER_MINUTE`] a minute.
#[derive(Default)]
struct IntroductionLimit {
    recent: VecDeque<Instant>,
}

impl IntroductionLimit {
    fn allow(&mut self, now: Instant) -> bool {
        while self
            .recent
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= Duration::from_secs(60))
        {
            self.recent.pop_front();
        }
        if self.recent.len() >= MAX_INCOMING_PER_MINUTE {
            return false;
        }
        self.recent.push_back(now);
        true
    }
}

/// Resolves when the caller stops waiting for a path that has not opened;
/// never once it has its answer.
async fn caller_gone(waiting: &mut Option<oneshot::Sender<Result<Punched, PunchError>>>) {
    match waiting {
        Some(caller) => caller.closed().await,
        None => std::future::pending().await,
    }
}

/// A round that saw fewer servers keeps the classification of an earlier
/// round for the same address, so one lost reply does not hide what is known.
fn merge(previous: &PublicStatus, mut new: PublicEndpoint) -> PublicEndpoint {
    if let PublicStatus::Ready(known) = previous
        && new.nat == NatKind::Unknown
        && known.addr == new.addr
    {
        new.nat = known.nat;
    }
    new
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU16, Ordering};

    use super::*;
    use crate::identity::test_identity;
    use crate::nat::punch::{KEEPALIVE_INTERVAL, new_session};
    use crate::nat::stun::{self, TransactionId};
    use crate::net;

    /// An agent on a loopback socket. The returned endpoint must stay alive:
    /// quinn is what reads the socket, so without it nothing reaches the tap.
    fn agent_on_loopback() -> (Arc<Agent>, quinn::Endpoint, SocketAddr) {
        let (socket, tap) = SharedSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let local = socket.local_addr().unwrap();
        let endpoint = net::client_endpoint_on(socket.clone()).unwrap();
        (Agent::spawn(socket, tap).unwrap(), endpoint, local)
    }

    /// A STUN server on loopback that answers each Binding Request with the
    /// sender's address, its port moved up by `shift` (a stand-in for a
    /// router's mapping).
    async fn fake_stun_server(shift: Arc<AtomicU16>) -> SocketAddr {
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = socket.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 1500];
            while let Ok((len, from)) = socket.recv_from(&mut buf).await {
                if !stun::is_stun(&buf[..len]) {
                    continue;
                }
                let id: TransactionId = buf[8..20].try_into().unwrap();
                let port = from.port().wrapping_add(shift.load(Ordering::SeqCst));
                let reply = stun::encode_binding_response(&id, SocketAddr::new(from.ip(), port));
                let _ = socket.send_to(&reply, from).await;
            }
        });
        addr
    }

    fn fake(name: &str, addr: SocketAddr) -> (String, SocketAddr) {
        (name.to_string(), addr)
    }

    #[tokio::test]
    async fn discover_returns_reflexive_address_from_fake_server() {
        let (agent, _endpoint, local) = agent_on_loopback();
        assert_eq!(agent.public(), PublicStatus::Disabled);
        let server = fake_stun_server(Arc::default()).await;

        let public = agent.discover(vec![fake("fake", server)]).await.unwrap();
        let expected = PublicEndpoint {
            addr: local,
            nat: NatKind::Unknown,
            via: "fake".into(),
        };
        assert_eq!(public, expected);
        assert_eq!(agent.public(), PublicStatus::Ready(expected));
    }

    #[tokio::test]
    async fn discover_reports_symmetric_when_second_fake_server_sees_another_port() {
        let (agent, _endpoint, local) = agent_on_loopback();
        let same = fake_stun_server(Arc::default()).await;
        let other = fake_stun_server(Arc::new(AtomicU16::new(1))).await;

        let public = agent
            .discover(vec![fake("same", same), fake("other", other)])
            .await
            .unwrap();
        assert_eq!(public.nat, NatKind::Symmetric);
        assert_eq!(public.addr.ip(), local.ip());
    }

    #[tokio::test]
    async fn no_servers_make_the_address_unavailable() {
        let (agent, _endpoint, _) = agent_on_loopback();
        let err = agent.discover(Vec::new()).await.unwrap_err();
        assert_eq!(agent.public(), PublicStatus::Unavailable(err));
    }

    #[tokio::test]
    async fn a_result_arriving_after_stop_does_not_undo_it() {
        let (agent, _endpoint, local) = agent_on_loopback();
        agent.start_refresh(Vec::new(), Duration::from_secs(60));
        agent.stop_refresh();
        // What a refresh task already past its last await could still do.
        agent.link.publish(Ok(PublicEndpoint {
            addr: local,
            nat: NatKind::Unknown,
            via: "late".into(),
        }));
        assert_eq!(agent.public(), PublicStatus::Disabled);
    }

    #[tokio::test]
    async fn refresh_updates_status_when_mapping_changes() {
        let (agent, _endpoint, local) = agent_on_loopback();
        let shift = Arc::new(AtomicU16::new(0));
        let server = fake_stun_server(shift.clone()).await;
        let mut status = agent.status();

        agent.start_refresh(vec![server.to_string()], Duration::from_millis(50));
        assert_eq!(agent.public(), PublicStatus::Discovering);
        let ready_at = |addr: SocketAddr| move |s: &AgentStatus| matches!(&s.public, PublicStatus::Ready(p) if p.addr == addr);
        tokio::time::timeout(Duration::from_secs(5), status.wait_for(ready_at(local)))
            .await
            .expect("first round never finished")
            .unwrap();

        shift.store(7, Ordering::SeqCst);
        let moved = SocketAddr::new(local.ip(), local.port() + 7);
        tokio::time::timeout(Duration::from_secs(5), status.wait_for(ready_at(moved)))
            .await
            .expect("refresh never saw the new mapping")
            .unwrap();

        agent.stop_refresh();
        assert_eq!(agent.public(), PublicStatus::Disabled);
    }

    #[tokio::test]
    async fn quic_handshake_succeeds_through_a_punched_path() {
        let identity = test_identity("nat-agent");
        let loopback: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let (host_socket, host_tap) = SharedSocket::bind(loopback).unwrap();
        let (viewer_socket, viewer_tap) = SharedSocket::bind(loopback).unwrap();
        let host_addr = host_socket.local_addr().unwrap();
        let viewer_addr = viewer_socket.local_addr().unwrap();
        let host_endpoint = net::server_endpoint_on(host_socket.clone(), &identity).unwrap();
        let viewer_endpoint = net::client_endpoint_on(viewer_socket.clone()).unwrap();
        let host = Agent::spawn(host_socket, host_tap).unwrap();
        let viewer = Agent::spawn(viewer_socket, viewer_tap).unwrap();

        // Each side has typed the other's address; only the viewer has a session.
        let window = Duration::from_secs(10);
        let (host_side, viewer_side) = tokio::join!(
            host.punch(viewer_addr, None, window),
            viewer.punch(host_addr, Some(new_session()), window),
        );
        let expected = Punched {
            peer: host_addr,
            observed_self: Some(viewer_addr),
        };
        assert_eq!(viewer_side.unwrap(), expected);
        assert_eq!(host_side.unwrap().peer, viewer_addr);

        let server = tokio::spawn(async move {
            let conn = host_endpoint.accept().await.unwrap().await.unwrap();
            let (mut send, mut recv) = conn.accept_bi().await.unwrap();
            let mut buf = [0u8; 1024];
            while let Some(n) = recv.read(&mut buf).await.unwrap() {
                send.write_all(&buf[..n]).await.unwrap();
            }
            send.finish().unwrap();
            conn.closed().await;
        });
        let conn = viewer_endpoint
            .connect(host_addr, "tidedesk-host")
            .unwrap()
            .await
            .unwrap();
        assert_eq!(net::peer_fingerprint(&conn), Some(identity.fingerprint()));

        // Talk for longer than a keepalive interval, so keepalive punches and
        // QUIC share both ports meanwhile.
        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        let until = Instant::now() + KEEPALIVE_INTERVAL + Duration::from_millis(500);
        let mut round = 0u32;
        while Instant::now() < until {
            send.write_all(&round.to_be_bytes()).await.unwrap();
            let mut back = [0u8; 4];
            recv.read_exact(&mut back).await.unwrap();
            assert_eq!(u32::from_be_bytes(back), round);
            round += 1;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(conn.close_reason().is_none(), "{:?}", conn.close_reason());
        send.finish().unwrap();
        assert_eq!(recv.read_to_end(16).await.unwrap(), b"");
        conn.close(0u32.into(), b"done");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn expect_then_punch_from_another_port_of_same_ip_still_opens() {
        let (host, _host_endpoint, host_addr) = agent_on_loopback();
        let (viewer, _viewer_endpoint, viewer_addr) = agent_on_loopback();
        // The host was given a port the viewer's router does not use towards
        // it. A bound, silent socket, so no other test's exchange sees the
        // host's punches on loopback.
        let decoy = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let typed = decoy.local_addr().unwrap();

        let window = Duration::from_secs(10);
        let (host_side, viewer_side) = tokio::join!(
            host.punch(typed, None, window),
            viewer.punch(host_addr, Some(new_session()), window),
        );
        assert_eq!(viewer_side.unwrap().peer, host_addr);
        let host_side = host_side.unwrap();
        assert_eq!(
            host_side.peer, viewer_addr,
            "the host follows the port that answered"
        );
    }

    #[tokio::test]
    async fn a_path_nobody_answers_reports_no_reply() {
        let (agent, _endpoint, _) = agent_on_loopback();
        let silent = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let result = agent
            .punch(
                silent.local_addr().unwrap(),
                None,
                Duration::from_millis(500),
            )
            .await;
        assert_eq!(result, Err(PunchError::NoReply));
    }

    #[tokio::test]
    async fn stopping_a_path_ends_the_wait() {
        let (agent, _endpoint, _) = agent_on_loopback();
        let silent = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let peer = silent.local_addr().unwrap();
        let waiting = tokio::spawn({
            let agent = agent.clone();
            async move { agent.punch(peer, None, Duration::from_secs(60)).await }
        });
        tokio::task::yield_now().await; // let it start punching
        agent.stop_punching(peer);
        let result = tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .expect("the wait ends")
            .unwrap();
        assert_eq!(result, Err(PunchError::Stopped));
    }

    /// The real rendezvous service on loopback. Its second port must be the
    /// first plus one, so a free pair is searched for.
    async fn rendezvous_service() -> SocketAddr {
        for _ in 0..50 {
            let main = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
            let port = main.local_addr().unwrap().port();
            let Some(next) = port.checked_add(1) else {
                continue;
            };
            if let Ok(alt) = tokio::net::UdpSocket::bind(("127.0.0.1", next)).await {
                let server = tidedesk_rendezvous::Server::new(Default::default());
                tokio::spawn(tidedesk_rendezvous::serve(main, alt, server));
                return SocketAddr::from(([127, 0, 0, 1], port));
            }
        }
        panic!("no two neighbouring free UDP ports on loopback");
    }

    #[tokio::test]
    async fn host_registers_with_a_service_and_punches_an_introduced_viewer() {
        use tidedesk_rendezvous_proto::{FromServer, ToServer, decode, encode};

        use crate::nat::punch::{Kind, Packet};

        let service = rendezvous_service().await;
        let identity = test_identity("nat-rendezvous");
        let (host, _endpoint, host_addr) = agent_on_loopback();
        assert_eq!(host.rendezvous(), RendezvousStatus::Off);
        host.start_rendezvous(service.to_string(), identity.rendezvous_credentials());
        assert_eq!(host.rendezvous(), RendezvousStatus::Connecting);
        let mut status = host.status();
        let registered =
            |s: &AgentStatus| matches!(s.rendezvous, RendezvousStatus::Registered { .. });
        tokio::time::timeout(Duration::from_secs(10), status.wait_for(registered))
            .await
            .expect("the host registers")
            .unwrap();
        assert!(matches!(
            host.rendezvous(),
            RendezvousStatus::Registered { public, nat: NatKind::EndpointIndependent } if public == host_addr
        ));

        // A viewer, by hand: Hello, Lookup, then one punch in the session.
        let viewer = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut buf = [0u8; 1500];
        let mut receive = async || -> (SocketAddr, Vec<u8>) {
            let (len, from) =
                tokio::time::timeout(Duration::from_secs(5), viewer.recv_from(&mut buf))
                    .await
                    .expect("an answer")
                    .unwrap();
            (from, buf[..len].to_vec())
        };
        viewer
            .send_to(&encode(&ToServer::hello([1; 8])), service)
            .await
            .unwrap();
        let Some(FromServer::Challenge { challenge, .. }) = decode(&receive().await.1) else {
            panic!("expected a challenge");
        };
        let lookup = ToServer::Lookup {
            device_id: identity.device_id(),
            nonce: [2; 8],
            challenge,
        };
        viewer.send_to(&encode(&lookup), service).await.unwrap();
        let Some(FromServer::Introduced { session, peer, .. }) = decode(&receive().await.1) else {
            panic!("expected an introduction");
        };
        assert_eq!(peer, host_addr);

        let punch = Packet {
            kind: Kind::Punch,
            session,
            token: [5; 8],
            observed: None,
        };
        // Punch until the host acks, as a real viewer does: the first punch
        // may arrive before the host has started its exchange.
        loop {
            viewer.send_to(&punch.encode(), peer).await.unwrap();
            let (from, data) = receive().await;
            if from == host_addr
                && let Some(packet) = Packet::decode(&data)
                && packet.kind == Kind::Ack
            {
                assert_eq!((packet.session, packet.token), (session, [5; 8]));
                break;
            }
        }

        host.stop_rendezvous();
        assert_eq!(host.rendezvous(), RendezvousStatus::Off);
    }

    #[test]
    fn introductions_are_limited_per_minute() {
        let t0 = Instant::now();
        let mut limit = IntroductionLimit::default();
        let allowed = (0..15).filter(|_| limit.allow(t0)).count();
        assert_eq!(allowed, MAX_INCOMING_PER_MINUTE);
        assert!(!limit.allow(t0 + Duration::from_secs(59)));
        assert!(
            limit.allow(t0 + Duration::from_secs(60)),
            "a minute later there is room again"
        );
    }

    #[test]
    fn unknown_nat_keeps_the_previous_classification_for_the_same_address() {
        let at = |port: u16, nat| PublicEndpoint {
            addr: SocketAddr::from(([203, 0, 113, 5], port)),
            nat,
            via: "a".into(),
        };
        let known = PublicStatus::Ready(at(40000, NatKind::EndpointIndependent));

        let merged = merge(&known, at(40000, NatKind::Unknown));
        assert_eq!(merged.nat, NatKind::EndpointIndependent);
        // A new address or a real classification is taken as it is.
        assert_eq!(
            merge(&known, at(40001, NatKind::Unknown)).nat,
            NatKind::Unknown
        );
        assert_eq!(
            merge(&known, at(40000, NatKind::Symmetric)).nat,
            NatKind::Symmetric
        );
        assert_eq!(
            merge(&PublicStatus::Discovering, at(40000, NatKind::Unknown)).nat,
            NatKind::Unknown
        );
    }

    #[test]
    fn status_reads_well_in_a_console() {
        let ready = PublicStatus::Ready(PublicEndpoint {
            addr: SocketAddr::from(([203, 0, 113, 5], 40000)),
            nat: NatKind::EndpointIndependent,
            via: "stun.example.org".into(),
        });
        assert_eq!(
            ready.to_string(),
            "203.0.113.5:40000 via stun.example.org (NAT: endpoint-independent)"
        );
        assert_eq!(
            PublicStatus::Unavailable("no reply from a".into()).to_string(),
            "unavailable (no reply from a)"
        );
    }
}
