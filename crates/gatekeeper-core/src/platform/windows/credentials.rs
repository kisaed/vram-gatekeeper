//! Windows Credential Manager storage for secrets (spec D7, §14.1).
//!
//! Responsibility: keep the LAN API key and the Hugging Face token out of
//! `config.json`, in the user's own credential vault.
//! Not responsible for: deciding which secrets exist (see `app`).
//!
//! Implements D7, the security rules of spec §19.

use windows::Win32::Foundation::{ERROR_NOT_FOUND, FILETIME, GetLastError};
use windows::Win32::Security::Credentials::{
    CRED_FLAGS, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree, CredReadW,
    CredWriteW,
};
use windows::core::{PCWSTR, PWSTR};

use super::wide;
use crate::ports::SecretStore;

/// Prefix of every credential target name (spec §14.1).
const TARGET_PREFIX: &str = "VRAM Gatekeeper/";

/// Secrets in the current user's Credential Manager vault.
pub struct WindowsSecretStore {
    /// Prefix, overridable so tests use their own namespace.
    prefix: String,
}

impl Default for WindowsSecretStore {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowsSecretStore {
    /// Uses the production `VRAM Gatekeeper/<name>` targets.
    pub fn new() -> Self {
        Self { prefix: TARGET_PREFIX.to_owned() }
    }

    /// Uses a custom prefix (tests).
    pub fn with_prefix(prefix: &str) -> Self {
        Self { prefix: prefix.to_owned() }
    }

    /// NUL-terminated UTF-16 target name for a secret.
    fn target(&self, name: &str) -> Vec<u16> {
        wide(&format!("{}{name}", self.prefix))
    }
}

impl SecretStore for WindowsSecretStore {
    fn get(&self, name: &str) -> Option<String> {
        let target = self.target(name);
        let mut cred: *mut CREDENTIALW = std::ptr::null_mut();
        // SAFETY: `target` is NUL-terminated and outlives the call; `cred` is a
        // valid out-pointer. On success Windows allocates the structure, which
        // we free with CredFree below.
        // https://learn.microsoft.com/windows/win32/api/wincred/nf-wincred-credreadw
        unsafe { CredReadW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None, &raw mut cred) }.ok()?;
        if cred.is_null() {
            return None;
        }
        // SAFETY: CredReadW succeeded, so `cred` points to a valid CREDENTIALW
        // whose blob holds `CredentialBlobSize` bytes.
        let value = unsafe {
            let c = &*cred;
            let bytes = std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize);
            String::from_utf8(bytes.to_vec()).ok()
        };
        // SAFETY: `cred` was allocated by CredReadW and is not used afterwards.
        // https://learn.microsoft.com/windows/win32/api/wincred/nf-wincred-credfree
        unsafe { CredFree(cred.cast()) };
        value
    }

    fn set(&self, name: &str, value: &str) -> Result<(), String> {
        let mut target = self.target(name);
        let mut blob = value.as_bytes().to_vec();
        let blob_len = u32::try_from(blob.len()).map_err(|_| "secret is too large".to_owned())?;
        let credential = CREDENTIALW {
            Flags: CRED_FLAGS(0),
            Type: CRED_TYPE_GENERIC,
            TargetName: PWSTR(target.as_mut_ptr()),
            Comment: PWSTR::null(),
            LastWritten: FILETIME::default(),
            CredentialBlobSize: blob_len,
            CredentialBlob: blob.as_mut_ptr(),
            // Stays on this machine and survives reboots; never roams.
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            AttributeCount: 0,
            Attributes: std::ptr::null_mut(),
            TargetAlias: PWSTR::null(),
            UserName: PWSTR::null(),
        };
        // SAFETY: `credential` points at buffers (`target`, `blob`) that outlive
        // the call; Windows copies them.
        // https://learn.microsoft.com/windows/win32/api/wincred/nf-wincred-credwritew
        unsafe { CredWriteW(&raw const credential, 0) }.map_err(|e| format!("could not store the secret: {e}"))
    }

    fn clear(&self, name: &str) {
        let target = self.target(name);
        // SAFETY: NUL-terminated target. A missing credential is not an error
        // for callers, so the result is ignored apart from the lookup below.
        // https://learn.microsoft.com/windows/win32/api/wincred/nf-wincred-creddeletew
        if unsafe { CredDeleteW(PCWSTR(target.as_ptr()), CRED_TYPE_GENERIC, None) }.is_err() {
            // SAFETY: reads the calling thread's last error code.
            let _ = unsafe { GetLastError() } == ERROR_NOT_FOUND;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_round_trip_and_clear() {
        let store = WindowsSecretStore::with_prefix(&format!("VRAM Gatekeeper Tests {}/", std::process::id()));
        assert_eq!(store.get("token"), None);
        store.set("token", "s3cret-ü-value").expect("store");
        assert_eq!(store.get("token").as_deref(), Some("s3cret-ü-value"));
        store.set("token", "replaced").expect("overwrite");
        assert_eq!(store.get("token").as_deref(), Some("replaced"));
        store.clear("token");
        assert_eq!(store.get("token"), None);
        store.clear("token"); // clearing a missing secret is fine
    }
}
