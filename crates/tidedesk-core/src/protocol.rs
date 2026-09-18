//! Wire protocol.
//!
//! One QUIC connection carries three channels, chosen so that each kind of data
//! gets the delivery guarantee it needs and none can stall the others:
//!
//! * **Control** — a bidirectional stream opened by the viewer. Length-prefixed
//!   postcard messages: handshake, input events, keyframe requests.
//! * **Video** — a unidirectional stream opened by the host. Reliable and
//!   ordered, because H.264 frames reference each other. Latency is kept low by
//!   dropping frames *before* encoding when the network falls behind.
//! * **Audio** — QUIC datagrams. Unreliable: a late audio packet is useless,
//!   and Opus conceals a lost one.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Bumped on any incompatible change to the messages below.
pub const PROTOCOL_VERSION: u16 = 3;

/// Text only; leave space for framing and variant metadata.
pub const MAX_CLIPBOARD_BYTES: usize = 48 * 1024;

/// Upper bound on a control message, so a hostile peer cannot make us allocate
/// arbitrary memory from a length prefix.
const MAX_CONTROL_MESSAGE: usize = 64 * 1024;

/// Upper bound on one encoded video frame (a 4K keyframe is well under this).
pub const MAX_VIDEO_FRAME: usize = 32 * 1024 * 1024;

/// Viewer → host control messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientMessage {
    /// First message on the control stream. See [`crate::auth`].
    Hello {
        protocol_version: u16,
        client_name: String,
        auth_tag: [u8; 32],
        want_audio: bool,
    },
    Input(InputEvent),
    /// Sent when the decoder lost sync; the host answers with an IDR frame.
    RequestKeyframe,
    SetSharing {
        request: u64,
        clipboard: bool,
        mouse: bool,
    },
    Clipboard {
        generation: u64,
        text: String,
    },
    /// Read the host pointer without moving it.
    PointerSync {
        request: u64,
    },
    /// Mouse events are accepted only against the current host pointer epoch.
    MouseInput {
        epoch: u64,
        event: InputEvent,
    },
    ReleaseMouse,
    /// Live streaming preset; independent of clipboard and input permissions.
    SetGameBoost {
        request: u64,
        enabled: bool,
    },
}

/// Host → viewer control messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServerMessage {
    Welcome {
        host_name: String,
        width: u32,
        height: u32,
        audio: bool,
    },
    Rejected {
        reason: RejectReason,
    },
    Sharing(crate::sharing::SharingState),
    Clipboard {
        generation: u64,
        text: String,
    },
    Pointer(crate::sharing::PointerPosition),
    PointerAnchor {
        request: u64,
        position: crate::sharing::PointerPosition,
    },
    /// Display-only telemetry; never authorizes input or warps the viewer cursor.
    Cursor(crate::sharing::PointerPosition),
    /// Acknowledged only after the encoder has produced a frame with this preset.
    Streaming(crate::streaming::StreamingStatus),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum RejectReason {
    BadCode,
    TooManyAttempts,
    Busy,
    IncompatibleVersion {
        host_version: u16,
    },
    /// The host is running but has paused accepting viewers.
    NotAccepting,
}

impl std::fmt::Display for RejectReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadCode => write!(f, "wrong access code"),
            Self::TooManyAttempts => write!(f, "too many failed attempts, try again later"),
            Self::Busy => write!(f, "host already has a viewer connected"),
            Self::NotAccepting => write!(f, "host is not accepting connections right now"),
            Self::IncompatibleVersion { host_version } => write!(
                f,
                "protocol mismatch (host speaks v{host_version}, viewer speaks v{PROTOCOL_VERSION})"
            ),
        }
    }
}

/// Input captured on the viewer and injected on the host.
///
/// Pointer positions are normalised to `0..=65535` across the shared screen so
/// they survive any scaling between the two sides.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum InputEvent {
    MouseMove {
        x: u16,
        y: u16,
    },
    MouseButton {
        button: MouseButton,
        pressed: bool,
    },
    /// Wheel delta in Windows units (120 per notch); positive is up/right.
    MouseWheel {
        dx: i32,
        dy: i32,
    },
    /// PC/AT set-1 scancode; extended keys carry the `0xE0` prefix in the high byte.
    Key {
        scancode: u16,
        pressed: bool,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

/// Header in front of every encoded frame on the video stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoFrameHeader {
    pub len: u32,
    pub width: u16,
    pub height: u16,
    pub keyframe: bool,
    /// Capture time in microseconds on the host's monotonic clock.
    pub capture_us: u64,
}

impl VideoFrameHeader {
    pub const SIZE: usize = 4 + 2 + 2 + 1 + 8;

    pub fn encode(&self) -> [u8; Self::SIZE] {
        let mut b = [0u8; Self::SIZE];
        b[0..4].copy_from_slice(&self.len.to_le_bytes());
        b[4..6].copy_from_slice(&self.width.to_le_bytes());
        b[6..8].copy_from_slice(&self.height.to_le_bytes());
        b[8] = self.keyframe as u8;
        b[9..17].copy_from_slice(&self.capture_us.to_le_bytes());
        b
    }

    pub fn decode(b: &[u8; Self::SIZE]) -> Self {
        Self {
            len: u32::from_le_bytes(b[0..4].try_into().unwrap()),
            width: u16::from_le_bytes(b[4..6].try_into().unwrap()),
            height: u16::from_le_bytes(b[6..8].try_into().unwrap()),
            keyframe: b[8] != 0,
            capture_us: u64::from_le_bytes(b[9..17].try_into().unwrap()),
        }
    }
}

/// Audio datagram: `[seq: u32 LE][opus packet]`.
pub fn encode_audio_datagram(seq: u32, opus: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(4 + opus.len());
    v.extend_from_slice(&seq.to_le_bytes());
    v.extend_from_slice(opus);
    v
}

pub fn decode_audio_datagram(d: &[u8]) -> Option<(u32, &[u8])> {
    let seq = u32::from_le_bytes(d.get(0..4)?.try_into().ok()?);
    Some((seq, &d[4..]))
}

pub async fn write_message<W, M>(w: &mut W, msg: &M) -> Result<()>
where
    W: AsyncWrite + Unpin,
    M: Serialize,
{
    let body = postcard::to_stdvec(msg)?;
    if body.len() > MAX_CONTROL_MESSAGE {
        bail!("control message exceeds limit");
    }
    w.write_all(&(body.len() as u32).to_le_bytes()).await?;
    w.write_all(&body).await?;
    Ok(())
}

/// Reads one control message; `Ok(None)` means the peer closed the stream cleanly.
pub async fn read_message<R, M>(r: &mut R) -> Result<Option<M>>
where
    R: AsyncRead + Unpin,
    M: for<'de> Deserialize<'de>,
{
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_CONTROL_MESSAGE {
        bail!("control message of {len} bytes exceeds limit");
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    Ok(Some(
        postcard::from_bytes(&body).context("malformed control message")?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_header_round_trips() {
        let h = VideoFrameHeader {
            len: 123_456,
            width: 2560,
            height: 1440,
            keyframe: true,
            capture_us: 987_654_321,
        };
        assert_eq!(VideoFrameHeader::decode(&h.encode()), h);
    }

    #[test]
    fn audio_datagram_round_trips() {
        let d = encode_audio_datagram(42, &[1, 2, 3]);
        assert_eq!(decode_audio_datagram(&d), Some((42, &[1u8, 2, 3][..])));
        assert_eq!(decode_audio_datagram(&[1, 2]), None);
    }

    #[tokio::test]
    async fn control_messages_round_trip() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let sent = ClientMessage::Input(InputEvent::Key {
            scancode: 0xE04B,
            pressed: true,
        });
        write_message(&mut a, &sent).await.unwrap();
        drop(a);
        let got: ClientMessage = read_message(&mut b).await.unwrap().unwrap();
        assert!(matches!(
            got,
            ClientMessage::Input(InputEvent::Key {
                scancode: 0xE04B,
                pressed: true
            })
        ));
        assert!(
            read_message::<_, ClientMessage>(&mut b)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn oversized_length_prefix_is_rejected() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&u32::MAX.to_le_bytes()).await.unwrap();
        assert!(read_message::<_, ClientMessage>(&mut b).await.is_err());
    }

    #[tokio::test]
    async fn clipboard_limit_and_mouse_handoff_survive_fragmented_streams() {
        let messages = vec![
            ClientMessage::SetSharing {
                request: 42,
                clipboard: true,
                mouse: true,
            },
            ClientMessage::Clipboard {
                generation: 3,
                text: "a".repeat(MAX_CLIPBOARD_BYTES),
            },
            ClientMessage::PointerSync { request: 17 },
            ClientMessage::MouseInput {
                epoch: 8,
                event: InputEvent::MouseMove { x: 12, y: 34 },
            },
            ClientMessage::ReleaseMouse,
            ClientMessage::SetGameBoost {
                request: 18,
                enabled: true,
            },
            ClientMessage::SetGameBoost {
                request: 19,
                enabled: false,
            },
        ];
        let expected = messages.clone();
        let (mut a, mut b) = tokio::io::duplex(17);
        let writer = tokio::spawn(async move {
            for message in messages {
                write_message(&mut a, &message).await.unwrap();
            }
        });
        for message in expected {
            assert_eq!(
                read_message::<_, ClientMessage>(&mut b).await.unwrap(),
                Some(message)
            );
        }
        writer.await.unwrap();
    }

    #[tokio::test]
    async fn server_permissions_and_anchor_round_trip() {
        let position = crate::sharing::PointerPosition {
            epoch: 9,
            x: 40000,
            y: 50000,
            inside: true,
        };
        let messages = vec![
            ServerMessage::Sharing(crate::sharing::SharingState {
                request: 1,
                generation: 2,
                clipboard: true,
                mouse: true,
            }),
            ServerMessage::Pointer(position),
            ServerMessage::Cursor(position),
            ServerMessage::Streaming(crate::streaming::StreamingStatus::requested(
                9, true, 30, 8_000_000,
            )),
            ServerMessage::PointerAnchor {
                request: 8,
                position,
            },
            ServerMessage::Clipboard {
                generation: 2,
                text: "copy back".into(),
            },
        ];
        for message in messages {
            let encoded = postcard::to_stdvec(&message).unwrap();
            let decoded: ServerMessage = postcard::from_bytes(&encoded).unwrap();
            assert_eq!(decoded, message);
        }
    }

    #[tokio::test]
    async fn oversized_outbound_messages_are_rejected_before_writing() {
        let (mut a, _) = tokio::io::duplex(1);
        let message = ClientMessage::Clipboard {
            generation: 1,
            text: "a".repeat(MAX_CONTROL_MESSAGE),
        };
        assert!(write_message(&mut a, &message).await.is_err());
    }
}
