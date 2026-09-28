//! A host's local addresses, sealed for the viewers that know its access
//! code.
//!
//! When a viewer and a host share one internet address, a path punched
//! through their router usually fails (few routers loop traffic back to
//! themselves) and a broadcast query does not cross network segments, so
//! the viewer needs the host's local addresses. The host puts them, sealed,
//! in its registration; the connection service passes them, unread, to a
//! viewer at the host's internet address; a viewer with the access code
//! opens them and tries them (see `tidedesk_view::connect`).
//!
//! The seal is ChaCha20-Poly1305 under a key derived from the access code
//! with PBKDF2, salted and bound to the device ID. What the service, and
//! anyone at the host's internet address who asks for its ID, sees is the
//! sealed bytes: guessing the code from them costs a key derivation per
//! guess ([`KDF_ITERATIONS`]), which keeps a 10-character code out of reach.
//! How the code itself is checked does not change (see [`crate::auth`]).
//! Only local-network addresses ([`super::check_public`]) go in or come out.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::num::NonZeroU32;

use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use ring::pbkdf2;
use tidedesk_rendezvous_proto::{DeviceId, MAX_CANDIDATES_LEN};

use super::{NotPublic, check_public, random_bytes};
use crate::auth::normalize_code;

/// Addresses a host seals: its real network adapters come first, so a few
/// are enough, and four keep the seal well inside [`MAX_CANDIDATES_LEN`].
pub const MAX_ADDRESSES: usize = 4;

/// Labels the key derivation and the seal, so neither means anything
/// elsewhere.
const CONTEXT: &[u8] = b"tidedesk local addresses v1";

/// Key derivations per guess of the code (OWASP's 2023 figure for
/// PBKDF2-HMAC-SHA256): tens of milliseconds on a PC, paid once per
/// registration and once per connection.
const KDF_ITERATIONS: NonZeroU32 = NonZeroU32::new(600_000).unwrap();

/// An IPv4 address and a port, big endian.
const ADDRESS_LEN: usize = 6;

/// Seals `addresses` for viewers that know `code`; empty when none of them
/// is a local-network address.
pub fn seal(code: &str, device_id: DeviceId, addresses: &[SocketAddrV4]) -> Vec<u8> {
    let mut plaintext: Vec<u8> = addresses
        .iter()
        .filter(|address| is_local(**address))
        .take(MAX_ADDRESSES)
        .flat_map(|address| {
            let mut bytes = address.ip().octets().to_vec();
            bytes.extend_from_slice(&address.port().to_be_bytes());
            bytes
        })
        .collect();
    if plaintext.is_empty() {
        return Vec::new();
    }
    let nonce: [u8; NONCE_LEN] = random_bytes();
    key_for(code, device_id)
        .seal_in_place_append_tag(
            Nonce::assume_unique_for_key(nonce),
            Aad::from(binding(device_id)),
            &mut plaintext,
        )
        .expect("sealing a few bytes cannot fail");
    let mut sealed = nonce.to_vec();
    sealed.extend_from_slice(&plaintext);
    debug_assert!(sealed.len() <= MAX_CANDIDATES_LEN);
    sealed
}

/// The addresses in `sealed`, if it was sealed for this `device_id` with
/// this `code`: `None` for a wrong code or anything tampered with.
pub fn unseal(code: &str, device_id: DeviceId, sealed: &[u8]) -> Option<Vec<SocketAddrV4>> {
    if sealed.len() > MAX_CANDIDATES_LEN {
        return None;
    }
    let (nonce, ciphertext) = sealed.split_at_checked(NONCE_LEN)?;
    let mut buffer = ciphertext.to_vec();
    let plaintext = key_for(code, device_id)
        .open_in_place(
            Nonce::try_assume_unique_for_key(nonce).ok()?,
            Aad::from(binding(device_id)),
            &mut buffer,
        )
        .ok()?;
    let (entries, rest) = plaintext.as_chunks::<ADDRESS_LEN>();
    if entries.is_empty() || !rest.is_empty() {
        return None;
    }
    let addresses = entries
        .iter()
        .map(|&[a, b, c, d, hi, lo]| {
            SocketAddrV4::new(Ipv4Addr::new(a, b, c, d), u16::from_be_bytes([hi, lo]))
        })
        .filter(|address| is_local(*address))
        .take(MAX_ADDRESSES)
        .collect();
    Some(addresses)
}

/// A local-network address with a port: what a viewer on the same network
/// can dial.
fn is_local(address: SocketAddrV4) -> bool {
    address.port() != 0 && check_public(SocketAddr::V4(address)) == Err(NotPublic::Local)
}

fn key_for(code: &str, device_id: DeviceId) -> LessSafeKey {
    let mut key = [0u8; 32];
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        KDF_ITERATIONS,
        &binding(device_id),
        normalize_code(code).as_bytes(),
        &mut key,
    );
    LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, &key).expect("a 32-byte key"))
}

/// The salt of the key and the associated data of the seal: a seal for one
/// host opens for no other.
fn binding(device_id: DeviceId) -> Vec<u8> {
    [CONTEXT, &device_id.0].concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: DeviceId = DeviceId([0x1A, 0x2B, 0x3C, 0x4D, 0x5E, 0x6F, 0x7A, 0x8B]);
    const CODE: &str = "K7QM-3XPA-WZ";

    fn addr(s: &str) -> SocketAddrV4 {
        s.parse().unwrap()
    }

    #[test]
    fn a_seal_opens_only_with_its_code_and_for_its_host() {
        let addresses = [addr("192.168.1.20:47800"), addr("10.0.0.5:50000")];
        let sealed = seal(CODE, ID, &addresses);
        assert!(sealed.len() <= MAX_CANDIDATES_LEN, "{} bytes", sealed.len());
        assert!(
            !sealed.windows(4).any(|w| w == [192, 168, 1, 20]),
            "addresses are not readable"
        );
        assert_eq!(unseal(CODE, ID, &sealed).as_deref(), Some(&addresses[..]));
        assert_eq!(
            unseal("k7qm 3xpa wz", ID, &sealed).as_deref(),
            Some(&addresses[..]),
            "the code in any spelling"
        );

        assert_eq!(unseal("K7QM-3XPA-WY", ID, &sealed), None, "wrong code");
        assert_eq!(
            unseal(CODE, DeviceId([9; 8]), &sealed),
            None,
            "another host"
        );
        let mut tampered = sealed.clone();
        tampered[NONCE_LEN + 1] ^= 1;
        assert_eq!(unseal(CODE, ID, &tampered), None);
        assert_eq!(unseal(CODE, ID, &sealed[..sealed.len() - 1]), None);
        assert_eq!(unseal(CODE, ID, &[]), None);
        assert_eq!(unseal(CODE, ID, &[0; MAX_CANDIDATES_LEN + 1]), None);

        // A new seal of the same addresses looks different (random nonce).
        assert_ne!(seal(CODE, ID, &addresses), sealed);
    }

    #[test]
    fn only_local_network_addresses_are_sealed_or_opened() {
        let local = addr("172.16.3.4:47800");
        let mixed = [
            addr("203.0.113.5:47800"), // internet
            addr("192.168.1.20:0"),    // no port
            local,
        ];
        let sealed = seal(CODE, ID, &mixed);
        assert_eq!(unseal(CODE, ID, &sealed).as_deref(), Some(&[local][..]));

        assert!(
            seal(CODE, ID, &[addr("203.0.113.5:47800")]).is_empty(),
            "nothing to seal"
        );
        assert!(seal(CODE, ID, &[]).is_empty());
    }

    #[test]
    fn at_most_four_addresses_are_sealed() {
        let many: Vec<SocketAddrV4> = (1..=6)
            .map(|n| addr(&format!("10.0.0.{n}:47800")))
            .collect();
        let sealed = seal(CODE, ID, &many);
        assert!(sealed.len() <= MAX_CANDIDATES_LEN, "{} bytes", sealed.len());
        assert_eq!(
            unseal(CODE, ID, &sealed).as_deref(),
            Some(&many[..MAX_ADDRESSES])
        );
    }
}
