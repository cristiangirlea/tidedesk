//! The viewer's address book (`computers.toml` in the config directory).
//!
//! Access codes are optional. When remembered they are encrypted with the
//! Windows Data Protection API, so only the same Windows account on the same
//! machine can read them back; the file alone reveals nothing.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tidedesk_core::paths;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Computer {
    pub name: String,
    pub address: String,
    #[serde(default = "yes")]
    pub sound: bool,
    /// Reached over the internet through a path the host opens.
    #[serde(default, skip_serializing_if = "is_false")]
    pub internet: bool,
    /// DPAPI-encrypted access code, hex encoded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    protected_code: Option<String>,
}

fn yes() -> bool {
    true
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl Computer {
    pub fn new(name: String, address: String, sound: bool) -> Self {
        Self {
            name,
            address,
            sound,
            internet: false,
            protected_code: None,
        }
    }

    pub fn has_code(&self) -> bool {
        self.protected_code.is_some()
    }

    pub fn code(&self) -> Option<String> {
        let bytes = from_hex(self.protected_code.as_deref()?)?;
        String::from_utf8(unprotect(&bytes)?).ok()
    }

    /// Remembers `code` (encrypted), or forgets it when `None` or empty.
    pub fn set_code(&mut self, code: Option<&str>) -> Result<()> {
        self.protected_code = match code.map(str::trim).filter(|c| !c.is_empty()) {
            Some(c) => Some(to_hex(&protect(c.as_bytes())?)),
            None => None,
        };
        Ok(())
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct AddressBook {
    #[serde(default, rename = "computer")]
    pub computers: Vec<Computer>,
}

impl AddressBook {
    fn path() -> Result<PathBuf> {
        Ok(paths::config_dir()?.join("computers.toml"))
    }

    pub fn load() -> Self {
        Self::path()
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<()> {
        let text = toml::to_string_pretty(self)?;
        std::fs::write(Self::path()?, text).context("saving computers.toml")
    }

    pub fn find_by_address(&self, address: &str) -> Option<usize> {
        self.computers
            .iter()
            .position(|c| same_address(&c.address, address))
    }
}

/// Compares addresses ignoring case and the default port; device IDs in
/// any spelling.
pub fn same_address(a: &str, b: &str) -> bool {
    use crate::connect::parse_device_id;
    if let (Some(a), Some(b)) = (parse_device_id(a), parse_device_id(b)) {
        return a == b;
    }
    let norm = |s: &str| {
        let s = s.trim().to_lowercase();
        s.strip_suffix(&format!(":{}", tidedesk_core::DEFAULT_PORT))
            .map(str::to_string)
            .unwrap_or(s)
    };
    norm(a) == norm(b)
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

#[cfg(windows)]
fn protect(data: &[u8]) -> Result<Vec<u8>> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData,
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptProtectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    }
    .context("encrypting the access code")?;
    let bytes =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe { LocalFree(Some(HLOCAL(output.pbData.cast()))) };
    Ok(bytes)
}

#[cfg(windows)]
fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData,
    };
    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_ptr() as *mut u8,
    };
    let mut output = CRYPT_INTEGER_BLOB::default();
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            None,
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut output,
        )
    }
    .ok()?;
    let bytes =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe { LocalFree(Some(HLOCAL(output.pbData.cast()))) };
    Some(bytes)
}

#[cfg(not(windows))]
fn protect(_data: &[u8]) -> Result<Vec<u8>> {
    anyhow::bail!("remembering access codes is not supported on this platform yet")
}

#[cfg(not(windows))]
fn unprotect(_data: &[u8]) -> Option<Vec<u8>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_compare_without_default_port() {
        assert!(same_address("My-PC", "my-pc:47800"));
        assert!(!same_address("my-pc:5000", "my-pc"));
    }

    #[test]
    fn same_address_still_ignores_default_port() {
        assert!(same_address("203.0.113.5:40000", " 203.0.113.5:40000"));
        assert!(same_address("203.0.113.5", "203.0.113.5:47800"));
        assert!(!same_address("203.0.113.5:40000", "203.0.113.5:40001"));
    }

    #[test]
    fn same_address_treats_device_ids_canonically() {
        assert!(same_address(
            "TD-1A2B-3C4D-5E6F-7A8B",
            "td 1a2b 3c4d 5e6f 7a8b"
        ));
        assert!(!same_address(
            "TD-1A2B-3C4D-5E6F-7A8B",
            "TD-1A2B-3C4D-5E6F-7A8C"
        ));
        assert!(same_address("my-pc", "MY-PC:47800"), "host names as before");
    }

    #[test]
    fn computers_round_trip_keeps_internet_flag_default_false() {
        // Saved before the flag existed.
        let old: AddressBook = toml::from_str(
            "[[computer]]
name = \"Office\"
address = \"10.0.0.5\"
",
        )
        .unwrap();
        assert!(!old.computers[0].internet);

        let mut far = Computer::new("Mum".into(), "203.0.113.5:40000".into(), true);
        far.internet = true;
        let book = AddressBook {
            computers: vec![far, old.computers[0].clone()],
        };
        let text = toml::to_string_pretty(&book).unwrap();
        assert_eq!(
            text.matches("internet").count(),
            1,
            "false is not written:
{text}"
        );
        let back: AddressBook = toml::from_str(&text).unwrap();
        assert_eq!(back.computers, book.computers);
    }

    #[test]
    fn book_round_trips_and_code_stays_encrypted() {
        let mut pc = Computer::new("Office".into(), "10.0.0.5".into(), false);
        pc.set_code(Some("K7QM-3XPA-WZ")).unwrap();
        let book = AddressBook {
            computers: vec![pc],
        };
        let text = toml::to_string_pretty(&book).unwrap();
        assert!(
            !text.contains("K7QM"),
            "code must not be stored in clear text"
        );

        let back: AddressBook = toml::from_str(&text).unwrap();
        assert_eq!(back.computers[0].code().as_deref(), Some("K7QM-3XPA-WZ"));
        assert!(!back.computers[0].sound);
    }
}
