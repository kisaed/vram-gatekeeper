//! Registry reading and change notification (spec §11.3, F7).
//!
//! Responsibility: read DWORD/string values under `HKEY_CURRENT_USER` and call
//! back when a key changes, without polling. Used for Steam's `RunningAppID`.
//! Not responsible for: what the values mean (see `detect::steam`).
//!
//! Implements FR-GAME-1.

use std::thread::JoinHandle;

use windows::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, WAIT_OBJECT_0};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_NOTIFY, KEY_READ, KEY_WRITE, REG_DWORD, REG_NOTIFY_CHANGE_LAST_SET,
    REG_OPTION_NON_VOLATILE, REG_SZ, REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW, RegDeleteTreeW, RegEnumKeyExW,
    RegNotifyChangeKeyValue, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects};
use windows::core::PCWSTR;

use super::wide;
use crate::error::{CoreError, CoreResult};

/// An open key under `HKEY_CURRENT_USER`; closed on drop.
pub struct RegistryKey {
    /// Handle from `RegOpenKeyExW`.
    handle: HKEY,
}

// SAFETY: a registry key handle is an opaque kernel reference; the registry
// APIs used here are thread-safe, and the handle is closed exactly once in
// `Drop`, so moving it to another thread is sound.
unsafe impl Send for RegistryKey {}

impl RegistryKey {
    /// Opens `subkey` for reading and change notification.
    ///
    /// # Errors
    /// Returns [`CoreError::Platform`] if the key does not exist.
    pub fn open(subkey: &str) -> CoreResult<Self> {
        let name = wide(subkey);
        let mut handle = HKEY::default();
        // SAFETY: `name` is NUL-terminated and outlives the call; `handle` is a
        // valid out-pointer. https://learn.microsoft.com/windows/win32/api/winreg/nf-winreg-regopenkeyexw
        let rc = unsafe {
            RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(name.as_ptr()), None, KEY_READ | KEY_NOTIFY, &raw mut handle)
        };
        if rc != ERROR_SUCCESS {
            return Err(CoreError::Platform(format!("cannot open HKCU\\{subkey}: error {}", rc.0)));
        }
        Ok(Self { handle })
    }

    /// Creates (or opens) `subkey` for writing; used by tests to fake Steam.
    ///
    /// # Errors
    /// Returns [`CoreError::Platform`] if the key cannot be created.
    pub fn create(subkey: &str) -> CoreResult<Self> {
        let name = wide(subkey);
        let mut handle = HKEY::default();
        // SAFETY: as in `open`; the class and security arguments may be null.
        // https://learn.microsoft.com/windows/win32/api/winreg/nf-winreg-regcreatekeyexw
        let rc = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(name.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_READ | KEY_WRITE | KEY_NOTIFY,
                None,
                &raw mut handle,
                None,
            )
        };
        if rc != ERROR_SUCCESS {
            return Err(CoreError::Platform(format!("cannot create HKCU\\{subkey}: error {}", rc.0)));
        }
        Ok(Self { handle })
    }

    /// The raw handle. Using this method (instead of the field) inside a
    /// `move` closure makes the closure capture the whole `RegistryKey`, which
    /// is `Send`, rather than just the non-`Send` handle inside it.
    fn raw(&self) -> HKEY {
        self.handle
    }

    /// Reads a DWORD value.
    pub fn read_dword(&self, name: &str) -> Option<u32> {
        let wname = wide(name);
        let mut kind = REG_VALUE_TYPE::default();
        let mut data = 0u32;
        let mut size = 4u32;
        // SAFETY: `data` is a 4-byte buffer and `size` says so; the name is
        // NUL-terminated. https://learn.microsoft.com/windows/win32/api/winreg/nf-winreg-regqueryvalueexw
        let rc = unsafe {
            RegQueryValueExW(
                self.handle,
                PCWSTR(wname.as_ptr()),
                None,
                Some(&raw mut kind),
                Some(std::ptr::from_mut(&mut data).cast()),
                Some(&raw mut size),
            )
        };
        (rc == ERROR_SUCCESS && kind == REG_DWORD).then_some(data)
    }

    /// Reads a string value.
    pub fn read_string(&self, name: &str) -> Option<String> {
        let wname = wide(name);
        let mut kind = REG_VALUE_TYPE::default();
        let mut size = 0u32;
        // SAFETY: a null data pointer asks only for the required size.
        // https://learn.microsoft.com/windows/win32/api/winreg/nf-winreg-regqueryvalueexw
        let rc = unsafe {
            RegQueryValueExW(self.handle, PCWSTR(wname.as_ptr()), None, Some(&raw mut kind), None, Some(&raw mut size))
        };
        if rc != ERROR_SUCCESS || kind != REG_SZ || size == 0 {
            return None;
        }
        let mut buf = vec![0u16; (size as usize).div_ceil(2)];
        // SAFETY: `buf` has at least `size` bytes, as the first call reported.
        let rc = unsafe {
            RegQueryValueExW(
                self.handle,
                PCWSTR(wname.as_ptr()),
                None,
                Some(&raw mut kind),
                Some(buf.as_mut_ptr().cast()),
                Some(&raw mut size),
            )
        };
        if rc != ERROR_SUCCESS {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }

    /// Names of the direct subkeys.
    pub fn subkeys(&self) -> Vec<String> {
        let mut out = Vec::new();
        for index in 0.. {
            let mut name = [0u16; 256];
            let mut len = name.len() as u32;
            // SAFETY: `name` and `len` describe a writable buffer of `len` UTF-16
            // units; the optional class/time arguments are null. The call fails
            // with ERROR_NO_MORE_ITEMS after the last subkey, ending the loop.
            // https://learn.microsoft.com/windows/win32/api/winreg/nf-winreg-regenumkeyexw
            let rc = unsafe {
                RegEnumKeyExW(
                    self.handle,
                    index,
                    Some(windows::core::PWSTR(name.as_mut_ptr())),
                    &raw mut len,
                    None,
                    None,
                    None,
                    None,
                )
            };
            if rc != ERROR_SUCCESS {
                break;
            }
            out.push(String::from_utf16_lossy(&name[..len as usize]));
        }
        out
    }

    /// Writes a DWORD value (tests).
    ///
    /// # Errors
    /// Returns [`CoreError::Platform`] on failure.
    pub fn set_dword(&self, name: &str, value: u32) -> CoreResult<()> {
        let wname = wide(name);
        // SAFETY: the data slice is the 4 bytes of `value`.
        // https://learn.microsoft.com/windows/win32/api/winreg/nf-winreg-regsetvalueexw
        let rc =
            unsafe { RegSetValueExW(self.handle, PCWSTR(wname.as_ptr()), None, REG_DWORD, Some(&value.to_le_bytes())) };
        if rc == ERROR_SUCCESS { Ok(()) } else { Err(CoreError::Platform(format!("RegSetValueExW failed: {}", rc.0))) }
    }
}

impl Drop for RegistryKey {
    fn drop(&mut self) {
        // SAFETY: `handle` is owned by this struct and closed exactly once.
        // https://learn.microsoft.com/windows/win32/api/winreg/nf-winreg-regclosekey
        let _ = unsafe { RegCloseKey(self.handle) };
    }
}

/// Deletes `subkey` and everything below it (tests clean up their fake keys).
pub fn delete_tree(subkey: &str) {
    let name = wide(subkey);
    // SAFETY: NUL-terminated name; failure (key already gone) is ignored.
    // https://learn.microsoft.com/windows/win32/api/winreg/nf-winreg-regdeletetreew
    let _ = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(name.as_ptr())) };
}

/// A running key watcher; stops and joins its thread on drop.
pub struct KeyWatcher {
    /// Event that tells the thread to exit.
    stop: HANDLE,
    /// The watcher thread.
    thread: Option<JoinHandle<()>>,
}

// SAFETY: `stop` is an event handle used only with SetEvent (thread-safe) and
// CloseHandle (in Drop), so the watcher may move between threads.
unsafe impl Send for KeyWatcher {}

impl Drop for KeyWatcher {
    fn drop(&mut self) {
        // SAFETY: `stop` is a valid event we created. Signalling wakes the
        // thread, which then exits; we join before closing the handle.
        // https://learn.microsoft.com/windows/win32/api/synchapi/nf-synchapi-setevent
        let _ = unsafe { SetEvent(self.stop) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        // SAFETY: the thread has exited, so nothing else uses the handle.
        let _ = unsafe { CloseHandle(self.stop) };
    }
}

/// Raw event handle that can move into the watcher thread.
struct SendHandle(HANDLE);
// SAFETY: event handles are plain kernel references usable from any thread.
unsafe impl Send for SendHandle {}

impl SendHandle {
    /// Unwraps the handle on the receiving thread.
    fn into_inner(self) -> HANDLE {
        self.0
    }
}

/// Calls `on_change` (from a dedicated thread) whenever a value under
/// `HKCU\<subkey>` changes. It also calls it once right after starting, so the
/// caller can read the initial state.
///
/// # Errors
/// Returns [`CoreError::Platform`] if the key cannot be opened (for example
/// Steam is not installed).
pub fn watch(subkey: &str, on_change: impl Fn() + Send + 'static) -> CoreResult<KeyWatcher> {
    let key = RegistryKey::open(subkey)?;
    // SAFETY: manual-reset, initially unsignalled, unnamed events.
    // https://learn.microsoft.com/windows/win32/api/synchapi/nf-synchapi-createeventw
    let stop = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
        .map_err(|e| CoreError::Platform(format!("CreateEventW failed: {e}")))?;
    // SAFETY: as above.
    let changed = unsafe { CreateEventW(None, false, false, PCWSTR::null()) }
        .map_err(|e| CoreError::Platform(format!("CreateEventW failed: {e}")))?;
    let (stop_for_thread, changed_for_thread) = (SendHandle(stop), SendHandle(changed));

    let thread = std::thread::Builder::new()
        .name("registry-watch".into())
        .spawn(move || {
            // Calling a by-value method moves the whole `SendHandle` into the
            // closure; touching the `.0` field would capture only the raw,
            // non-Send handle (closures capture disjoint fields).
            let stop = stop_for_thread.into_inner();
            let changed = changed_for_thread.into_inner();
            on_change();
            loop {
                // Re-arm the notification, then wait for it or for the stop signal.
                // SAFETY: `key.handle` is open with KEY_NOTIFY and `changed` is a
                // valid event; asynchronous mode signals the event on change.
                // https://learn.microsoft.com/windows/win32/api/winreg/nf-winreg-regnotifychangekeyvalue
                let rc = unsafe {
                    RegNotifyChangeKeyValue(key.raw(), true, REG_NOTIFY_CHANGE_LAST_SET, Some(changed), true)
                };
                if rc != ERROR_SUCCESS {
                    break;
                }
                // SAFETY: both handles are valid events owned by the watcher.
                // https://learn.microsoft.com/windows/win32/api/synchapi/nf-synchapi-waitformultipleobjects
                let which = unsafe { WaitForMultipleObjects(&[stop, changed], false, INFINITE) };
                if which.0 == WAIT_OBJECT_0.0 {
                    break; // stop requested
                }
                on_change();
            }
            // SAFETY: `changed` is owned by this thread and no longer used.
            let _ = unsafe { CloseHandle(changed) };
        })
        .map_err(|e| CoreError::Platform(format!("cannot start registry watcher: {e}")))?;
    Ok(KeyWatcher { stop, thread: Some(thread) })
}
