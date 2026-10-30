//! Sleep and resume notifications (spec §21, "PC sleeps and resumes").
//!
//! Responsibility: call a closure when Windows wakes from sleep. Drivers
//! sometimes reset across a sleep, so the app rescans games, re-samples VRAM
//! and health-checks its model servers.
//! Not responsible for: what to do on resume (see `app` and the controller).
//!
//! Uses the callback form of `PowerRegisterSuspendResumeNotification`, which
//! needs no window or message loop.

use std::ffi::c_void;

use windows::Win32::Foundation::{ERROR_SUCCESS, HANDLE};
use windows::Win32::System::Power::{
    DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS, HPOWERNOTIFY, PowerRegisterSuspendResumeNotification,
    PowerUnregisterSuspendResumeNotification,
};
use windows::Win32::UI::WindowsAndMessaging::DEVICE_NOTIFY_CALLBACK;

use crate::error::{CoreError, CoreResult};

/// Sent on every wake from sleep.
const PBT_APMRESUMEAUTOMATIC: u32 = 0x12;
/// Sent after a wake that the user triggered (in addition to the above).
const PBT_APMRESUMESUSPEND: u32 = 0x07;

/// The closure invoked on resume.
type OnResume = Box<dyn Fn() + Send + Sync>;

/// Keeps the registration alive; dropping it unregisters.
pub struct ResumeWatcher {
    /// Registration handle.
    handle: HPOWERNOTIFY,
    /// Parameters the registration points at; must outlive it.
    _params: Box<DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS>,
    /// The closure the callback calls through `Context`.
    _callback: Box<OnResume>,
}

// SAFETY: the handle is only used to unregister in `Drop`, the parameter block
// is never mutated after registration, and the closure is `Send + Sync`.
unsafe impl Send for ResumeWatcher {}
// SAFETY: see above; no method takes `&self` and touches the handle.
unsafe impl Sync for ResumeWatcher {}

/// Called by Windows on a system thread.
///
/// # Safety
/// `context` must be the pointer registered in [`ResumeWatcher::new`]: a
/// `*const OnResume` that stays valid until the registration is dropped.
unsafe extern "system" fn on_power_event(context: *const c_void, event: u32, _setting: *const c_void) -> u32 {
    if matches!(event, PBT_APMRESUMEAUTOMATIC | PBT_APMRESUMESUSPEND) && !context.is_null() {
        // SAFETY: `context` points at the boxed closure owned by the watcher,
        // which unregisters (and waits for callbacks to finish) before freeing it.
        let callback = unsafe { &*context.cast::<OnResume>() };
        callback();
    }
    0
}

impl ResumeWatcher {
    /// Registers `on_resume`.
    ///
    /// # Errors
    /// Returns [`CoreError::Platform`] if Windows refuses the registration.
    pub fn new(on_resume: impl Fn() + Send + Sync + 'static) -> CoreResult<Self> {
        let callback: Box<OnResume> = Box::new(Box::new(on_resume));
        let context = std::ptr::from_ref::<OnResume>(&callback).cast::<c_void>().cast_mut();
        let params = Box::new(DEVICE_NOTIFY_SUBSCRIBE_PARAMETERS { Callback: Some(on_power_event), Context: context });
        let mut raw: *mut c_void = std::ptr::null_mut();
        // SAFETY: `params` is a valid parameter block that outlives the
        // registration (it is stored next to the handle), the flags select the
        // callback form, and `raw` is a valid out-pointer.
        // https://learn.microsoft.com/windows/win32/api/powrprof/nf-powrprof-powerregistersuspendresumenotification
        let rc = unsafe {
            PowerRegisterSuspendResumeNotification(
                DEVICE_NOTIFY_CALLBACK,
                HANDLE(std::ptr::from_ref(&*params).cast_mut().cast::<c_void>()),
                &raw mut raw,
            )
        };
        if rc != ERROR_SUCCESS {
            return Err(CoreError::Platform(format!("PowerRegisterSuspendResumeNotification failed: {rc:?}")));
        }
        Ok(Self { handle: HPOWERNOTIFY(raw as isize), _params: params, _callback: callback })
    }
}

impl Drop for ResumeWatcher {
    fn drop(&mut self) {
        // SAFETY: `handle` came from a successful registration and is
        // unregistered exactly once; the call waits for a running callback.
        // https://learn.microsoft.com/windows/win32/api/powrprof/nf-powrprof-powerunregistersuspendresumenotification
        let _ = unsafe { PowerUnregisterSuspendResumeNotification(self.handle) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registration_succeeds_and_drops_cleanly() {
        let w = ResumeWatcher::new(|| {}).expect("register");
        drop(w);
    }

    #[test]
    fn the_callback_routes_resume_events_only() {
        use std::sync::atomic::{AtomicU32, Ordering};
        let hits = std::sync::Arc::new(AtomicU32::new(0));
        let counter = hits.clone();
        let callback: Box<OnResume> = Box::new(Box::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
        }));
        let context = std::ptr::from_ref::<OnResume>(&callback).cast::<c_void>();
        // SAFETY: `context` points at `callback`, alive for the whole test.
        unsafe {
            on_power_event(context, PBT_APMRESUMEAUTOMATIC, std::ptr::null());
            on_power_event(context, PBT_APMRESUMESUSPEND, std::ptr::null());
            on_power_event(context, 0x04, std::ptr::null()); // PBT_APMSUSPEND: ignored
        }
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }
}
