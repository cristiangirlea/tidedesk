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
    /// The private key is kept sealed for this Windows account
    /// (`host-key.sealed`, see [`crate::secret`]); a key saved plain by an
    /// earlier release (`host-key.der`) is sealed in place, keeping the
    /// identity. Where secrets cannot be sealed, the key stays a plain file.
    pub fn load_or_create(dir: &Path) -> Result<Self> {
        let cert_path = dir.join("host-cert.der");
        let sealed_path = dir.join("host-key.sealed");
        let plain_path = dir.join("host-key.der");
        if let Ok(cert) = std::fs::read(&cert_path) {
            if let Ok(sealed) = std::fs::read(&sealed_path) {
                if let Some(key) = crate::secret::unprotect(&sealed) {
                    return Ok(Self::from_der(cert, key));
                }
                // Sealed for another Windows account, or copied from another
                // computer: this account cannot use it. A new identity, with
                // the old one kept aside rather than overwritten.
                tracing::warn!(
                    "this Windows account cannot open the saved host key: making a new identity                      (a new device ID and fingerprint); the old files are kept as *.unreadable"
                );
                let _ = std::fs::rename(&sealed_path, dir.join("host-key.sealed.unreadable"));
                let _ = std::fs::rename(&cert_path, dir.join("host-cert.der.unreadable"));
            } else if let Ok(key) = std::fs::read(&plain_path) {
                match crate::secret::protect(&key) {
                    Ok(sealed) => match std::fs::write(&sealed_path, sealed) {
                        Ok(()) => {
                            if let Err(e) = std::fs::remove_file(&plain_path) {
                                tracing::warn!("could not remove the plain host key: {e}");
                            }
                        }
                        Err(e) => tracing::warn!("could not seal the host key: {e}"),
                    },
                    // Nothing to seal it with here: it stays as it was.
                    Err(e) => tracing::debug!("host key stays plain: {e:#}"),
                }
                return Ok(Self::from_der(cert, key));
            }
        }
        let generated = rcgen::generate_simple_self_signed(vec!["tidedesk-host".to_string()])
            .context("generating host certificate")?;
        let cert = generated.cert.der().to_vec();
        let key = generated.signing_key.serialize_der();
        match crate::secret::protect(&key) {
            Ok(sealed) => std::fs::write(&sealed_path, sealed).context("saving host key")?,
            Err(_) => std::fs::write(&plain_path, &key).context("saving host key")?,
        }
        std::fs::write(&cert_path, &cert).context("saving host certificate")?;
        Ok(Self::from_der(cert, key))
    }

    fn from_der(cert: Vec<u8>, key: Vec<u8>) -> Self {
        Self {
            cert: CertificateDer::from(cert),
            key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
        }
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(&self.cert)
    }

    /// The host's rendezvous name, derived from its certificate.
    pub fn device_id(&self) -> tidedesk_rendezvous_proto::DeviceId {
        tidedesk_rendezvous_proto::DeviceId::from_cert(&self.cert)
    }

    /// What registering with a rendezvous service needs, without sealed
    /// local addresses (the host adds those; see `nat::candidates`).
    /// Shared, so the private key is copied once however many users it has.
    pub fn rendezvous_credentials(&self) -> std::sync::Arc<crate::nat::signal::Credentials> {
        std::sync::Arc::new(crate::nat::signal::Credentials {
            device_id: self.device_id(),
            cert_der: self.cert.to_vec(),
            pkcs8: self.pkcs8().to_vec(),
            candidates: Vec::new(),
        })
    }

    /// The private key as PKCS#8, for signing rendezvous registrations.
    pub fn pkcs8(&self) -> &[u8] {
        match &self.key {
            PrivateKeyDer::Pkcs8(key) => key.secret_pkcs8_der(),
            // Identities are always created and saved as PKCS#8.
            _ => &[],
        }
    }
}

/// SHA-256 of the DER certificate, as colon-free uppercase hex in groups of 4
/// so people can read it aloud: `3F2A 91C0 ...`.
pub fn fingerprint(cert: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, cert);
    let hex: String = digest.as_ref().iter().map(|b| format!("{b:02X}")).collect();
    format_fingerprint(&hex)
}

/// A fingerprint in any spelling, as uppercase hex in groups of four.
pub fn format_fingerprint(fp: &str) -> String {
    let hex = normalize_fingerprint(fp);
    hex.as_bytes()
        .chunks(4)
        .map(|c| std::str::from_utf8(c).expect("hex digits are ASCII"))
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
/// This viewer's own identity: a self-signed certificate it shows hosts, so
/// that a host that trusts it lets it in without the access code. Its key
/// is sealed for this Windows account.
pub struct ViewerIdentity {
    pub cert: CertificateDer<'static>,
    pub key: PrivateKeyDer<'static>,
}

impl ViewerIdentity {
    /// Loads the identity from `dir`, making and saving one on first run.
    pub fn load_or_create(dir: &Path) -> Result<Self> {
        let cert_path = dir.join("viewer-cert.der");
        let key_path = dir.join("viewer-key.sealed");
        if let (Ok(cert), Ok(sealed)) = (std::fs::read(&cert_path), std::fs::read(&key_path))
            && let Some(key) = crate::secret::unprotect(&sealed)
        {
            return Ok(Self {
                cert: CertificateDer::from(cert),
                key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
            });
        }
        let generated = rcgen::generate_simple_self_signed(vec!["tidedesk-viewer".to_string()])
            .context("making the viewer's certificate")?;
        let cert = generated.cert.der().to_vec();
        let key = generated.signing_key.serialize_der();
        std::fs::write(&key_path, crate::secret::protect(&key)?)
            .context("saving the viewer's key")?;
        std::fs::write(&cert_path, &cert).context("saving the viewer's certificate")?;
        Ok(Self {
            cert: CertificateDer::from(cert),
            key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key)),
        })
    }

    /// What a host lists it by.
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.cert)
    }
}

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
    fn device_id_derives_from_fingerprint() {
        let identity = HostIdentity::load_or_create(&temp_dir("device-id")).unwrap();
        let id = identity.device_id();
        assert_eq!(
            tidedesk_rendezvous_proto::DeviceId::from_fingerprint_hex(&identity.fingerprint()),
            Some(id)
        );
        assert!(id.to_string().starts_with("TD-"));
    }

    #[test]
    fn sign_registration_verifies_with_proto() {
        use tidedesk_rendezvous_proto::{sign_registration, verify_registration};
        let identity = HostIdentity::load_or_create(&temp_dir("registration")).unwrap();
        let challenge = [5u8; 16];
        let signature =
            sign_registration(identity.pkcs8(), &identity.device_id(), &challenge).unwrap();
        assert!(verify_registration(
            &identity.cert,
            &identity.device_id(),
            &challenge,
            &signature
        ));
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

    #[cfg(windows)]
    #[test]
    fn a_new_host_key_is_kept_sealed_and_loads_again() {
        let dir = temp_dir("host-sealed");
        let first = HostIdentity::load_or_create(&dir).unwrap();
        assert!(dir.join("host-key.sealed").exists());
        assert!(!dir.join("host-key.der").exists(), "never written plain");
        let sealed = std::fs::read(dir.join("host-key.sealed")).unwrap();
        assert!(
            !sealed
                .windows(16)
                .any(|w| first.pkcs8().windows(16).any(|k| k == w)),
            "the key cannot be read from the file"
        );
        let again = HostIdentity::load_or_create(&dir).unwrap();
        assert_eq!(again.fingerprint(), first.fingerprint());
        assert_eq!(again.pkcs8(), first.pkcs8());
    }

    #[cfg(windows)]
    #[test]
    fn a_plain_host_key_from_before_is_sealed_keeping_the_identity() {
        let dir = temp_dir("host-plain");
        let generated = rcgen::generate_simple_self_signed(vec!["tidedesk-host".into()]).unwrap();
        std::fs::write(dir.join("host-cert.der"), generated.cert.der()).unwrap();
        std::fs::write(
            dir.join("host-key.der"),
            generated.signing_key.serialize_der(),
        )
        .unwrap();
        let expected = fingerprint(generated.cert.der());

        let loaded = HostIdentity::load_or_create(&dir).unwrap();
        assert_eq!(loaded.fingerprint(), expected, "the same identity");
        assert!(dir.join("host-key.sealed").exists());
        assert!(!dir.join("host-key.der").exists(), "the plain key is gone");
        let again = HostIdentity::load_or_create(&dir).unwrap();
        assert_eq!(again.fingerprint(), expected);
    }

    #[cfg(windows)]
    #[test]
    fn a_key_this_account_cannot_open_makes_a_new_identity_and_keeps_the_old() {
        let dir = temp_dir("host-unreadable");
        let first = HostIdentity::load_or_create(&dir).unwrap();
        std::fs::write(dir.join("host-key.sealed"), b"sealed for someone else").unwrap();
        let second = HostIdentity::load_or_create(&dir).unwrap();
        assert_ne!(second.fingerprint(), first.fingerprint());
        assert!(dir.join("host-key.sealed.unreadable").exists());
        assert!(dir.join("host-cert.der.unreadable").exists());
        let again = HostIdentity::load_or_create(&dir).unwrap();
        assert_eq!(
            again.fingerprint(),
            second.fingerprint(),
            "and keeps the new one"
        );
    }
}
