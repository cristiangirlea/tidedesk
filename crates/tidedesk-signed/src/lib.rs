//! Signed text blocks and calendar dates, shared by TideDesk's programs.
//!
//! A signed block is TOML between two marker lines, with a `signature` line:
//!
//! ```text
//! -----BEGIN TIDEDESK LICENCE-----
//! licensee = "Ana Pop"
//! issued = "2026-10-02"
//! signature = "…"
//! -----END TIDEDESK LICENCE-----
//! ```
//!
//! The signature (ECDSA P-256, SHA-256) covers the lines between the markers
//! other than the signature's, each trimmed, joined with `\n`: line endings
//! and indentation an email client changes do not break it. Each kind of
//! block has its own markers, so one cannot be read as another.

pub mod dates;

use anyhow::{Context, Result, anyhow, bail};
use ring::rand::SystemRandom;
use ring::signature::{
    ECDSA_P256_SHA256_FIXED, ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, UnparsedPublicKey,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// TideDesk's public signing keys (uncompressed P-256 points, hex). More than
/// one so that a key can be replaced without voiding what it signed.
pub const KEYS: &[&str] = &[
    // 2026-10-02
    "04eb81c10a9279b55b575cf2c95b48e9231bb0b6201e1ea960df4100c657c03de2f88ba7b17533a1b2574fcf9f4c6584290ccb2f8a6ce5d824c2d7cef7bdfcfb16",
];

/// [`KEYS`] as bytes.
pub fn keys() -> Vec<Vec<u8>> {
    KEYS.iter().filter_map(|k| from_hex(k).ok()).collect()
}

/// What kind of block: its marker lines, and its name in messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kind {
    pub begin: &'static str,
    pub end: &'static str,
    /// "licence", for "this licence was not issued for TideDesk".
    pub what: &'static str,
}

/// The signed lines and the signature of a block.
fn split(text: &str, kind: Kind) -> Result<(String, Vec<u8>)> {
    let start = text
        .find(kind.begin)
        .with_context(|| format!("no {} found: it starts with {}", kind.what, kind.begin))?;
    let body = &text[start + kind.begin.len()..];
    let end = body
        .find(kind.end)
        .with_context(|| format!("the {} is cut short: its last line is missing", kind.what))?;
    let mut signed = Vec::new();
    let mut signature = None;
    for line in body[..end].lines().map(str::trim).filter(|l| !l.is_empty()) {
        match line.strip_prefix("signature") {
            Some(rest) if rest.trim_start().starts_with('=') => {
                let value = rest.trim_start()[1..].trim().trim_matches('"');
                signature = Some(from_hex(value).context("the signature is not readable")?);
            }
            _ => signed.push(line),
        }
    }
    Ok((
        signed.join("\n"),
        signature.with_context(|| format!("the {} has no signature", kind.what))?,
    ))
}

/// Reads a block of `kind` signed with one of `keys`.
pub fn read<T: DeserializeOwned>(text: &str, kind: Kind, keys: &[Vec<u8>]) -> Result<T> {
    let (signed, signature) = split(text, kind)?;
    let genuine = keys.iter().any(|key| {
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, key)
            .verify(signed.as_bytes(), &signature)
            .is_ok()
    });
    if !genuine {
        bail!(
            "this {} was not issued for TideDesk, or it was changed",
            kind.what
        );
    }
    toml::from_str(&signed).map_err(|e| anyhow!("the {}'s fields are not readable: {e}", kind.what))
}

/// Signs `value` with a PKCS#8 P-256 key: the block of `kind` to hand out.
pub fn sign<T: Serialize>(value: &T, kind: Kind, pkcs8: &[u8]) -> Result<String> {
    let fields = toml::to_string(value).with_context(|| format!("writing the {}", kind.what))?;
    let signed: Vec<&str> = fields
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let signed = signed.join("\n");
    let rng = SystemRandom::new();
    let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8, &rng)
        .map_err(|_| anyhow!("not a P-256 signing key"))?;
    let signature = key
        .sign(&rng, signed.as_bytes())
        .map_err(|_| anyhow!("signing failed"))?;
    Ok(format!(
        "{}\n{signed}\nsignature = \"{}\"\n{}\n",
        kind.begin,
        to_hex(signature.as_ref()),
        kind.end
    ))
}

/// Just the block of `kind` in a pasted text, without what surrounds it.
pub fn block(text: &str, kind: Kind) -> Option<String> {
    let start = text.find(kind.begin)?;
    let end = start + text[start..].find(kind.end)? + kind.end.len();
    let lines: Vec<&str> = text[start..end]
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    Some(lines.join("\n") + "\n")
}

/// A new signing key: (PKCS#8 private key, public key as hex for [`KEYS`]).
pub fn new_key() -> Result<(Vec<u8>, String)> {
    use ring::signature::KeyPair;
    let rng = SystemRandom::new();
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
        .map_err(|_| anyhow!("making a key failed"))?;
    let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, pkcs8.as_ref(), &rng)
        .map_err(|_| anyhow!("reading the new key failed"))?;
    Ok((pkcs8.as_ref().to_vec(), to_hex(key.public_key().as_ref())))
}

pub fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn from_hex(text: &str) -> Result<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        bail!("odd length");
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).map_err(|e| anyhow!("{e}")))
        .collect()
}

#[cfg(test)]
mod tests {
    use serde::Deserialize;

    use super::*;

    const NOTE: Kind = Kind {
        begin: "-----BEGIN TIDEDESK NOTE-----",
        end: "-----END TIDEDESK NOTE-----",
        what: "note",
    };
    const OTHER: Kind = Kind {
        begin: "-----BEGIN TIDEDESK OTHER-----",
        end: "-----END TIDEDESK OTHER-----",
        what: "other",
    };

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Note {
        to: String,
        count: u32,
    }

    #[test]
    fn a_block_of_any_kind_signs_and_reads_back_and_only_as_its_kind() {
        let (private, public) = new_key().unwrap();
        let public = vec![from_hex(&public).unwrap()];
        let note = Note {
            to: "Ana \"the\" Pop \\ x".into(),
            count: 3,
        };
        let signed = sign(&note, NOTE, &private).unwrap();
        assert!(signed.starts_with(NOTE.begin));
        assert_eq!(read::<Note>(&signed, NOTE, &public).unwrap(), note);
        let why = read::<Note>(&signed, OTHER, &public)
            .unwrap_err()
            .to_string();
        assert!(why.contains("no other found"), "{why}");
        let changed = signed.replace("count = 3", "count = 4");
        let why = read::<Note>(&changed, NOTE, &public)
            .unwrap_err()
            .to_string();
        assert!(why.contains("note was not issued for TideDesk"), "{why}");
        assert_eq!(block(&format!("hi\n  {signed}bye"), NOTE).unwrap(), signed);
    }

    #[test]
    fn the_built_in_keys_are_readable() {
        assert_eq!(keys().len(), KEYS.len());
        assert!(keys().iter().all(|k| k.len() == 65 && k[0] == 4));
    }
}
