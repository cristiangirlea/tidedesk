//! Host identity (a persistent self-signed certificate) and viewer-side pinning.
//!
//! There is no certificate authority: the host's certificate is identified by
//! its SHA-256 fingerprint, which the host prints at start-up. The viewer
//! remembers the fingerprint per address on first connect (trust on first use)
//! and refuses to connect if it ever changes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};

pub struct HostIdentity {
    pub cert: CertificateDer<'static>,
    pub key: PrivateKeyDer<'static>,
}

impl HostIdentity {
    /// Loads the identity from `dir`, generating and saving one on first run.
    pub fn load_or_create(dir: &Path) -> Result<Self> {
        let cert_path = dir.join("host-cert.der");
        let key_path = dir.join("host-key.der");
        if cert_path.exists() && key_path.exists() {
            let cert = std::fs::read(&cert_path).context("reading host certificate")?;
            let key = std::fs::read(&key_path).context("reading host key")?;
            return Ok(Self {
                cert: CertificateDer::from(cert),
                key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
            });
        }
        let generated = rcgen::generate_simple_self_signed(vec!["tidedesk-host".to_string()])
            .context("generating host certificate")?;
        let cert = generated.cert.der().to_vec();
        let key = generated.signing_key.serialize_der();
        std::fs::write(&cert_path, &cert).context("saving host certificate")?;
        std::fs::write(&key_path, &key).context("saving host key")?;
        Ok(Self {
            cert: CertificateDer::from(cert),
            key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
        })
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(&self.cert)
    }
}

/// SHA-256 of the DER certificate, as colon-free uppercase hex in groups of 4
/// so people can read it aloud: `3F2A 91C0 ...`.
pub fn fingerprint(cert: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, cert);
    let hex: String = digest.as_ref().iter().map(|b| format!("{b:02X}")).collect();
    hex.as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).unwrap())
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn normalize_fingerprint(fp: &str) -> String {
    fp.chars()
        .filter(|c| c.is_ascii_hexdigit())
        .map(|c| c.to_ascii_uppercase())
        .collect()
}

/// What the viewer knows about a host address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PinStatus {
    Trusted,
    Unknown,
    Mismatch { pinned: String },
}

/// `known_hosts.txt`: one `address fingerprint` pair per line.
pub struct KnownHosts {
    path: PathBuf,
    entries: BTreeMap<String, String>,
}

impl KnownHosts {
    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join("known_hosts.txt");
        let mut entries = BTreeMap::new();
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines().map(str::trim) {
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                if let Some((addr, fp)) = line.split_once(char::is_whitespace) {
                    entries.insert(addr.to_string(), normalize_fingerprint(fp));
                }
            }
        }
        Ok(Self { path, entries })
    }

    pub fn check(&self, addr: &str, fp: &str) -> PinStatus {
        match self.entries.get(addr) {
            None => PinStatus::Unknown,
            Some(p) if *p == normalize_fingerprint(fp) => PinStatus::Trusted,
            Some(p) => PinStatus::Mismatch { pinned: p.clone() },
        }
    }

    /// Whether this fingerprint is pinned under any address.
    pub fn is_trusted_fingerprint(&self, fp: &str) -> bool {
        let fp = normalize_fingerprint(fp);
        self.entries.values().any(|pinned| *pinned == fp)
    }

    /// Like [`KnownHosts::check`], but a fingerprint pinned under any
    /// address is trusted: for keys that change while the host does not,
    /// such as a home router's public address and port.
    pub fn check_fingerprint_first(&self, addr: &str, fp: &str) -> PinStatus {
        if self.is_trusted_fingerprint(fp) {
            PinStatus::Trusted
        } else {
            self.check(addr, fp)
        }
    }

    /// Addresses of hosts connected to before, in sorted order.
    pub fn addresses(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    pub fn pin(&mut self, addr: &str, fp: &str) -> Result<()> {
        if addr.contains(char::is_whitespace) {
            bail!("address may not contain whitespace");
        }
        self.entries
            .insert(addr.to_string(), normalize_fingerprint(fp));
        let mut text = String::from("# TideDesk pinned host fingerprints: <address> <sha256>\n");
        for (a, f) in &self.entries {
            text.push_str(&format!("{a} {f}\n"));
        }
        std::fs::write(&self.path, text).context("saving known_hosts.txt")
    }
}

/// A host identity in a temporary directory, for tests elsewhere in the crate.
#[cfg(test)]
pub(crate) fn test_identity(name: &str) -> HostIdentity {
    let dir = std::env::temp_dir().join(format!("tidedesk-test-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    HostIdentity::load_or_create(&dir).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tidedesk-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn fingerprint_trusted_under_another_address_is_trusted() {
        let mut known = KnownHosts::load(&temp_dir("fp-first")).unwrap();
        known.pin("203.0.113.5:40000", "AAAA 1111").unwrap();
        assert!(known.is_trusted_fingerprint("aaaa1111"));
        assert_eq!(
            known.check_fingerprint_first("203.0.113.5:40999", "AAAA 1111"),
            PinStatus::Trusted
        );
        // The exact check still sees a new address as unknown.
        assert_eq!(
            known.check("203.0.113.5:40999", "AAAA 1111"),
            PinStatus::Unknown
        );
    }

    #[test]
    fn mismatch_still_reported_when_fingerprint_is_unknown() {
        let mut known = KnownHosts::load(&temp_dir("fp-mismatch")).unwrap();
        known.pin("203.0.113.5:40000", "AAAA 1111").unwrap();
        assert_eq!(
            known.check_fingerprint_first("203.0.113.5:40000", "BBBB 2222"),
            PinStatus::Mismatch {
                pinned: "AAAA1111".into()
            }
        );
        assert_eq!(
            known.check_fingerprint_first("198.51.100.1:47800", "BBBB 2222"),
            PinStatus::Unknown
        );
        assert!(!known.is_trusted_fingerprint("BBBB 2222"));
    }

    #[test]
    fn repinning_a_new_key_keeps_the_old_one() {
        let dir = temp_dir("fp-repin");
        let mut known = KnownHosts::load(&dir).unwrap();
        known.pin("203.0.113.5:40000", "AAAA 1111").unwrap();
        known.pin("203.0.113.5:40999", "AAAA 1111").unwrap();
        let reloaded = KnownHosts::load(&dir).unwrap();
        assert_eq!(
            reloaded.check("203.0.113.5:40000", "AAAA1111"),
            PinStatus::Trusted
        );
        assert_eq!(
            reloaded.check("203.0.113.5:40999", "AAAA1111"),
            PinStatus::Trusted
        );
    }

    #[test]
    fn identity_persists_across_loads() {
        let dir = temp_dir("identity");
        let a = HostIdentity::load_or_create(&dir).unwrap();
        let b = HostIdentity::load_or_create(&dir).unwrap();
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert_eq!(normalize_fingerprint(&a.fingerprint()).len(), 64);
    }

    #[test]
    fn known_hosts_pins_and_detects_mismatch() {
        let dir = temp_dir("known-hosts");
        let mut kh = KnownHosts::load(&dir).unwrap();
        assert_eq!(kh.check("10.0.0.2:47800", "AB CD"), PinStatus::Unknown);
        kh.pin("10.0.0.2:47800", "abcd").unwrap();

        let kh = KnownHosts::load(&dir).unwrap();
        assert_eq!(kh.check("10.0.0.2:47800", "AB CD"), PinStatus::Trusted);
        assert!(matches!(
            kh.check("10.0.0.2:47800", "FFFF"),
            PinStatus::Mismatch { .. }
        ));
    }
}
