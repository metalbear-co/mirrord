//! General process helpers.

use std::{env, path::Path};

use str_win::{string_to_u16_buffer, u16_buffer_to_string};
use winapi::{
    shared::{
        minwindef::{DWORD, FALSE},
        ntdef::HANDLE,
    },
    um::{
        handleapi::{CloseHandle, INVALID_HANDLE_VALUE},
        libloaderapi::{LOAD_LIBRARY_SEARCH_SYSTEM32, LoadLibraryExW},
        processenv::GetCommandLineW,
        processthreadsapi::{
            GetCurrentProcess, GetCurrentProcessId, GetExitCodeProcess, OpenProcess,
            OpenProcessToken, ProcessIdToSessionId,
        },
        securitybaseapi::{GetSidSubAuthority, GetSidSubAuthorityCount, GetTokenInformation},
        synchapi::WaitForSingleObject,
        tlhelp32::{
            CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
            TH32CS_SNAPPROCESS,
        },
        winbase::{QueryFullProcessImageNameW, WAIT_OBJECT_0},
        winnt::{
            PROCESS_QUERY_LIMITED_INFORMATION, SECURITY_MANDATORY_HIGH_RID,
            SECURITY_MANDATORY_LOW_RID, SECURITY_MANDATORY_MEDIUM_RID,
            SECURITY_MANDATORY_SYSTEM_RID, SYNCHRONIZE, TOKEN_MANDATORY_LABEL, TOKEN_QUERY,
            TokenIntegrityLevel,
        },
        wow64apiset::IsWow64Process2,
    },
};

/// Returns the current process's executable name.
///
/// The extension is stripped. An empty string is returned when the name cannot be read.
///
/// # Returns
///
/// The executable stem. For example `python` for `C:\Python39\python.exe`.
pub fn get_current_process_name() -> String {
    env::current_exe()
        .ok()
        .and_then(|path| path.file_stem()?.to_str().map(String::from))
        .unwrap_or_default()
}

/// Loads a Windows system DLL from `System32` only, with a hijack-safe search path.
///
/// Plain `LoadLibraryW` searches the application directory, the working directory, and `PATH`, so a
/// malicious DLL planted on any of those can be loaded instead. `LOAD_LIBRARY_SEARCH_SYSTEM32`
/// restricts the search to `System32`, the only correct location for a system DLL such as
/// `dbghelp.dll` or `Msftedit.dll`. The handle is intentionally leaked: callers preload a DLL for
/// the process lifetime and never unload it.
///
/// # Arguments
///
/// * `name` - the system DLL file name, e.g. `dbghelp.dll`.
pub fn load_system_library(name: &str) {
    let wide = string_to_u16_buffer(name);
    unsafe {
        LoadLibraryExW(
            wide.as_ptr(),
            std::ptr::null_mut(),
            LOAD_LIBRARY_SEARCH_SYSTEM32,
        )
    };
}

/// A snapshot of a process's liveness and identity.
#[derive(Debug, Clone)]
pub struct ProcessStatus {
    pub pid: u32,
    pub name: String,
    pub alive: bool,
    pub exit_code: Option<u32>,
}

/// Looks up a process's liveness and image name.
///
/// This is best-effort. Missing access rights yield a not-alive, unnamed result.
///
/// Liveness uses a zero-timeout wait. That avoids the false "alive" that a raw exit-code check
/// gives for a process which exited with code 259.
///
/// # Arguments
///
/// * `pid` - the process id to query.
///
/// # Returns
///
/// A [`ProcessStatus`] snapshot.
pub fn process_status(pid: u32) -> ProcessStatus {
    let mut status = ProcessStatus {
        pid,
        name: String::new(),
        alive: false,
        exit_code: None,
    };

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, FALSE, pid);
        if handle.is_null() {
            return status;
        }

        // A signaled process has exited. A timeout means it is still running.
        if WaitForSingleObject(handle, 0) == WAIT_OBJECT_0 {
            let mut code: u32 = 0;
            if GetExitCodeProcess(handle, &mut code) != 0 {
                status.exit_code = Some(code);
            }
        } else {
            status.alive = true;
        }

        // Image name, stripped of its directory via the path API. Sized past `MAX_PATH` (260) so a
        // long path is not truncated, matching the buffer `modules.rs` uses.
        let mut buf = [0u16; 1024];
        let mut size = buf.len() as u32;
        if QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut size) != 0 {
            let written = buf.get(..size as usize).unwrap_or(&buf);
            let full = u16_buffer_to_string(written);
            status.name = Path::new(&full)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or(full);
        }

        CloseHandle(handle);
    }

    status
}

/// A snapshot of the current process's identity.
///
/// Every field is best-effort. A field that cannot be read falls back to a placeholder. This is
/// the context stamped into the early snapshot and the crash report.
#[derive(Debug, Clone)]
pub struct ProcessIdentity {
    /// This process's id.
    pub pid: u32,
    /// The parent process's id, when it could be read.
    pub parent_pid: Option<u32>,
    /// The parent process's image name, when it could be read.
    pub parent_name: Option<String>,
    /// The raw command line.
    pub command_line: String,
    /// The integrity-level label. For example `medium` or `high`.
    pub integrity: &'static str,
    /// The Windows session id, when it could be read.
    pub session_id: Option<u32>,
    /// Whether the process runs under WOW64. That is 32-bit on 64-bit Windows.
    pub wow64: bool,
}

impl ProcessIdentity {
    /// Gathers the current process's identity.
    ///
    /// This touches several Windows APIs and the process snapshot. Run it at a safe time. Never
    /// from a crash handler.
    ///
    /// # Returns
    ///
    /// A best-effort [`ProcessIdentity`].
    pub fn capture() -> Self {
        let pid = unsafe { GetCurrentProcessId() };
        let (parent_pid, parent_name) = parent_of(pid);
        Self {
            pid,
            parent_pid,
            parent_name,
            command_line: current_command_line(),
            integrity: current_integrity(),
            session_id: session_of(pid),
            wow64: current_wow64(),
        }
    }
}

/// Finds a process's parent id and the parent's image name.
///
/// This walks a one-shot process snapshot. The whole snapshot is read first so the parent can be
/// resolved regardless of its position in the list.
fn parent_of(pid: u32) -> (Option<u32>, Option<String>) {
    let processes = snapshot_processes();
    let parent_pid = processes
        .iter()
        .find(|(process_pid, _, _)| *process_pid == pid)
        .map(|(_, parent, _)| *parent);
    let parent_name = parent_pid.and_then(|parent| {
        processes
            .iter()
            .find(|(process_pid, _, _)| *process_pid == parent)
            .map(|(_, _, name)| name.clone())
    });
    (parent_pid, parent_name)
}

/// Reads the full process list as `(pid, parent_pid, name)` tuples.
fn snapshot_processes() -> Vec<(u32, u32, String)> {
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Vec::new();
    }

    let mut entry: PROCESSENTRY32W = unsafe { std::mem::zeroed() };
    entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as DWORD;

    let mut out = Vec::new();
    let mut have = unsafe { Process32FirstW(snapshot, &mut entry) };
    while have != FALSE {
        out.push((
            entry.th32ProcessID,
            entry.th32ParentProcessID,
            u16_buffer_to_string(entry.szExeFile),
        ));
        have = unsafe { Process32NextW(snapshot, &mut entry) };
    }

    unsafe { CloseHandle(snapshot) };
    out
}

/// Reads the current process's raw command line.
fn current_command_line() -> String {
    let ptr = unsafe { GetCommandLineW() };
    if ptr.is_null() {
        return String::new();
    }

    let mut len = 0usize;
    while unsafe { *ptr.add(len) } != 0 {
        len += 1;
    }
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    u16_buffer_to_string(slice)
}

/// Reads a process's session id.
fn session_of(pid: u32) -> Option<u32> {
    let mut session: DWORD = 0;
    if unsafe { ProcessIdToSessionId(pid, &mut session) } == FALSE {
        None
    } else {
        Some(session)
    }
}

/// Reports whether the current process runs under WOW64.
fn current_wow64() -> bool {
    let mut process_machine: u16 = 0;
    let mut native_machine: u16 = 0;
    let ok = unsafe {
        IsWow64Process2(
            GetCurrentProcess(),
            &mut process_machine,
            &mut native_machine,
        )
    };
    // A non-`IMAGE_FILE_MACHINE_UNKNOWN` process machine marks a WOW64 process.
    ok != FALSE && process_machine != 0
}

/// Reads the current process's integrity level as a short label.
///
/// Returns `unknown` when the token or its mandatory label cannot be read.
fn current_integrity() -> &'static str {
    unsafe {
        let mut token: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == FALSE {
            return "unknown";
        }

        // The first call sizes the buffer for the mandatory label.
        let mut needed: DWORD = 0;
        GetTokenInformation(
            token,
            TokenIntegrityLevel,
            std::ptr::null_mut(),
            0,
            &mut needed,
        );
        if needed == 0 {
            CloseHandle(token);
            return "unknown";
        }

        let mut buffer = vec![0u8; needed as usize];
        let ok = GetTokenInformation(
            token,
            TokenIntegrityLevel,
            buffer.as_mut_ptr() as *mut _,
            needed,
            &mut needed,
        );
        CloseHandle(token);
        if ok == FALSE {
            return "unknown";
        }

        let label = &*(buffer.as_ptr() as *const TOKEN_MANDATORY_LABEL);
        let sid = label.Label.Sid;
        let count = GetSidSubAuthorityCount(sid);
        if count.is_null() || *count == 0 {
            return "unknown";
        }
        let rid = *GetSidSubAuthority(sid, (*count - 1) as DWORD);
        integrity_label(rid)
    }
}

/// Maps a mandatory-label RID to a short integrity label.
fn integrity_label(rid: DWORD) -> &'static str {
    if rid >= SECURITY_MANDATORY_SYSTEM_RID {
        "system"
    } else if rid >= SECURITY_MANDATORY_HIGH_RID {
        "high"
    } else if rid >= SECURITY_MANDATORY_MEDIUM_RID {
        "medium"
    } else if rid >= SECURITY_MANDATORY_LOW_RID {
        "low"
    } else {
        "untrusted"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integrity_label_thresholds() {
        assert_eq!(integrity_label(SECURITY_MANDATORY_SYSTEM_RID), "system");
        assert_eq!(integrity_label(SECURITY_MANDATORY_HIGH_RID), "high");
        assert_eq!(integrity_label(SECURITY_MANDATORY_MEDIUM_RID), "medium");
        assert_eq!(integrity_label(SECURITY_MANDATORY_LOW_RID), "low");
        assert_eq!(integrity_label(0), "untrusted");
        // A RID above system still maps to system; one between medium and high rounds down.
        assert_eq!(integrity_label(SECURITY_MANDATORY_SYSTEM_RID + 1), "system");
        assert_eq!(integrity_label(SECURITY_MANDATORY_MEDIUM_RID + 1), "medium");
    }
}
