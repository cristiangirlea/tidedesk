//! Secrets kept on disk for this Windows account only (DPAPI): another
//! account, or the file copied to another computer, cannot read them.

#[cfg(not(windows))]
use anyhow::Result;
#[cfg(windows)]
use anyhow::{Context, Result};

#[cfg(windows)]
pub fn protect(data: &[u8]) -> Result<Vec<u8>> {
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
    .context("encrypting for this Windows account")?;
    let bytes =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    unsafe { LocalFree(Some(HLOCAL(output.pbData.cast()))) };
    Ok(bytes)
}

#[cfg(windows)]
pub fn unprotect(data: &[u8]) -> Option<Vec<u8>> {
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
pub fn protect(_data: &[u8]) -> Result<Vec<u8>> {
    anyhow::bail!("keeping secrets is not supported on this platform yet")
}

#[cfg(not(windows))]
pub fn unprotect(_data: &[u8]) -> Option<Vec<u8>> {
    None
}

#[cfg(all(test, windows))]
mod tests {
    #[test]
    fn a_secret_comes_back_for_this_account() {
        let sealed = super::protect(b"a secret").unwrap();
        assert!(!sealed.windows(8).any(|w| w == b"a secret"));
        assert_eq!(super::unprotect(&sealed).unwrap(), b"a secret");
        assert_eq!(super::unprotect(b"not sealed"), None);
    }
}
