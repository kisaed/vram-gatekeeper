//! Process enumeration without opening process handles (spec §11.1).
//!
//! Responsibility: list the executable file names of running processes using
//! a Toolhelp snapshot.
//! Not responsible for: deciding which names are games (see
//! `detect::process_scan`).
//!
//! Anti-cheat friendliness (NFR-4): a Toolhelp snapshot reads the system
//! process table. It never calls `OpenProcess`, so no handle to a game is
//! ever created.
//!
//! Implements FR-GAME-1, NFR-4.

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};

use crate::error::{CoreError, CoreResult};

/// One running process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessEntry {
    /// Process id.
    pub pid: u32,
    /// Executable file name, e.g. `eldenring.exe`.
    pub name: String,
}

/// Takes a snapshot of all running processes.
///
/// # Errors
/// Returns [`CoreError::Platform`] if the snapshot cannot be created.
pub fn list_processes() -> CoreResult<Vec<ProcessEntry>> {
    // SAFETY: TH32CS_SNAPPROCESS with process id 0 snapshots all processes and
    // takes no other arguments. The returned handle is closed below on every
    // path after it is obtained.
    // https://learn.microsoft.com/windows/win32/api/tlhelp32/nf-tlhelp32-createtoolhelp32snapshot
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }
        .map_err(|e| CoreError::Platform(format!("CreateToolhelp32Snapshot failed: {e}")))?;

    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(std::mem::size_of::<PROCESSENTRY32W>()).unwrap_or(0),
        ..PROCESSENTRY32W::default()
    };
    let mut out = Vec::new();

    // SAFETY: `snapshot` is a valid snapshot handle and `entry` is a
    // PROCESSENTRY32W whose `dwSize` is set as the API requires. Process32FirstW
    // returns Err when the table is empty; that simply ends the loop.
    // https://learn.microsoft.com/windows/win32/api/tlhelp32/nf-tlhelp32-process32firstw
    let mut more = unsafe { Process32FirstW(snapshot, &raw mut entry) }.is_ok();
    while more {
        let len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len());
        out.push(ProcessEntry { pid: entry.th32ProcessID, name: String::from_utf16_lossy(&entry.szExeFile[..len]) });
        // SAFETY: same handle and entry as above; Process32NextW returns Err
        // (ERROR_NO_MORE_FILES) after the last process, which ends the loop.
        // https://learn.microsoft.com/windows/win32/api/tlhelp32/nf-tlhelp32-process32nextw
        more = unsafe { Process32NextW(snapshot, &raw mut entry) }.is_ok();
    }

    // SAFETY: `snapshot` is a handle we own and no longer use.
    // https://learn.microsoft.com/windows/win32/api/handleapi/nf-handleapi-closehandle
    let _ = unsafe { CloseHandle(snapshot) };
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_this_test_process() {
        let me = std::process::id();
        let list = list_processes().expect("snapshot");
        assert!(list.iter().any(|p| p.pid == me), "own pid {me} must appear in the process list");
        assert!(list.iter().all(|p| !p.name.is_empty() || p.pid == 0));
    }
}
