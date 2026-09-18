//! Shared building blocks for TideDesk: the wire protocol, authentication,
//! host identity/pinning, QUIC endpoint setup and small audio helpers.
//!
//! Nothing in this crate is platform-specific, so Linux, Android and iOS ports
//! reuse it unchanged; only capture, input and presentation live per platform.

pub mod audio;
pub mod auth;
pub mod clipboard;
pub mod identity;
pub mod net;
pub mod paths;
pub mod protocol;
pub mod sharing;
pub mod stats;
pub mod streaming;

/// Default UDP port the host listens on.
pub const DEFAULT_PORT: u16 = 47800;
