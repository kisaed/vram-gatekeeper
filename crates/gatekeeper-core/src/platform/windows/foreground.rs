//! Foreground-window change notifications and full-screen state
//! (spec §11.2, §11.4, F6).
//!
//! Responsibility: tell the app, without polling, when the foreground window
//! changes (a thread running `SetWinEventHook`), and report whether an
//! exclusive full-screen Direct3D application is running
//! (`SHQueryUserNotificationState`).
//! Not responsible for: deciding what counts as a game (see `detect`).
//!
//! Anti-cheat friendliness (NFR-4): the hook is registered out-of-context
//! (`WINEVENT_OUTOFCONTEXT`), so Windows delivers events to *our* process and
//! nothing is injected into any other process.
//!
//! Implements FR-GAME-1, NFR-2, NFR-4.

use std::sync::Mutex;
use std::thread::JoinHandle;

use tokio::sync::mpsc::UnboundedSender;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::Shell::{
    QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN, SHQueryUserNotificationState,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, EVENT_SYSTEM_FOREGROUND, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, MSG,
    PostThreadMessageW, TranslateMessage, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS, WM_QUIT,
};

use crate::error::{CoreError, CoreResult};

/// Where the hook callback sends the new foreground process id. The callback
/// is a plain C function pointer with no user data, so the sink is global.
static SINK: Mutex<Option<UnboundedSender<u32>>> = Mutex::new(None);

/// What `SHQueryUserNotificationState` reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FullscreenState {
    /// An exclusive full-screen Direct3D application is running.
    ExclusiveD3d,
    /// Some full-screen application (video, presentation) is running. Too
    /// broad to call a game on its own (spec F6).
    Busy,
    /// Nothing full-screen.
    None,
}

/// Queries the current full-screen state.
///
/// # Errors
/// Returns [`CoreError::Platform`] if the shell call fails.
pub fn fullscreen_state() -> CoreResult<FullscreenState> {
    // SAFETY: SHQueryUserNotificationState has no preconditions.
    // https://learn.microsoft.com/windows/win32/api/shellapi/nf-shellapi-shqueryusernotificationstate
    let state = unsafe { SHQueryUserNotificationState() }
        .map_err(|e| CoreError::Platform(format!("SHQueryUserNotificationState failed: {e}")))?;
    Ok(if state == QUNS_RUNNING_D3D_FULL_SCREEN {
        FullscreenState::ExclusiveD3d
    } else if state == QUNS_BUSY || state == QUNS_PRESENTATION_MODE {
        FullscreenState::Busy
    } else {
        FullscreenState::None
    })
}

/// Process id owning the current foreground window, if there is one.
pub fn foreground_pid() -> Option<u32> {
    // SAFETY: both calls only read window-manager state; a null window yields None.
    // https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-getforegroundwindow
    let hwnd = unsafe { GetForegroundWindow() };
    pid_of_window(hwnd)
}

/// Process id owning `hwnd`.
fn pid_of_window(hwnd: HWND) -> Option<u32> {
    if hwnd.0.is_null() {
        return None;
    }
    let mut pid = 0u32;
    // SAFETY: `pid` is a valid out-pointer. This needs no process handle
    // (NFR-4). https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-getwindowthreadprocessid
    let thread = unsafe { GetWindowThreadProcessId(hwnd, Some(&raw mut pid)) };
    (thread != 0 && pid != 0).then_some(pid)
}

/// The hook callback. Windows calls it on the hook thread whenever the
/// foreground window changes.
unsafe extern "system" fn on_foreground(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _object: i32,
    _child: i32,
    _thread: u32,
    _time: u32,
) {
    if let Some(pid) = pid_of_window(hwnd)
        && let Ok(guard) = SINK.lock()
        && let Some(tx) = guard.as_ref()
    {
        let _ = tx.send(pid);
    }
}

/// A running foreground hook; stops and joins its thread on drop.
pub struct ForegroundHook {
    /// Thread id used to post `WM_QUIT`.
    thread_id: u32,
    /// The hook thread.
    thread: Option<JoinHandle<()>>,
}

/// Starts the hook thread. Each foreground change sends the new foreground
/// process id into `tx`.
///
/// # Errors
/// Returns [`CoreError::Platform`] if the hook cannot be installed.
pub fn start_foreground_hook(tx: UnboundedSender<u32>) -> CoreResult<ForegroundHook> {
    *SINK.lock().map_err(|_| CoreError::Platform("foreground sink poisoned".into()))? = Some(tx);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<u32, String>>();

    let thread = std::thread::Builder::new()
        .name("foreground-hook".into())
        .spawn(move || {
            // SAFETY: GetCurrentThreadId has no preconditions.
            let tid = unsafe { GetCurrentThreadId() };
            // SAFETY: `on_foreground` has the WINEVENTPROC signature. Out-of-context
            // delivery runs the callback in this process, injecting nothing
            // elsewhere; SKIPOWNPROCESS ignores our own windows.
            // https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-setwineventhook
            let hook = unsafe {
                SetWinEventHook(
                    EVENT_SYSTEM_FOREGROUND,
                    EVENT_SYSTEM_FOREGROUND,
                    None,
                    Some(on_foreground),
                    0,
                    0,
                    WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS,
                )
            };
            if hook.0.is_null() {
                let _ = ready_tx.send(Err("SetWinEventHook returned no hook".into()));
                return;
            }
            let _ = ready_tx.send(Ok(tid));

            // The hook only fires while this thread pumps messages.
            let mut msg = MSG::default();
            // SAFETY: `msg` is a valid MSG. GetMessageW returns 0 on WM_QUIT and
            // -1 on error; both end the loop.
            // https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-getmessagew
            while unsafe { GetMessageW(&raw mut msg, None, 0, 0) }.0 > 0 {
                // SAFETY: standard message dispatch of a message we just received.
                unsafe {
                    let _ = TranslateMessage(&raw const msg);
                    DispatchMessageW(&raw const msg);
                }
            }
            // SAFETY: `hook` came from SetWinEventHook on this thread.
            // https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-unhookwinevent
            let _ = unsafe { UnhookWinEvent(hook) };
        })
        .map_err(|e| CoreError::Platform(format!("cannot start foreground hook thread: {e}")))?;

    match ready_rx.recv() {
        Ok(Ok(thread_id)) => Ok(ForegroundHook { thread_id, thread: Some(thread) }),
        Ok(Err(msg)) => {
            let _ = thread.join();
            Err(CoreError::Platform(msg))
        }
        Err(_) => Err(CoreError::Platform("foreground hook thread ended unexpectedly".into())),
    }
}

impl Drop for ForegroundHook {
    fn drop(&mut self) {
        // SAFETY: posting WM_QUIT to our own hook thread's queue; if the thread
        // already ended the call fails harmlessly.
        // https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-postthreadmessagew
        let _ = unsafe { PostThreadMessageW(self.thread_id, WM_QUIT, WPARAM(0), LPARAM(0)) };
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        if let Ok(mut sink) = SINK.lock() {
            *sink = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_starts_and_stops_cleanly() {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let hook = start_foreground_hook(tx).expect("hook installs");
        drop(hook); // joins the thread; hangs if WM_QUIT is not delivered
    }

    #[test]
    fn fullscreen_state_can_be_queried() {
        // No assertion about the value: it depends on what the developer is
        // running. The call must simply work.
        fullscreen_state().expect("shell query works");
    }

    #[test]
    fn a_foreground_pid_is_available_on_an_interactive_desktop() {
        // May be None on a locked or headless session; only checks it does not crash.
        let _ = foreground_pid();
    }
}
