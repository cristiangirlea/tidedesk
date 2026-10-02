//! Shared building blocks for TideDesk: the wire protocol, authentication,
//! host identity/pinning, QUIC endpoint setup and small audio helpers.
//!
//! Nothing in this crate is platform-specific, so Linux, Android and iOS ports
//! reuse it unchanged; only capture, input and presentation live per platform.

pub mod audio;
pub mod auth;
pub mod chat;
pub mod clipboard;
pub mod company;
pub use tidedesk_signed::dates;
pub mod files;
pub mod history;
pub mod identity;
pub mod licence;
pub mod logs;
pub mod nat;
pub mod net;
pub mod password;
pub mod paths;
pub mod protocol;
pub mod secret;
pub mod sharing;
pub mod stats;
pub mod streaming;

/// Default UDP port the host listens on.
pub const DEFAULT_PORT: u16 = 47800;

/// The terms of use, including the connection service's rules.
pub const TERMS_URL: &str =
    "https://github.com/cristiangirlea/tidedesk/blob/main/docs/terms-of-use.md";

#[cfg(test)]
mod tests {
    #[test]
    fn terms_url_points_at_the_terms_document() {
        let document = "docs/terms-of-use.md";
        assert_eq!(
            super::TERMS_URL,
            format!("https://github.com/cristiangirlea/tidedesk/blob/main/{document}")
        );
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(document);
        assert!(path.is_file(), "{}", path.display());
    }
}
