//! Access-code authentication.
//!
//! The host shows an access code; the viewer proves it knows the code without
//! sending it. The proof is an HMAC keyed by the code over keying material
//! exported from *this* TLS session. A machine-in-the-middle terminates two
//! different TLS sessions, so a proof it relays from one side is worthless on
//! the other — which keeps the code safe even on a first, not-yet-pinned
//! connection.

use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use ring::hmac;
use ring::rand::{SecureRandom, SystemRandom};

const EXPORTER_LABEL: &[u8] = b"EXPORTER-tidedesk-auth-v1";

/// Unambiguous alphabet: no 0/O, 1/I/L.
const CODE_ALPHABET: &[u8] = b"23456789ABCDEFGHJKMNPQRSTUVWXYZ";
pub const CODE_LEN: usize = 10;

/// Generates a random access code, e.g. `K7QM-3XPA-WZ`.
pub fn generate_code() -> String {
    let rng = SystemRandom::new();
    let mut raw = [0u8; CODE_LEN];
    let mut out = String::with_capacity(CODE_LEN + 2);
    for i in 0..CODE_LEN {
        // Rejection sampling keeps the distribution uniform.
        let c = loop {
            rng.fill(&mut raw[i..=i]).expect("system RNG failed");
            let limit = 256 - (256 % CODE_ALPHABET.len());
            if (raw[i] as usize) < limit {
                break CODE_ALPHABET[raw[i] as usize % CODE_ALPHABET.len()];
            }
        };
        if i == 4 || i == 8 {
            out.push('-');
        }
        out.push(c as char);
    }
    out
}

/// Canonical form: uppercase, separators and spaces removed.
pub fn normalize_code(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

fn session_binding(conn: &quinn::Connection) -> Result<[u8; 32]> {
    let mut out = [0u8; 32];
    conn.export_keying_material(&mut out, EXPORTER_LABEL, b"")
        .map_err(|_| anyhow!("TLS keying material export failed"))?;
    Ok(out)
}

fn tag_for(code: &str, binding: &[u8; 32]) -> hmac::Tag {
    let key = hmac::Key::new(hmac::HMAC_SHA256, normalize_code(code).as_bytes());
    hmac::sign(&key, binding)
}

/// Viewer side: the proof to put in `Hello`.
pub fn client_tag(conn: &quinn::Connection, code: &str) -> Result<[u8; 32]> {
    let tag = tag_for(code, &session_binding(conn)?);
    Ok(tag.as_ref().try_into().expect("SHA-256 tag is 32 bytes"))
}

/// Host side: constant-time check of the viewer's proof.
pub fn verify_tag(conn: &quinn::Connection, code: &str, tag: &[u8; 32]) -> Result<bool> {
    let key = hmac::Key::new(hmac::HMAC_SHA256, normalize_code(code).as_bytes());
    Ok(hmac::verify(&key, &session_binding(conn)?, tag).is_ok())
}

/// Global throttle on failed attempts. A 10-character code from a 31-symbol
/// alphabet has ~49 bits of entropy; with exponential lock-outs online guessing
/// is hopeless.
#[derive(Debug, Default)]
pub struct Throttle {
    failures: u32,
    locked_until: Option<Instant>,
}

impl Throttle {
    const FREE_ATTEMPTS: u32 = 5;
    const MAX_LOCKOUT: Duration = Duration::from_secs(15 * 60);

    pub fn is_locked(&self, now: Instant) -> bool {
        self.locked_until.is_some_and(|t| now < t)
    }

    pub fn record_failure(&mut self, now: Instant) {
        self.failures += 1;
        if self.failures >= Self::FREE_ATTEMPTS {
            let exp = (self.failures - Self::FREE_ATTEMPTS).min(10);
            let lock = Duration::from_secs(2u64 << exp).min(Self::MAX_LOCKOUT);
            self.locked_until = Some(now + lock);
        }
    }

    pub fn record_success(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_well_formed_and_distinct() {
        let a = generate_code();
        let b = generate_code();
        assert_eq!(a.len(), CODE_LEN + 2);
        assert_eq!(normalize_code(&a).len(), CODE_LEN);
        assert!(
            normalize_code(&a)
                .bytes()
                .all(|c| CODE_ALPHABET.contains(&c))
        );
        assert_ne!(a, b);
    }

    #[test]
    fn normalization_ignores_case_and_separators() {
        assert_eq!(normalize_code("k7qm-3xpa wz"), "K7QM3XPAWZ");
        let binding = [7u8; 32];
        assert_eq!(
            tag_for("k7qm-3xpa-wz", &binding).as_ref(),
            tag_for("K7QM3XPAWZ", &binding).as_ref()
        );
        assert_ne!(
            tag_for("K7QM3XPAWZ", &binding).as_ref(),
            tag_for("K7QM3XPAWY", &binding).as_ref()
        );
    }

    #[test]
    fn throttle_locks_after_free_attempts_and_resets() {
        let now = Instant::now();
        let mut t = Throttle::default();
        for _ in 0..Throttle::FREE_ATTEMPTS - 1 {
            t.record_failure(now);
            assert!(!t.is_locked(now));
        }
        t.record_failure(now);
        assert!(t.is_locked(now));
        assert!(!t.is_locked(now + Duration::from_secs(3)));
        t.record_success();
        assert!(!t.is_locked(now));
    }
}
