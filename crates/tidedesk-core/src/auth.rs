//! Access-code authentication.
//!
//! The host shows an access code; the viewer proves it knows the code without
//! sending it. The proof is an HMAC keyed by the code over keying material
//! exported from *this* TLS session. A machine-in-the-middle terminates two
//! different TLS sessions, so a proof it relays from one side is worthless on
//! the other, which keeps the code safe even on a first, not-yet-pinned
//! connection.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
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

/// Wrong codes, counted per address: one address that guesses is locked
/// out, with waits that grow, while viewers elsewhere (the owner's) still
/// get in. Many addresses guessing at once slow everyone down for a minute.
/// A 10-character code from a 31-symbol alphabet has ~49 bits of entropy;
/// online guessing is hopeless either way.
#[derive(Debug, Default)]
pub struct Throttle {
    addresses: HashMap<IpAddr, Failures>,
    /// When the recent wrong codes from any address came.
    recent: VecDeque<Instant>,
}

#[derive(Debug)]
struct Failures {
    count: u32,
    last: Instant,
    locked_until: Option<Instant>,
}

/// An address that is locked out, for the host to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Blocked {
    pub address: IpAddr,
    pub failures: u32,
    pub for_another: Duration,
}

impl Throttle {
    const FREE_ATTEMPTS: u32 = 5;
    const MAX_LOCKOUT: Duration = Duration::from_secs(60 * 60);
    /// An address that has not guessed for this long starts afresh.
    const FORGET_AFTER: Duration = Duration::from_secs(60 * 60);
    /// Wrong codes from all addresses together within [`Self::WINDOW`]
    /// that make everyone wait for the rest of it.
    const CEILING: usize = 30;
    const WINDOW: Duration = Duration::from_secs(60);

    pub fn is_locked(&mut self, address: IpAddr, now: Instant) -> bool {
        self.forget(now);
        self.recent.len() >= Self::CEILING
            || self
                .addresses
                .get(&address)
                .and_then(|f| f.locked_until)
                .is_some_and(|until| now < until)
    }

    pub fn record_failure(&mut self, address: IpAddr, now: Instant) {
        self.forget(now);
        self.recent.push_back(now);
        let failures = self.addresses.entry(address).or_insert(Failures {
            count: 0,
            last: now,
            locked_until: None,
        });
        failures.count += 1;
        failures.last = now;
        if failures.count >= Self::FREE_ATTEMPTS {
            let exp = (failures.count - Self::FREE_ATTEMPTS).min(12);
            let lock = Duration::from_secs(2u64 << exp).min(Self::MAX_LOCKOUT);
            failures.locked_until = Some(now + lock);
        }
    }

    pub fn record_success(&mut self, address: IpAddr) {
        self.addresses.remove(&address);
    }

    /// The addresses locked out now, the longest wait first.
    pub fn blocked(&mut self, now: Instant) -> Vec<Blocked> {
        self.forget(now);
        let mut blocked: Vec<_> = self
            .addresses
            .iter()
            .filter_map(|(address, f)| {
                let until = f.locked_until.filter(|until| now < *until)?;
                Some(Blocked {
                    address: *address,
                    failures: f.count,
                    for_another: until - now,
                })
            })
            .collect();
        blocked.sort_by_key(|b| std::cmp::Reverse(b.for_another));
        blocked
    }

    fn forget(&mut self, now: Instant) {
        while self
            .recent
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= Self::WINDOW)
        {
            self.recent.pop_front();
        }
        self.addresses.retain(|_, f| {
            f.locked_until.is_some_and(|until| now < until)
                || now.saturating_duration_since(f.last) < Self::FORGET_AFTER
        });
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

    fn ip(last: u8) -> IpAddr {
        IpAddr::from([203, 0, 113, last])
    }

    #[test]
    fn an_address_that_guesses_is_locked_out_and_others_are_not() {
        let now = Instant::now();
        let mut t = Throttle::default();
        for _ in 0..Throttle::FREE_ATTEMPTS - 1 {
            t.record_failure(ip(9), now);
            assert!(!t.is_locked(ip(9), now));
        }
        t.record_failure(ip(9), now);
        assert!(t.is_locked(ip(9), now));
        // The owner, somewhere else, still gets in.
        assert!(!t.is_locked(ip(1), now));
        assert!(!t.is_locked(ip(9), now + Duration::from_secs(3)));
        // The right code from one address forgets that address only.
        t.record_failure(ip(1), now);
        t.record_success(ip(1));
        t.record_failure(ip(9), now + Duration::from_secs(3));
        assert!(t.is_locked(ip(9), now + Duration::from_secs(3)));
        t.record_success(ip(9));
        assert!(!t.is_locked(ip(9), now + Duration::from_secs(3)));
    }

    #[test]
    fn the_wait_grows_to_an_hour() {
        let now = Instant::now();
        let mut t = Throttle::default();
        let mut waits = Vec::new();
        for _ in 0..30 {
            t.record_failure(ip(9), now);
            waits.push(t.blocked(now).first().map(|b| b.for_another));
        }
        assert_eq!(waits[3], None);
        assert_eq!(waits[4], Some(Duration::from_secs(2)));
        assert_eq!(waits[5], Some(Duration::from_secs(4)));
        assert_eq!(waits[29], Some(Duration::from_secs(60 * 60)));
        let blocked = &t.blocked(now)[0];
        assert_eq!((blocked.address, blocked.failures), (ip(9), 30));
    }

    #[test]
    fn many_addresses_guessing_at_once_slow_everyone_for_a_minute() {
        let now = Instant::now();
        let mut t = Throttle::default();
        for n in 0..Throttle::CEILING {
            assert!(!t.is_locked(ip(1), now));
            t.record_failure(ip(100 + n as u8), now);
        }
        assert!(t.is_locked(ip(1), now), "everyone waits");
        assert!(!t.is_locked(ip(1), now + Throttle::WINDOW));
    }

    #[test]
    fn an_address_that_stopped_guessing_is_forgotten() {
        let now = Instant::now();
        let mut t = Throttle::default();
        for _ in 0..Throttle::FREE_ATTEMPTS - 1 {
            t.record_failure(ip(9), now);
        }
        let later = now + Throttle::FORGET_AFTER;
        t.record_failure(ip(9), later);
        assert!(!t.is_locked(ip(9), later), "starts afresh");
        assert!(t.blocked(later).is_empty());
    }
}
