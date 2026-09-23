//! Wire format of the TideDesk rendezvous service.
//!
//! A host registers the internet address its router gives it under a device
//! ID derived from its certificate. A viewer looks the ID up, and the service
//! introduces the two, who then punch a direct path between themselves. The
//! service never carries session data and cannot impersonate a host: the
//! viewer still checks the host's certificate on the direct connection.
//!
//! Every datagram is `00 'T' 'D' 'R'`, a version byte, then a postcard-encoded
//! message, at most [`MAX_DATAGRAM`] bytes in all.

#[cfg(feature = "test-service")]
pub mod test_service;

use std::fmt;
use std::net::SocketAddr;
use std::str::FromStr;

use ring::digest::{SHA256, digest};
use ring::rand::SystemRandom;
use ring::signature::{
    ECDSA_P256_SHA256_FIXED, ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, UnparsedPublicKey,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

/// First bytes of every rendezvous datagram. The zero keeps them apart from
/// QUIC, the rest from STUN and from punch packets (`00 'T' 'D' 'P'`).
pub const MAGIC: [u8; 4] = [0x00, b'T', b'D', b'R'];
pub const VERSION: u8 = 1;

/// Largest datagram: fits in one packet on any internet path.
pub const MAX_DATAGRAM: usize = 1200;

/// UDP port of the service; the second port is this one plus one.
pub const DEFAULT_PORT: u16 = 47900;

/// Padding every Hello carries, so that it is never smaller than its answer
/// and a forged source address cannot use the service to amplify traffic.
pub const HELLO_PADDING: usize = 48;

pub type Nonce = [u8; 8];
pub type Challenge = [u8; 16];
pub type Token = [u8; 16];
/// A punch session ID (see `tidedesk_core::nat::punch`).
pub type Session = [u8; 8];

/// Whether a datagram belongs to the rendezvous protocol.
pub fn is_signal(datagram: &[u8]) -> bool {
    datagram.starts_with(&MAGIC)
}

/// A host's stable name: the first 64 bits of its certificate's SHA-256,
/// shown as `TD-1A2B-3C4D-5E6F-7A8B`. Taking over an ID means finding
/// another certificate with the same hash prefix, about 2^64 work.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DeviceId(pub [u8; 8]);

impl DeviceId {
    pub fn from_cert(cert_der: &[u8]) -> Self {
        let hash = digest(&SHA256, cert_der);
        Self(hash.as_ref()[..8].try_into().expect("SHA-256 is 32 bytes"))
    }

    /// From a certificate fingerprint as TideDesk shows it (SHA-256 in hex).
    pub fn from_fingerprint_hex(fingerprint: &str) -> Option<Self> {
        let hex: String = fingerprint.chars().filter(|c| !c.is_whitespace()).collect();
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        hex[..16].parse().ok()
    }
}

impl fmt::Display for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TD")?;
        for pair in self.0.chunks(2) {
            write!(f, "-{:02X}{:02X}", pair[0], pair[1])?;
        }
        Ok(())
    }
}

impl fmt::Debug for DeviceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseDeviceIdError;

impl fmt::Display for ParseDeviceIdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a device ID looks like TD-1A2B-3C4D-5E6F-7A8B")
    }
}

impl std::error::Error for ParseDeviceIdError {}

impl FromStr for DeviceId {
    type Err = ParseDeviceIdError;

    /// Accepts any case, with or without the `TD` prefix and separators.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let text = text.trim();
        let text = match text.get(..2) {
            Some(prefix) if prefix.eq_ignore_ascii_case("td") => &text[2..],
            _ => text,
        };
        let hex: Vec<u8> = text
            .bytes()
            .filter(|b| !matches!(b, b'-' | b' ' | b':'))
            .collect();
        if hex.len() != 16 {
            return Err(ParseDeviceIdError);
        }
        let mut id = [0u8; 8];
        for (byte, pair) in id.iter_mut().zip(hex.chunks(2)) {
            let pair = std::str::from_utf8(pair).map_err(|_| ParseDeviceIdError)?;
            *byte = u8::from_str_radix(pair, 16).map_err(|_| ParseDeviceIdError)?;
        }
        Ok(Self(id))
    }
}

/// What hosts and viewers send to the service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToServer {
    /// Asks for a challenge, which later messages from the same address
    /// return to prove they come from it. Sent to the second port as well,
    /// it learns the address seen there, to tell a symmetric NAT. Build it
    /// with [`ToServer::hello`].
    Hello { nonce: Nonce, padding: Vec<u8> },
    /// Proves the host holds the key of the certificate its ID comes from.
    Register {
        device_id: DeviceId,
        cert_der: Vec<u8>,
        challenge: Challenge,
        signature: Vec<u8>,
    },
    /// Keeps a registration, and the router's mapping, alive.
    Refresh { device_id: DeviceId, token: Token },
    /// A viewer asks to be introduced to a host, returning a challenge from
    /// Hello: only addresses that proved they are real are introduced.
    Lookup {
        device_id: DeviceId,
        nonce: Nonce,
        challenge: Challenge,
    },
}

impl ToServer {
    /// A Hello with its padding.
    pub fn hello(nonce: Nonce) -> Self {
        Self::Hello {
            nonce,
            padding: vec![0; HELLO_PADDING],
        }
    }
}

/// What the service answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FromServer {
    Challenge {
        nonce: Nonce,
        challenge: Challenge,
        /// The address the Hello came from.
        reflexive: SocketAddr,
    },
    Registered {
        device_id: DeviceId,
        token: Token,
        reflexive: SocketAddr,
        ttl_secs: u16,
    },
    /// To a viewer: the host to punch towards, in this session.
    Introduced {
        nonce: Nonce,
        session: Session,
        peer: SocketAddr,
    },
    /// To a host: a viewer is about to punch towards it, in this session.
    Incoming {
        session: Session,
        peer: SocketAddr,
    },
    NotFound {
        nonce: Nonce,
    },
    Error {
        code: ErrorCode,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorCode {
    /// The challenge is unknown or expired: send Hello again.
    UnknownChallenge,
    BadSignature,
    /// A live registration holds this ID with another key.
    IdTaken,
    /// No live registration for this token: register again.
    NotRegistered,
    /// The service has no room for more hosts.
    Full,
}

pub fn encode<T: Serialize>(message: &T) -> Vec<u8> {
    let mut datagram = MAGIC.to_vec();
    datagram.push(VERSION);
    // Writing into a Vec cannot fail for these plain types.
    postcard::to_extend(message, datagram).expect("messages always serialize")
}

/// `None` for anything that is not a well-formed message of this version.
pub fn decode<T: DeserializeOwned>(datagram: &[u8]) -> Option<T> {
    if !is_signal(datagram) || datagram.len() > MAX_DATAGRAM || datagram.get(4) != Some(&VERSION) {
        return None;
    }
    match postcard::take_from_bytes(&datagram[5..]) {
        Ok((message, [])) => Some(message),
        _ => None,
    }
}

/// Labels what is signed, so the signature means nothing anywhere else.
const REGISTRATION_CONTEXT: &[u8] = b"tidedesk rendezvous registration v1\0";

/// What a host signs to register: bound to this protocol, the ID and the
/// service's fresh challenge, so a signature cannot be replayed.
pub fn registration_bytes(device_id: &DeviceId, challenge: &Challenge) -> Vec<u8> {
    [REGISTRATION_CONTEXT, &device_id.0, challenge].concat()
}

/// Signs a registration with the host's PKCS#8 ECDSA P-256 key.
pub fn sign_registration(
    pkcs8: &[u8],
    device_id: &DeviceId,
    challenge: &Challenge,
) -> Result<Vec<u8>, ring::error::Unspecified> {
    let rng = SystemRandom::new();
    let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8, &rng)
        .map_err(|_| ring::error::Unspecified)?;
    let signature = key.sign(&rng, &registration_bytes(device_id, challenge))?;
    Ok(signature.as_ref().to_vec())
}

/// Whether `signature` registers `device_id`: the ID must come from the
/// certificate, and the signature from the certificate's key.
pub fn verify_registration(
    cert_der: &[u8],
    device_id: &DeviceId,
    challenge: &Challenge,
    signature: &[u8],
) -> bool {
    if DeviceId::from_cert(cert_der) != *device_id {
        return false;
    }
    let Some(public_key) = public_key_from_cert(cert_der) else {
        return false;
    };
    UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, public_key)
        .verify(&registration_bytes(device_id, challenge), signature)
        .is_ok()
}

/// `SEQUENCE { id-ecPublicKey, prime256v1 }`, the only key algorithm TideDesk
/// certificates use.
const EC_P256_ALGORITHM: &[u8] = &[
    0x06, 0x07, 0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01, // 1.2.840.10045.2.1
    0x06, 0x08, 0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07, // 1.2.840.10045.3.1.7
];

/// The P-256 public key (an uncompressed point) of an X.509 certificate.
pub fn public_key_from_cert(cert_der: &[u8]) -> Option<&[u8]> {
    let (certificate, _) = element(cert_der, 0x30)?;
    let (mut tbs, _) = element(certificate, 0x30)?;
    if tbs.first() == Some(&0xA0) {
        tbs = skip(tbs)?; // [0] version
    }
    // Serial number, signature algorithm, issuer, validity, subject.
    for _ in 0..5 {
        tbs = skip(tbs)?;
    }
    let (public_key_info, _) = element(tbs, 0x30)?;
    let (algorithm, rest) = element(public_key_info, 0x30)?;
    if algorithm != EC_P256_ALGORITHM {
        return None;
    }
    let (bits, _) = element(rest, 0x03)?;
    let (&unused_bits, point) = bits.split_first()?;
    (unused_bits == 0 && point.len() == 65 && point[0] == 0x04).then_some(point)
}

/// One DER element with the expected tag: its contents and what follows.
fn element(input: &[u8], tag: u8) -> Option<(&[u8], &[u8])> {
    let (&found, rest) = input.split_first()?;
    if found != tag {
        return None;
    }
    let (&first, rest) = rest.split_first()?;
    let (length, rest) = if first < 0x80 {
        (usize::from(first), rest)
    } else {
        // Long form; certificates here are far smaller than 64 KiB.
        let count = usize::from(first & 0x7F);
        if !(1..=2).contains(&count) {
            return None;
        }
        let (bytes, rest) = rest.split_at_checked(count)?;
        let length = bytes
            .iter()
            .fold(0usize, |length, byte| length << 8 | usize::from(*byte));
        (length, rest)
    };
    rest.split_at_checked(length)
}

/// What follows the next DER element, whatever its tag.
fn skip(input: &[u8]) -> Option<&[u8]> {
    element(input, *input.first()?).map(|(_, rest)| rest)
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use super::*;

    fn host_cert() -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let generated = rcgen::generate_simple_self_signed(vec!["tidedesk-host".into()]).unwrap();
        (
            generated.cert.der().to_vec(),
            generated.signing_key.serialize_der(),
            generated.signing_key.public_key_raw().to_vec(),
        )
    }

    #[test]
    fn device_id_is_first_8_bytes_of_cert_sha256_and_formats_as_td_groups() {
        let (cert, _, _) = host_cert();
        let digest = ring::digest::digest(&ring::digest::SHA256, &cert);
        let id = DeviceId::from_cert(&cert);
        assert_eq!(id.0, digest.as_ref()[..8]);

        let id = DeviceId([0x1A, 0x2B, 0x3C, 0x4D, 0x5E, 0x6F, 0x7A, 0x8B]);
        assert_eq!(id.to_string(), "TD-1A2B-3C4D-5E6F-7A8B");
        assert_eq!(format!("{id:?}"), "TD-1A2B-3C4D-5E6F-7A8B");
    }

    #[test]
    fn device_id_parses_leniently_and_rejects_wrong_length() {
        let id = DeviceId([0x1A, 0x2B, 0x3C, 0x4D, 0x5E, 0x6F, 0x7A, 0x8B]);
        for text in [
            "TD-1A2B-3C4D-5E6F-7A8B",
            "td-1a2b-3c4d-5e6f-7a8b",
            " TD 1A2B 3C4D 5E6F 7A8B ",
            "1A2B3C4D5E6F7A8B",
        ] {
            assert_eq!(text.parse::<DeviceId>(), Ok(id), "{text}");
        }
        for bad in [
            "TD-1A2B",
            "TD-1A2B-3C4D-5E6F-7A8B-9",
            "TD-1A2B-3C4D-5E6F-7A8G",
            "",
            "TD",
        ] {
            assert!(bad.parse::<DeviceId>().is_err(), "{bad}");
        }

        // A viewer can work out the ID from the fingerprint the host shows.
        let (cert, _, _) = host_cert();
        let digest = ring::digest::digest(&ring::digest::SHA256, &cert);
        let shown: Vec<String> = digest
            .as_ref()
            .chunks(2)
            .map(|c| format!("{:02X}{:02X}", c[0], c[1]))
            .collect();
        let fingerprint = shown.join(" ");
        assert_eq!(
            DeviceId::from_fingerprint_hex(&fingerprint),
            Some(DeviceId::from_cert(&cert))
        );
        assert_eq!(DeviceId::from_fingerprint_hex("ABCD"), None);
        // 64 bytes, but not 64 hex digits: no panic, just no ID.
        let odd = format!("{}é{}", "A".repeat(15), "B".repeat(47));
        assert_eq!(odd.len(), 64);
        assert_eq!(DeviceId::from_fingerprint_hex(&odd), None);
    }

    #[test]
    fn public_key_from_cert_matches_key_pair() {
        let (cert, _, public_key) = host_cert();
        assert_eq!(public_key_from_cert(&cert), Some(public_key.as_slice()));
        assert_eq!(public_key_from_cert(b"not a certificate"), None);
        assert_eq!(public_key_from_cert(&cert[..cert.len() / 2]), None);
    }

    #[test]
    fn registration_signature_verifies_and_rejects_tampered_id_or_challenge() {
        let (cert, pkcs8, _) = host_cert();
        let id = DeviceId::from_cert(&cert);
        let challenge = [7u8; 16];
        let signature = sign_registration(&pkcs8, &id, &challenge).unwrap();
        assert!(verify_registration(&cert, &id, &challenge, &signature));

        assert!(!verify_registration(&cert, &id, &[8u8; 16], &signature));
        let other_id = DeviceId([1; 8]);
        let signed_other = sign_registration(&pkcs8, &other_id, &challenge).unwrap();
        assert!(
            !verify_registration(&cert, &other_id, &challenge, &signed_other),
            "an ID must come from the certificate"
        );
        let mut tampered = signature.clone();
        tampered[10] ^= 1;
        assert!(!verify_registration(&cert, &id, &challenge, &tampered));

        let (stranger, stranger_key, _) = host_cert();
        let forged = sign_registration(&stranger_key, &id, &challenge).unwrap();
        assert!(!verify_registration(&cert, &id, &challenge, &forged));
        assert!(!verify_registration(&stranger, &id, &challenge, &signature));
    }

    #[test]
    fn hello_is_never_smaller_than_its_answer() {
        let hello = encode(&ToServer::hello([1; 8]));
        for reflexive in ["203.0.113.5:40000", "[2001:db8::1234:5678:9abc:def0]:65535"] {
            let answer = encode(&FromServer::Challenge {
                nonce: [1; 8],
                challenge: [0xFF; 16],
                reflexive: reflexive.parse().unwrap(),
            });
            assert!(
                hello.len() >= answer.len(),
                "{} < {}",
                hello.len(),
                answer.len()
            );
        }
    }

    #[test]
    fn encode_adds_magic_and_version_and_rejects_other_versions() {
        let hello = ToServer::hello([3; 8]);
        let datagram = encode(&hello);
        assert!(datagram.starts_with(&MAGIC));
        assert_eq!(datagram[4], VERSION);
        assert!(is_signal(&datagram));
        assert_eq!(decode::<ToServer>(&datagram), Some(hello));

        let mut newer = datagram.clone();
        newer[4] = VERSION + 1;
        assert_eq!(decode::<ToServer>(&newer), None);
        let mut trailing = datagram.clone();
        trailing.push(0);
        assert_eq!(
            decode::<ToServer>(&trailing),
            None,
            "no bytes after the message"
        );
        assert_eq!(decode::<ToServer>(&datagram[..4]), None);
        assert_eq!(decode::<ToServer>(b"\x00TDP punch"), None);

        // A real registration fits in one datagram.
        let (cert, pkcs8, _) = host_cert();
        let id = DeviceId::from_cert(&cert);
        let register = ToServer::Register {
            device_id: id,
            signature: sign_registration(&pkcs8, &id, &[1; 16]).unwrap(),
            challenge: [1; 16],
            cert_der: cert,
        };
        let datagram = encode(&register);
        assert!(datagram.len() <= MAX_DATAGRAM, "{} bytes", datagram.len());
        assert_eq!(decode::<ToServer>(&datagram), Some(register));

        let reply = FromServer::Introduced {
            nonce: [1; 8],
            session: [2; 8],
            peer: "203.0.113.5:40000".parse::<SocketAddr>().unwrap(),
        };
        assert_eq!(decode::<FromServer>(&encode(&reply)), Some(reply));
        let oversized = [MAGIC.as_slice(), &[VERSION], &[0; MAX_DATAGRAM]].concat();
        assert_eq!(decode::<ToServer>(&oversized), None);
    }
}
