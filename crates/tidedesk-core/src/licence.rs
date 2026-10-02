//! A TideDesk licence: a short signed text block the app checks offline.
//! No account, no server, no usage detection: the licence only says who may
//! use TideDesk for what, until when.
//!
//! ```text
//! -----BEGIN TIDEDESK LICENCE-----
//! licensee = "Ana Pop"
//! email = "ana@example.com"
//! edition = "Pro"
//! features = ["work"]
//! seats = 1
//! issued = "2026-10-02"
//! expires = "2027-10-02"
//! signature = "…"
//! -----END TIDEDESK LICENCE-----
//! ```
//!
//! The signature (ECDSA P-256, SHA-256) covers the lines between the markers
//! other than the signature's, each trimmed, joined with `\n`: line endings
//! and indentation an email client changes do not break it.

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use ring::rand::SystemRandom;
use ring::signature::{
    ECDSA_P256_SHA256_FIXED, ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, UnparsedPublicKey,
};
use serde::{Deserialize, Serialize};

const BEGIN: &str = "-----BEGIN TIDEDESK LICENCE-----";
const END: &str = "-----END TIDEDESK LICENCE-----";

/// Days a licence keeps working after it expired.
pub const GRACE_DAYS: i64 = 14;

/// The public keys licences are signed with (uncompressed P-256 points, hex).
/// More than one so that a key can be replaced without voiding licences.
const KEYS: &[&str] = &[
    // 2026-10-02
    "04eb81c10a9279b55b575cf2c95b48e9231bb0b6201e1ea960df4100c657c03de2f88ba7b17533a1b2574fcf9f4c6584290ccb2f8a6ce5d824c2d7cef7bdfcfb16",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Licence {
    pub licensee: String,
    #[serde(default)]
    pub email: String,
    pub edition: String,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default = "one")]
    pub seats: u32,
    pub issued: String,
    /// None for a licence that does not expire.
    #[serde(default)]
    pub expires: Option<String>,
}

fn one() -> u32 {
    1
}

/// Where a licence stands on a given day.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Standing {
    Active,
    /// Expired, still working until this date.
    Grace {
        until: String,
    },
    Expired,
}

impl Licence {
    pub fn standing(&self, today: &str) -> Standing {
        let (Some(expires), Some(today)) = (
            self.expires.as_deref().and_then(crate::dates::parse),
            crate::dates::parse(today),
        ) else {
            return Standing::Active;
        };
        if today <= expires {
            Standing::Active
        } else if today <= expires + GRACE_DAYS {
            Standing::Grace {
                until: crate::dates::format(expires + GRACE_DAYS),
            }
        } else {
            Standing::Expired
        }
    }

    /// Whether this licence turns `feature` on, on `today`: listed, and not
    /// past its grace.
    pub fn allows(&self, feature: &str, today: &str) -> bool {
        self.standing(today) != Standing::Expired && self.features.iter().any(|f| f == feature)
    }
}

/// Whether the licence this computer holds turns `feature` on today.
pub fn allows(feature: &str) -> bool {
    load().is_some_and(|l| l.allows(feature, &crate::dates::today()))
}

/// The signed lines and the signature of a licence block.
fn split(text: &str) -> Result<(String, Vec<u8>)> {
    let start = text
        .find(BEGIN)
        .context("no licence found: it starts with ".to_string() + BEGIN)?;
    let body = &text[start + BEGIN.len()..];
    let end = body
        .find(END)
        .context("the licence is cut short: its last line is missing")?;
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
        signature.context("the licence has no signature")?,
    ))
}

/// Reads and checks a licence block.
pub fn read(text: &str) -> Result<Licence> {
    let keys: Vec<Vec<u8>> = KEYS.iter().filter_map(|k| from_hex(k).ok()).collect();
    read_with(text, &keys)
}

fn read_with(text: &str, keys: &[Vec<u8>]) -> Result<Licence> {
    let (signed, signature) = split(text)?;
    let genuine = keys.iter().any(|key| {
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, key)
            .verify(signed.as_bytes(), &signature)
            .is_ok()
    });
    if !genuine {
        bail!("this licence was not issued for TideDesk, or it was changed");
    }
    let licence: Licence = toml::from_str(&signed)
        .map_err(|e| anyhow!("the licence's fields are not readable: {e}"))?;
    if crate::dates::parse(&licence.issued).is_none()
        || licence
            .expires
            .as_deref()
            .is_some_and(|d| crate::dates::parse(d).is_none())
    {
        bail!("the licence's dates are not readable");
    }
    Ok(licence)
}

/// Signs `licence` with a PKCS#8 P-256 key: the block to send to its owner.
pub fn sign(licence: &Licence, pkcs8: &[u8]) -> Result<String> {
    let fields = toml::to_string(licence).context("writing the licence")?;
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
        "{BEGIN}\n{signed}\nsignature = \"{}\"\n{END}\n",
        to_hex(signature.as_ref())
    ))
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

/// `licence.txt` in the settings folder.
pub fn path() -> Result<PathBuf> {
    Ok(crate::paths::config_dir()?.join("licence.txt"))
}

/// The licence this computer holds, if one is there and checks out.
pub fn load() -> Option<Licence> {
    static WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    let text = std::fs::read_to_string(path().ok()?).ok()?;
    read(&text)
        .inspect_err(|e| {
            // Once: the licence is looked up again every few seconds.
            if !WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                tracing::warn!("the saved licence is not used: {e:#}");
            }
        })
        .ok()
}

/// Just the licence block of a pasted text, without the email around it.
fn block(text: &str) -> Option<String> {
    let start = text.find(BEGIN)?;
    let end = start + text[start..].find(END)? + END.len();
    let lines: Vec<&str> = text[start..end]
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    Some(lines.join("\n") + "\n")
}

/// Checks a pasted licence and keeps it: what it says.
pub fn add(text: &str) -> Result<Licence> {
    let licence = read(text)?;
    let block = block(text).context("no licence found")?;
    let path = path()?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).context("making the settings folder")?;
    }
    // Written aside first, so that a failed write keeps the licence before.
    let new = path.with_extension("txt.new");
    std::fs::write(&new, block).context("saving the licence")?;
    std::fs::rename(&new, &path).context("saving the licence")?;
    Ok(licence)
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(text: &str) -> Result<Vec<u8>> {
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
    use super::*;

    fn licence() -> Licence {
        Licence {
            licensee: "Ana Pop".into(),
            email: "ana@example.com".into(),
            edition: "Pro".into(),
            features: vec!["work".into()],
            seats: 1,
            issued: "2026-10-02".into(),
            expires: Some("2027-10-02".into()),
        }
    }

    fn key() -> (Vec<u8>, Vec<u8>) {
        let (pkcs8, public) = new_key().unwrap();
        (pkcs8, from_hex(&public).unwrap())
    }

    #[test]
    fn a_signed_licence_reads_back() {
        let (private, public) = key();
        let block = sign(&licence(), &private).unwrap();
        assert!(block.starts_with(BEGIN) && block.trim_end().ends_with(END));
        assert_eq!(
            read_with(&block, std::slice::from_ref(&public)).unwrap(),
            licence()
        );

        // As an email client may pass it on: other line endings, indented,
        // quoted text around it.
        let mangled = format!(
            "Thanks for buying!\r\n\r\n{}\r\nRegards",
            block
                .lines()
                .map(|l| format!("    {l}"))
                .collect::<Vec<_>>()
                .join("\r\n")
        );
        assert_eq!(read_with(&mangled, &[public]).unwrap(), licence());
        // Only the block is kept, not the email around it.
        assert_eq!(super::block(&mangled).unwrap(), block);
    }

    #[test]
    fn a_changed_or_foreign_licence_is_refused() {
        let (private, public) = key();
        let block = sign(&licence(), &private).unwrap();
        let changed = block.replace("2027-10-02", "2037-10-02");
        let why = read_with(&changed, std::slice::from_ref(&public))
            .unwrap_err()
            .to_string();
        assert!(
            why.contains("not issued for TideDesk, or it was changed"),
            "{why}"
        );

        let (_, other) = key();
        assert!(read_with(&block, &[other]).is_err(), "another key");
        assert!(read_with(&block, &[]).is_err(), "no keys at all");

        let unsigned = block
            .lines()
            .filter(|l| !l.starts_with("signature"))
            .collect::<Vec<_>>()
            .join("\n");
        let why = read_with(&unsigned, std::slice::from_ref(&public))
            .unwrap_err()
            .to_string();
        assert!(why.contains("no signature"), "{why}");
        assert!(read_with("hello", &[public]).is_err());
    }

    /// A licence signed with TideDesk's own key, by `examples/licence.rs`,
    /// reads with the keys built into the app.
    #[test]
    fn the_built_in_key_reads_a_real_licence() {
        let real = r#"
        -----BEGIN TIDEDESK LICENCE-----
        licensee = "Test Licensee"
        email = "test@example.com"
        edition = "Pro"
        features = ["work"]
        seats = 1
        issued = "2026-10-02"
        expires = "2027-10-02"
        signature = "fdf9c5126a6a2b0215942825d05f1ae9469c5e8425cec8f39ad63dd3e1fa1f718c6a4eaba343b8fa226e8557bc35d0e8d80f010509a24eaa7ab09335f53160a2"
        -----END TIDEDESK LICENCE-----
"#;
        let licence = read(real).unwrap();
        assert_eq!(licence.licensee, "Test Licensee");
        assert_eq!(licence.edition, "Pro");
        assert_eq!(licence.expires.as_deref(), Some("2027-10-02"));
        assert!(read(&real.replace("Test Licensee", "Someone Else")).is_err());
    }

    #[test]
    fn an_expired_licence_has_two_weeks_of_grace() {
        let licence = licence();
        assert_eq!(licence.standing("2027-10-02"), Standing::Active);
        assert_eq!(
            licence.standing("2027-10-03"),
            Standing::Grace {
                until: "2027-10-16".into()
            }
        );
        assert_eq!(
            licence.standing("2027-10-16"),
            Standing::Grace {
                until: "2027-10-16".into()
            }
        );
        assert_eq!(licence.standing("2027-10-17"), Standing::Expired);
        let forever = Licence {
            expires: None,
            ..licence
        };
        assert_eq!(forever.standing("2099-01-01"), Standing::Active);
    }

    #[test]
    fn a_licence_allows_its_features_until_its_grace_ends() {
        let licence = licence();
        assert!(licence.allows("work", "2027-10-02"));
        assert!(licence.allows("work", "2027-10-16"), "in grace");
        assert!(!licence.allows("work", "2027-10-17"), "expired");
        assert!(!licence.allows("session-log", "2027-10-02"), "not listed");
    }
}
