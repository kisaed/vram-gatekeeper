//! Kill-on-close job object for llama-server children.
//!
//! Responsibility: guarantee that every llama-server process we start dies
//! when VRAM Gatekeeper exits, including crashes and Task Manager kills.
//! Not responsible for: deciding *when* to stop a model (see
//! `gate::controller`).
//!
//! Implements NFR-5.

use std::os::windows::io::RawHandle;

use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
};
use windows::core::PCWSTR;

use crate::error::{CoreError, CoreResult};

/// Owns a Windows job object configured to kill all assigned processes
/// when the last handle to the job closes.
///
/// We keep exactly one `JobObject` for the app's lifetime. Windows closes
/// our handle automatically when the process exits for any reason, which
/// triggers `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` and kills the children.
pub struct JobObject {
    /// Handle returned by `CreateJobObjectW`. Closed in `Drop`.
    handle: HANDLE,
}

// SAFETY: a job-object HANDLE is an opaque kernel object reference. The
// kernel serialises operations on it, and we only use it for
// AssignProcessToJobObject (thread-safe) and CloseHandle (in Drop, with
// exclusive ownership), so sharing it across threads is sound.
unsafe impl Send for JobObject {}
// SAFETY: see the `Send` justification; `&JobObject` only permits
// AssignProcessToJobObject, which the kernel serialises.
unsafe impl Sync for JobObject {}

impl JobObject {
    /// Creates the job object and enables kill-on-close.
    ///
    /// # Errors
    /// Returns [`CoreError::Platform`] if `CreateJobObjectW` or
    /// `SetInformationJobObject` fails. The app treats this as fatal at
    /// startup, because without it a crash could leave models in VRAM.
    pub fn new() -> CoreResult<Self> {
        // SAFETY: CreateJobObjectW accepts null for both the security
        // attributes (default security) and the name (unnamed job).
        // https://learn.microsoft.com/windows/win32/api/jobapi2/nf-jobapi2-createjobobjectw
        let handle = unsafe { CreateJobObjectW(None, PCWSTR::null()) }
            .map_err(|e| CoreError::Platform(format!("CreateJobObjectW failed: {e}")))?;

        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

        // SAFETY: `info` is a fully initialised JOBOBJECT_EXTENDED_LIMIT_INFORMATION
        // that outlives the call, and the length passed is exactly its size.
        // https://learn.microsoft.com/windows/win32/api/jobapi2/nf-jobapi2-setinformationjobobject
        let set = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                std::ptr::from_ref(&info).cast(),
                u32::try_from(std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>())
                    .map_err(|_| CoreError::Platform("job info size overflow".into()))?,
            )
        };
        if let Err(e) = set {
            // SAFETY: `handle` was just created by us and is not used again.
            // https://learn.microsoft.com/windows/win32/api/handleapi/nf-handleapi-closehandle
            let _ = unsafe { CloseHandle(handle) };
            return Err(CoreError::Platform(format!("SetInformationJobObject failed: {e}")));
        }
        Ok(Self { handle })
    }

    /// Adds a running process to the job so it dies with us.
    ///
    /// `process` must be a live process handle (for example
    /// `tokio::process::Child::raw_handle`).
    ///
    /// # Errors
    /// Returns [`CoreError::Platform`] if `AssignProcessToJobObject` fails,
    /// for example because the process already exited.
    pub fn assign(&self, process: RawHandle) -> CoreResult<()> {
        // SAFETY: `self.handle` is a valid job handle for our lifetime, and
        // `process` is a handle the caller owns and keeps open for the call.
        // https://learn.microsoft.com/windows/win32/api/jobapi2/nf-jobapi2-assignprocesstojobobject
        unsafe { AssignProcessToJobObject(self.handle, HANDLE(process)) }
            .map_err(|e| CoreError::Platform(format!("AssignProcessToJobObject failed: {e}")))
    }
}

impl Drop for JobObject {
    fn drop(&mut self) {
        // SAFETY: we exclusively own `handle`; closing it is what triggers
        // kill-on-close for any children still running.
        // https://learn.microsoft.com/windows/win32/api/handleapi/nf-handleapi-closehandle
        let _ = unsafe { CloseHandle(self.handle) };
    }
}
