//! Top-level window titles by process, for "Add from running apps"
//! (spec §11.7).
//!
//! Responsibility: map process ids to the title of their visible top-level
//! window, so the user can recognise their game in the list.
//! Not responsible for: choosing games.
//!
//! This only reads titles with `GetWindowTextW`; it opens no process handles
//! (NFR-4).

use std::collections::HashMap;

use windows::Win32::Foundation::{HWND, LPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
};
use windows::core::BOOL;

/// Callback state: first non-empty visible title per process.
struct Collector {
    /// `pid -> title`.
    titles: HashMap<u32, String>,
}

/// Called by `EnumWindows` for each top-level window.
unsafe extern "system" fn visit(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the pointer to the `Collector` we passed to
    // EnumWindows below, which outlives the enumeration and is only used on
    // this thread.
    let collector = unsafe { &mut *(lparam.0 as *mut Collector) };
    // SAFETY: plain window-manager queries on a handle Windows just gave us.
    // https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-iswindowvisible
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return BOOL(1);
    }
    // SAFETY: as above.
    let len = unsafe { GetWindowTextLengthW(hwnd) };
    if len <= 0 {
        return BOOL(1);
    }
    let mut buf = vec![0u16; len as usize + 1];
    // SAFETY: `buf` has room for `len` characters plus the terminator.
    // https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-getwindowtextw
    let copied = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if copied <= 0 {
        return BOOL(1);
    }
    let mut pid = 0u32;
    // SAFETY: `pid` is a valid out-pointer; no process handle is opened.
    unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut pid)) };
    if pid != 0 {
        let title = String::from_utf16_lossy(&buf[..copied as usize]);
        collector.titles.entry(pid).or_insert(title);
    }
    BOOL(1) // keep enumerating
}

/// Visible window titles keyed by process id.
pub fn window_titles() -> HashMap<u32, String> {
    let mut collector = Collector { titles: HashMap::new() };
    // SAFETY: `visit` has the WNDENUMPROC signature and `collector` outlives
    // the synchronous enumeration.
    // https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-enumwindows
    let _ = unsafe { EnumWindows(Some(visit), LPARAM(std::ptr::from_mut(&mut collector) as isize)) };
    collector.titles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumeration_runs_without_crashing() {
        // The content depends on the desktop session; a headless runner may
        // return nothing. The call itself must be safe.
        let titles = window_titles();
        assert!(titles.values().all(|t| !t.is_empty()));
    }
}
