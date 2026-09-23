//! The side-channel actor: owns the shared socket's tap and runs STUN on the
//! port QUIC uses, so the address it learns is the one peers must reach.

use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::runtime::Handle;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;

use super::socket::{RawDatagram, SharedSocket};
use super::stun::{Discovery, NatKind, PublicEndpoint, resolve_servers};

/// Side-channel datagrams buffered per listener before the oldest are dropped.
const FAN_OUT_CAPACITY: usize = 64;

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

/// Everything the agent reports. Later increments add hole-punching state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStatus {
    pub public: PublicStatus,
}

pub struct Agent {
    link: Link,
    runtime: Handle,
    dispatcher: JoinHandle<()>,
    refresh: Mutex<Option<JoinHandle<()>>>,
}

/// What a task working for the agent needs; cheap to clone into one.
#[derive(Clone)]
struct Link {
    socket: Arc<SharedSocket>,
    /// Every side-channel datagram, for whichever task is waiting for one.
    datagrams: broadcast::Sender<RawDatagram>,
    status: watch::Sender<AgentStatus>,
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
        });
        Ok(Arc::new(Self {
            link: Link {
                socket,
                datagrams,
                status,
            },
            runtime,
            dispatcher,
            refresh: Mutex::new(None),
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
        let task = self.runtime.spawn(async move {
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
        if let Some(task) = self.refresh.get_mut().unwrap().take() {
            task.abort();
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
