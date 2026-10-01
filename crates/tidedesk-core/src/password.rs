//! A password for the owner's own computers, as an alternative to the
//! access code.
//!
//! Neither side sends it. Both derive a key from it (Argon2id, salted with
//! the host's certificate fingerprint), run SPAKE2 with that key, bound to
//! this TLS session, and each proves to the other that it reached the same
//! secret. A fake host or a guessing viewer learns nothing it can test
//! guesses against offline: each connection is one guess, which the host's
//! lockout limits. The host keeps only the derived key, never the password.

use anyhow::{Result, anyhow, bail};
use argon2::{Algorithm, Argon2, Params, Version};
use ring::hmac;
use spake2::{Ed25519Group, Identity, Password, Spake2};

/// The derived key both sides run SPAKE2 with.
pub type Key = [u8; 32];
/// What each side sends to prove it reached the same secret.
pub type Proof = [u8; 32];

pub const MIN_LEN: usize = 12;

/// Passwords seen too often to be secret, lowercased. Short on purpose:
/// the length and variety rules catch most of the rest.
const COMMON: &[&str] = &[
    "password1234",
    "password12345",
    "password123456",
    "passwordpassword",
    "123456789012",
    "1234567890123",
    "12345678901234",
    "qwertyuiopas",
    "qwertyuiop123",
    "qwerty123456",
    "1q2w3e4r5t6y",
    "iloveyou1234",
    "administrator",
    "letmein12345",
    "welcome12345",
    "tidedesk1234",
];

/// Whether what a viewer typed is a password rather than an access code:
/// too long for a code and not shaped like one. A mistyped code, shorter
/// than any password, stays a code and gets the code's answer.
pub fn is_password(typed: &str) -> bool {
    !crate::auth::is_code(typed) && typed.chars().count() >= MIN_LEN
}

/// Why a password cannot be used, or None.
pub fn problem(password: &str) -> Option<&'static str> {
    let chars: Vec<char> = password.chars().collect();
    if chars.len() < MIN_LEN {
        return Some("Use at least 12 characters.");
    }
    let mut distinct = chars.clone();
    distinct.sort_unstable();
    distinct.dedup();
    if distinct.len() < 6 {
        return Some("Use more different characters.");
    }
    if COMMON.contains(&password.to_lowercase().as_str()) {
        return Some("This password is too common.");
    }
    if crate::auth::is_code(password) {
        return Some("This looks like an access code; choose something else.");
    }
    None
}

/// The key a password gives for the host with this certificate
/// fingerprint: slow to compute on purpose, so a stolen key file or a
/// guess costs time.
pub fn derive_key(password: &str, fingerprint: &str) -> Result<Key> {
    // Always 32 bytes, whatever the fingerprint's form.
    let fingerprint = crate::identity::normalize_fingerprint(fingerprint);
    let salt = ring::digest::digest(
        &ring::digest::SHA256,
        &[
            b"tidedesk password salt ".as_slice(),
            fingerprint.as_bytes(),
        ]
        .concat(),
    );
    let params = Params::new(64 * 1024, 3, 1, Some(32)).map_err(|e| anyhow!("{e}"))?;
    let mut key = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password.as_bytes(), salt.as_ref(), &mut key)
        .map_err(|e| anyhow!("deriving the password key: {e}"))?;
    Ok(key)
}

fn identities(binding: &[u8; 32]) -> (Identity, Identity) {
    let with = |who: &[u8]| Identity::new(&[who, binding.as_slice()].concat());
    (with(b"tidedesk viewer"), with(b"tidedesk host"))
}

fn proof(secret: &[u8], who: &[u8], binding: &[u8; 32]) -> Proof {
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    let tag = hmac::sign(&key, &[who, binding.as_slice()].concat());
    tag.as_ref().try_into().expect("SHA-256 tag is 32 bytes")
}

/// Whether `proof` is `who`'s, in constant time.
fn proves(secret: &[u8], who: &[u8], binding: &[u8; 32], proof: &Proof) -> bool {
    let key = hmac::Key::new(hmac::HMAC_SHA256, secret);
    hmac::verify(&key, &[who, binding.as_slice()].concat(), proof).is_ok()
}

/// The viewer's side, between its first message and the host's answer.
pub struct Viewer {
    state: Spake2<Ed25519Group>,
    binding: [u8; 32],
}

impl Viewer {
    /// Starts the exchange: the message to send to the host.
    pub fn start(key: &Key, binding: [u8; 32]) -> (Self, Vec<u8>) {
        let (viewer, host) = identities(&binding);
        let (state, message) = Spake2::<Ed25519Group>::start_a(&Password::new(key), &viewer, &host);
        (Self { state, binding }, message)
    }

    /// Checks the host's answer: the viewer's proof to send back, or an
    /// error when the host does not know the same password (a wrong
    /// password, or not the real host).
    pub fn finish(self, message: &[u8], host_proof: &Proof) -> Result<Proof> {
        let secret = self
            .state
            .finish(message)
            .map_err(|_| anyhow!("the host's answer is not valid"))?;
        if !proves(&secret, b"host", &self.binding, host_proof) {
            bail!("wrong password");
        }
        Ok(proof(&secret, b"viewer", &self.binding))
    }
}

/// The host's side, waiting for the viewer's proof.
pub struct Host {
    secret: Vec<u8>,
    binding: [u8; 32],
}

impl Host {
    /// Answers the viewer's first message: the message and proof to send.
    pub fn answer(key: &Key, binding: [u8; 32], message: &[u8]) -> Result<(Self, Vec<u8>, Proof)> {
        let (viewer, host) = identities(&binding);
        let (state, reply) = Spake2::<Ed25519Group>::start_b(&Password::new(key), &viewer, &host);
        let secret = state
            .finish(message)
            .map_err(|_| anyhow!("the viewer's message is not valid"))?;
        let host_proof = proof(&secret, b"host", &binding);
        Ok((Self { secret, binding }, reply, host_proof))
    }

    /// Whether the viewer proved it knows the password.
    pub fn accepts(&self, viewer_proof: &Proof) -> bool {
        proves(&self.secret, b"viewer", &self.binding, viewer_proof)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FINGERPRINT: &str = "5DC3 28F3 124B 12F4 855E A372 C310 B311";

    fn run(
        viewer_key: &Key,
        host_key: &Key,
        viewer_binding: [u8; 32],
        host_binding: [u8; 32],
    ) -> Result<bool> {
        let (viewer, first) = Viewer::start(viewer_key, viewer_binding);
        let (host, reply, host_proof) = Host::answer(host_key, host_binding, &first)?;
        let viewer_proof = viewer.finish(&reply, &host_proof)?;
        Ok(host.accepts(&viewer_proof))
    }

    #[test]
    fn the_same_password_opens_and_another_does_not() {
        let key = derive_key("correct horse battery", FINGERPRINT).unwrap();
        let binding = [7u8; 32];
        assert!(run(&key, &key, binding, binding).unwrap());

        let wrong = derive_key("correct horse battery!", FINGERPRINT).unwrap();
        let why = run(&wrong, &key, binding, binding).unwrap_err().to_string();
        assert_eq!(why, "wrong password");
    }

    /// A machine in the middle runs two TLS sessions: what it passes on
    /// from one does not fit the other.
    #[test]
    fn the_exchange_is_bound_to_its_connection() {
        let key = derive_key("correct horse battery", FINGERPRINT).unwrap();
        assert!(run(&key, &key, [1u8; 32], [2u8; 32]).is_err());
    }

    /// A viewer that never got the host's proof right cannot make one the
    /// host accepts either.
    #[test]
    fn the_host_accepts_only_the_viewers_proof() {
        let key = derive_key("correct horse battery", FINGERPRINT).unwrap();
        let (_, first) = Viewer::start(&key, [7u8; 32]);
        let (host, _, host_proof) = Host::answer(&key, [7u8; 32], &first).unwrap();
        assert!(!host.accepts(&host_proof));
        assert!(!host.accepts(&[0u8; 32]));
    }

    #[test]
    fn the_key_depends_on_the_host() {
        let here = derive_key("correct horse battery", FINGERPRINT).unwrap();
        assert_eq!(
            here,
            derive_key("correct horse battery", &FINGERPRINT.to_lowercase()).unwrap()
        );
        let there = derive_key("correct horse battery", "AAAA BBBB").unwrap();
        assert_ne!(here, there);
    }

    #[test]
    fn what_was_typed_is_told_apart() {
        assert!(is_password("correct horse battery"));
        assert!(!is_password("K7QM-3XPA-WZ"), "a code");
        assert!(!is_password("K7QM-3XPA-W"), "a code with a letter missing");
        assert!(!is_password("ABCD-EFGH"));
    }

    #[test]
    fn weak_passwords_are_refused() {
        assert_eq!(problem("short"), Some("Use at least 12 characters."));
        assert_eq!(
            problem("aaaaaaaaaaaaaaa"),
            Some("Use more different characters.")
        );
        assert_eq!(
            problem("121212121212"),
            Some("Use more different characters.")
        );
        assert_eq!(
            problem("Password1234"),
            Some("This password is too common.")
        );
        assert!(problem("K7QM-3XPA-WZ").is_some(), "shaped like a code");
        assert_eq!(problem("correct horse battery"), None);
        assert_eq!(problem("Bucătărie-2026!"), None);
    }
}
