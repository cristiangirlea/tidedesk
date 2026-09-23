//! A UDP socket shared by QUIC and the NAT side channel.

use std::fmt;
use std::io::{self, IoSliceMut};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll, ready};

use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, Runtime, UdpPoller};
use tokio::sync::mpsc;

/// A side-channel datagram taken out of QUIC's way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawDatagram {
    pub from: SocketAddr,
    pub data: Vec<u8>,
}

/// How many side-channel datagrams may wait unread before new ones are dropped.
/// Side-channel traffic is retransmitted by design, so dropping is harmless.
const TAP_CAPACITY: usize = 256;

/// Wraps the socket quinn would otherwise create for itself. Datagrams that
/// belong to the side channel (see [`super::is_side_channel`]) are diverted to
/// a channel instead of reaching QUIC, and [`SharedSocket::send_raw`] sends
/// datagrams from the same port QUIC uses.
///
/// Diverted datagrams are only seen while a quinn endpoint is driving the
/// socket, since the endpoint is what reads from it.
pub struct SharedSocket {
    inner: Arc<dyn AsyncUdpSocket>,
    tap: mpsc::Sender<RawDatagram>,
}

impl SharedSocket {
    /// Binds a new socket. Must be called inside a tokio runtime.
    pub fn bind(addr: SocketAddr) -> io::Result<(Arc<Self>, mpsc::Receiver<RawDatagram>)> {
        Self::from_std(std::net::UdpSocket::bind(addr)?)
    }

    /// Wraps an already bound socket. Must be called inside a tokio runtime.
    pub fn from_std(
        socket: std::net::UdpSocket,
    ) -> io::Result<(Arc<Self>, mpsc::Receiver<RawDatagram>)> {
        let inner = quinn::TokioRuntime.wrap_udp_socket(socket)?;
        let (tap, side_channel) = mpsc::channel(TAP_CAPACITY);
        Ok((Arc::new(Self { inner, tap }), side_channel))
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    /// Sends one datagram from the shared port, waiting until the socket can
    /// take it. As with any UDP send, delivery is not guaranteed.
    pub async fn send_raw(&self, to: SocketAddr, data: &[u8]) -> io::Result<()> {
        let transmit = Transmit {
            destination: to,
            ecn: None,
            contents: data,
            segment_size: None,
            src_ip: None,
        };
        // The runtime only lets a send through once it has seen the socket
        // become writable, so wait for that first, as quinn itself does.
        let mut poller = self.inner.clone().create_io_poller();
        loop {
            std::future::poll_fn(|cx| poller.as_mut().poll_writable(cx)).await?;
            match self.inner.try_send(&transmit) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                result => return result,
            }
        }
    }

    fn divert(&self, from: SocketAddr, data: &[u8]) {
        let from = SocketAddr::new(from.ip().to_canonical(), from.port());
        // A full or closed tap just drops the datagram; see TAP_CAPACITY.
        let _ = self.tap.try_send(RawDatagram {
            from,
            data: data.to_vec(),
        });
    }
}

impl fmt::Debug for SharedSocket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedSocket")
            .field("inner", &self.inner)
            .finish_non_exhaustive()
    }
}

impl AsyncUdpSocket for SharedSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        self.inner.clone().create_io_poller()
    }

    fn try_send(&self, transmit: &Transmit) -> io::Result<()> {
        self.inner.try_send(transmit)
    }

    fn poll_recv(
        &self,
        cx: &mut Context,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        let received = ready!(self.inner.poll_recv(cx, bufs, meta))?;
        for (buf, meta) in bufs.iter_mut().zip(meta.iter_mut()).take(received) {
            split_segments(buf, meta, &mut |from, data| self.divert(from, data));
        }
        Poll::Ready(Ok(received))
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    fn max_transmit_segments(&self) -> usize {
        self.inner.max_transmit_segments()
    }

    fn max_receive_segments(&self) -> usize {
        self.inner.max_receive_segments()
    }

    fn may_fragment(&self) -> bool {
        self.inner.may_fragment()
    }
}

/// Walks the datagrams in one receive buffer, hands side-channel ones to
/// `divert` and moves the remaining QUIC datagrams to the front, in order.
///
/// With receive offload (GRO) one buffer can hold several datagrams from the
/// same sender, each `stride` bytes long except possibly the last. Keeping
/// the order preserves that shape, so quinn can still split the buffer by
/// `stride`. `meta.len` becomes 0 when nothing is left for QUIC; `stride` is
/// never changed (quinn would loop forever on a zero stride).
pub(crate) fn split_segments(
    buf: &mut [u8],
    meta: &mut RecvMeta,
    divert: &mut dyn FnMut(SocketAddr, &[u8]),
) {
    let len = meta.len.min(buf.len());
    let stride = if meta.stride == 0 { len } else { meta.stride };
    let (mut read, mut write) = (0, 0);
    while read < len {
        let end = (read + stride).min(len);
        if super::is_side_channel(&buf[read..end]) {
            divert(meta.addr, &buf[read..end]);
        } else {
            buf.copy_within(read..end, write);
            write += end - read;
        }
        read = end;
    }
    meta.len = write;
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    use crate::identity::HostIdentity;
    use crate::nat::{PUNCH_MAGIC, STUN_MAGIC_COOKIE};
    use crate::net;

    const PEER: &str = "203.0.113.7:40001";

    fn meta(len: usize, stride: usize) -> RecvMeta {
        RecvMeta {
            addr: PEER.parse().unwrap(),
            len,
            stride,
            ecn: None,
            dst_ip: None,
        }
    }

    fn quic(fill: u8, len: usize) -> Vec<u8> {
        let mut d = vec![fill; len];
        d[0] = 0x40 | (fill & 0x3F); // short header: fixed bit set
        d
    }

    fn stun(len: usize) -> Vec<u8> {
        let mut d = vec![0u8; len];
        d[1] = 0x01;
        d[4..8].copy_from_slice(&STUN_MAGIC_COOKIE);
        d
    }

    fn punch(seq: u8, len: usize) -> Vec<u8> {
        let mut d = vec![seq; len];
        d[..4].copy_from_slice(&PUNCH_MAGIC);
        d
    }

    #[test]
    fn split_segments_compacts_coalesced_batches_in_place() {
        let (q1, p, q2, s, tail) = (
            quic(1, 40),
            punch(9, 40),
            quic(2, 40),
            stun(40),
            quic(3, 10),
        );
        let mut buf = [q1.clone(), p.clone(), q2.clone(), s.clone(), tail.clone()].concat();
        let mut m = meta(buf.len(), 40);
        let mut diverted = Vec::new();
        split_segments(&mut buf, &mut m, &mut |from, d| {
            diverted.push((from, d.to_vec()))
        });

        let from: SocketAddr = PEER.parse().unwrap();
        assert_eq!(diverted, vec![(from, p), (from, s)]);
        assert_eq!(m.len, 90);
        assert_eq!(m.stride, 40);
        assert_eq!(&buf[..90], [q1, q2, tail].concat().as_slice());

        // Everything diverted: nothing left for QUIC, stride untouched.
        let mut buf = [punch(1, 42), punch(2, 42)].concat();
        let mut m = meta(buf.len(), 42);
        let mut count = 0;
        split_segments(&mut buf, &mut m, &mut |_, _| count += 1);
        assert_eq!((count, m.len, m.stride), (2, 0, 42));

        // Single datagram without offload (stride == len), left alone.
        let q = quic(7, 1200);
        let mut buf = q.clone();
        let mut m = meta(buf.len(), buf.len());
        split_segments(&mut buf, &mut m, &mut |_, _| {
            panic!("QUIC must not be diverted")
        });
        assert_eq!((m.len, buf), (1200, q));
    }

    #[tokio::test]
    async fn send_raw_reaches_a_plain_udp_socket() {
        let (shared, _tap) = SharedSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let plain = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        shared
            .send_raw(plain.local_addr().unwrap(), b"hello from the shared port")
            .await
            .unwrap();

        let mut buf = [0u8; 64];
        let (n, from) = tokio::time::timeout(Duration::from_secs(5), plain.recv_from(&mut buf))
            .await
            .expect("datagram arrives")
            .unwrap();
        assert_eq!(&buf[..n], b"hello from the shared port");
        assert_eq!(from, shared.local_addr().unwrap());
    }

    fn test_identity() -> HostIdentity {
        let dir =
            std::env::temp_dir().join(format!("tidedesk-test-nat-socket-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        HostIdentity::load_or_create(&dir).unwrap()
    }

    #[tokio::test]
    async fn side_channel_datagrams_are_diverted_while_quic_connects() {
        let identity = test_identity();
        let (host_socket, mut host_tap) =
            SharedSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let (viewer_socket, _viewer_tap) =
            SharedSocket::bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let host_addr = host_socket.local_addr().unwrap();
        let viewer_addr = viewer_socket.local_addr().unwrap();
        let host = net::server_endpoint_on(host_socket, &identity).unwrap();
        let viewer = net::client_endpoint_on(viewer_socket.clone()).unwrap();

        let mut expected = Vec::new();
        for i in 0..5u8 {
            let mut s = stun(20);
            s[8] = i;
            viewer_socket.send_raw(host_addr, &s).await.unwrap();
            expected.push(s);
            let p = punch(i, 42);
            viewer_socket.send_raw(host_addr, &p).await.unwrap();
            expected.push(p);
        }

        let server = tokio::spawn(async move {
            let conn = host.accept().await.unwrap().await.unwrap();
            let (mut send, mut recv) = conn.accept_bi().await.unwrap();
            let data = recv.read_to_end(64 * 1024).await.unwrap();
            send.write_all(&data).await.unwrap();
            send.finish().unwrap();
            conn.closed().await;
        });

        let conn = viewer
            .connect(host_addr, "tidedesk-host")
            .unwrap()
            .await
            .unwrap();
        assert_eq!(net::peer_fingerprint(&conn), Some(identity.fingerprint()));

        let (mut send, mut recv) = conn.open_bi().await.unwrap();
        let payload: Vec<u8> = (0..10 * 1024).map(|i| i as u8).collect();
        for (i, chunk) in payload.chunks(512).enumerate() {
            send.write_all(chunk).await.unwrap();
            let p = punch(100 + i as u8, 42);
            viewer_socket.send_raw(host_addr, &p).await.unwrap();
            expected.push(p);
        }
        send.finish().unwrap();
        let echoed = recv.read_to_end(64 * 1024).await.unwrap();
        assert_eq!(echoed, payload);

        let mut received = Vec::new();
        while received.len() < expected.len() {
            let d = tokio::time::timeout(Duration::from_secs(5), host_tap.recv())
                .await
                .expect("every side-channel datagram reaches the tap")
                .unwrap();
            assert_eq!(d.from, viewer_addr);
            received.push(d.data);
        }
        assert_eq!(received, expected);

        conn.close(0u32.into(), b"done");
        server.await.unwrap();
    }

    #[test]
    fn endpoint_config_disables_fixed_bit_greasing() {
        let config = format!("{:?}", net::endpoint_config());
        assert!(config.contains("grease_quic_bit: false"), "{config}");
    }
}
