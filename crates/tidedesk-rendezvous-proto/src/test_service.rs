//! A minimal rendezvous service for tests: the wire protocol without the
//! limits and hardening of a real one. Hosts and viewers under test talk to
//! it on loopback exactly as they would to a service on the internet.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::time::Duration;

use ring::digest;
use ring::rand::{SecureRandom, SystemRandom};
use tokio::net::UdpSocket;

use crate::{
    Challenge, DeviceId, ErrorCode, FromServer, HELLO_PADDING, Nonce, Session, ToServer, Token,
    decode, encode, verify_registration,
};

/// Which of the two listening ports a datagram came in on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Port {
    Main,
    /// Answers only Hello: its view of the sender's address, compared with
    /// the main port's, tells a symmetric NAT.
    Alt,
}

pub struct Service {
    hosts: HashMap<DeviceId, (SocketAddr, Token)>,
    /// Challenges handed out, with the address each went to.
    issued: HashSet<(SocketAddr, Challenge)>,
    rng: SystemRandom,
}

impl Default for Service {
    fn default() -> Self {
        Self::new()
    }
}

impl Service {
    pub fn new() -> Self {
        Self {
            hosts: HashMap::new(),
            issued: HashSet::new(),
            rng: SystemRandom::new(),
        }
    }

    /// Handles one datagram; returns the datagrams to send, and where.
    pub fn handle(
        &mut self,
        from: SocketAddr,
        port: Port,
        datagram: &[u8],
    ) -> Vec<(SocketAddr, Vec<u8>)> {
        let Some(message) = decode::<ToServer>(datagram) else {
            return Vec::new();
        };
        let reply = match (port, message) {
            (_, ToServer::Hello { nonce, padding }) => {
                if padding.len() < HELLO_PADDING {
                    return Vec::new(); // an answer larger than the question
                }
                let challenge: Challenge = self.random();
                self.issued.insert((from, challenge));
                FromServer::Challenge {
                    nonce,
                    challenge,
                    reflexive: from,
                }
            }
            (Port::Alt, _) => return Vec::new(),
            (
                Port::Main,
                ToServer::Register {
                    device_id,
                    cert_der,
                    challenge,
                    signature,
                },
            ) => {
                if !self.issued.contains(&(from, challenge)) {
                    FromServer::Error {
                        code: ErrorCode::UnknownChallenge,
                    }
                } else if !verify_registration(&cert_der, &device_id, &challenge, &signature) {
                    FromServer::Error {
                        code: ErrorCode::BadSignature,
                    }
                } else {
                    let token: Token = self.random();
                    self.hosts.insert(device_id, (from, token));
                    registered(device_id, token, from)
                }
            }
            (Port::Main, ToServer::Refresh { device_id, token }) => {
                match self.hosts.get_mut(&device_id) {
                    Some((addr, known)) if *known == token => {
                        *addr = from; // the router may have moved the host
                        registered(device_id, token, from)
                    }
                    _ => FromServer::Error {
                        code: ErrorCode::NotRegistered,
                    },
                }
            }
            (
                Port::Main,
                ToServer::Lookup {
                    device_id,
                    nonce,
                    challenge,
                },
            ) => {
                if !self.issued.contains(&(from, challenge)) {
                    FromServer::Error {
                        code: ErrorCode::UnknownChallenge,
                    }
                } else if let Some(&(host, _)) = self.hosts.get(&device_id) {
                    // The same lookup, retried after a lost reply, gets the
                    // same session, so the host runs one exchange per viewer.
                    let session = session_for(&device_id, from, &nonce);
                    let to_viewer = FromServer::Introduced {
                        nonce,
                        session,
                        peer: host,
                    };
                    let to_host = FromServer::Incoming {
                        session,
                        peer: from,
                    };
                    return vec![(from, encode(&to_viewer)), (host, encode(&to_host))];
                } else {
                    FromServer::NotFound { nonce }
                }
            }
        };
        vec![(from, encode(&reply))]
    }

    fn random<const N: usize>(&self) -> [u8; N] {
        let mut bytes = [0u8; N];
        self.rng.fill(&mut bytes).expect("system RNG failed");
        bytes
    }
}

fn registered(device_id: DeviceId, token: Token, reflexive: SocketAddr) -> FromServer {
    FromServer::Registered {
        device_id,
        token,
        reflexive,
        ttl_secs: 75,
    }
}

fn session_for(device_id: &DeviceId, viewer: SocketAddr, nonce: &Nonce) -> Session {
    let input = [
        b"session".as_slice(),
        &device_id.0,
        viewer.to_string().as_bytes(),
        nonce,
    ]
    .concat();
    let mut session: Session = digest::digest(&digest::SHA256, &input).as_ref()[..8]
        .try_into()
        .expect("SHA-256 is 32 bytes");
    if session == [0; 8] {
        session[0] = 1; // zero means "not known yet" to punching
    }
    session
}

/// Answers datagrams on both ports, each from the port it was asked on.
pub async fn serve(main: UdpSocket, alt: UdpSocket, mut service: Service) {
    let mut main_buf = vec![0u8; 2048];
    let mut alt_buf = vec![0u8; 2048];
    loop {
        let (port, received) = tokio::select! {
            received = main.recv_from(&mut main_buf) => (Port::Main, received),
            received = alt.recv_from(&mut alt_buf) => (Port::Alt, received),
        };
        let Ok((len, from)) = received else {
            // Windows reports an earlier send's "port unreachable" here.
            tokio::time::sleep(Duration::from_millis(10)).await;
            continue;
        };
        let (socket, buf) = match port {
            Port::Main => (&main, &main_buf),
            Port::Alt => (&alt, &alt_buf),
        };
        for (to, reply) in service.handle(from, port, &buf[..len]) {
            let _ = socket.send_to(&reply, to).await;
        }
    }
}

/// Starts a service on loopback and returns its main address. The second
/// port is the next one up, as hosts and viewers expect.
///
/// With `TIDEDESK_TEST_SERVICE=ip:port` set, nothing is started and that
/// address is returned instead: the way to run the same tests against a
/// real service on this machine.
pub async fn spawn() -> SocketAddr {
    if let Ok(external) = std::env::var("TIDEDESK_TEST_SERVICE") {
        return external
            .parse()
            .expect("TIDEDESK_TEST_SERVICE is an ip:port");
    }
    for _ in 0..50 {
        let main = UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("bind on loopback");
        let addr = main.local_addr().expect("a bound socket has an address");
        let Some(next) = addr.port().checked_add(1) else {
            continue;
        };
        if let Ok(alt) = UdpSocket::bind(("127.0.0.1", next)).await {
            tokio::spawn(serve(main, alt, Service::new()));
            return addr;
        }
    }
    panic!("no two neighbouring free UDP ports on loopback");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn answers_hello_on_both_ports_from_the_port_asked() {
        let service = spawn().await;
        let alt = SocketAddr::new(service.ip(), service.port() + 1);
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let me = client.local_addr().unwrap();
        for server in [service, alt] {
            client
                .send_to(&encode(&ToServer::hello([7; 8])), server)
                .await
                .unwrap();
            let mut buf = [0u8; 1500];
            let (len, from) =
                tokio::time::timeout(Duration::from_secs(5), client.recv_from(&mut buf))
                    .await
                    .expect("the service answers")
                    .unwrap();
            assert_eq!(from, server, "answered from the port that was asked");
            let reply = decode::<FromServer>(&buf[..len]).unwrap();
            assert!(matches!(reply, FromServer::Challenge { reflexive, .. } if reflexive == me));
        }
    }

    #[tokio::test]
    async fn unknown_id_is_not_found_and_lookups_need_a_challenge() {
        let service = spawn().await;
        let client = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut buf = [0u8; 1500];
        let mut ask = async |message: &ToServer| -> FromServer {
            client.send_to(&encode(message), service).await.unwrap();
            let (len, _) = tokio::time::timeout(Duration::from_secs(5), client.recv_from(&mut buf))
                .await
                .expect("the service answers")
                .unwrap();
            decode::<FromServer>(&buf[..len]).unwrap()
        };
        let forged = ToServer::Lookup {
            device_id: DeviceId([1; 8]),
            nonce: [2; 8],
            challenge: [3; 16],
        };
        assert!(matches!(
            ask(&forged).await,
            FromServer::Error {
                code: ErrorCode::UnknownChallenge
            }
        ));
        let FromServer::Challenge { challenge, .. } = ask(&ToServer::hello([4; 8])).await else {
            panic!("a Hello gets a challenge");
        };
        let honest = ToServer::Lookup {
            device_id: DeviceId([1; 8]),
            nonce: [2; 8],
            challenge,
        };
        assert!(matches!(ask(&honest).await, FromServer::NotFound { nonce } if nonce == [2; 8]));
    }
}
