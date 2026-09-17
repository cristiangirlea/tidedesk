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
#[derive(Debug, PartialEq, Eq)]
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
