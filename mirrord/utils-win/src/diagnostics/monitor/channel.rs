//! The client side of the crash channel — what a registered layer holds and signals through.
//!
//! A layer registers once at startup (over TCP, in a healthy state), opens the per-pid objects the
//! monitor created, and keeps them in a [`MonitorChannel`] for the rest of the process's life. From
//! then on the crash path never opens anything by name and never allocates: it writes the shared
//! section and flips an event.

use std::{
    io::Read,
    net::{SocketAddr, TcpStream},
    time::Duration,
};

use str_win::string_to_u16_buffer;
use winapi::{
    shared::{minwindef::FALSE, ntdef::HANDLE},
    um::{
        memoryapi::{FILE_MAP_ALL_ACCESS, MapViewOfFile, OpenFileMappingW},
        synchapi::{OpenEventW, SetEvent, WaitForSingleObject},
        winbase::WAIT_OBJECT_0,
        winnt::{EVENT_MODIFY_STATE, SYNCHRONIZE},
    },
};

use super::{
    ACK_READY, CLEAN_EVENT_PREFIX, CRASH_EVENT_PREFIX, CrashInfo, DONE_EVENT_PREFIX,
    DUMP_ACK_TIMEOUT_MS, INFO_SECTION_PREFIX, KIND_CRASH, KIND_INIT_FAILURE, REASON_CAPACITY,
    Registration, write_registration,
};
use crate::diagnostics::handle::OwnedHandle;

/// The client side of the crash channel, held by a registered layer.
///
/// All handles are opened once at registration, in a healthy state, so the crash path never opens
/// anything by name and never allocates. Each is an [`OwnedHandle`], so dropping the channel frees
/// every handle and unmaps the view — no hand-written cleanup.
///
/// `view` is the heart of the channel. At registration the monitor created a named shared section
/// (`mirrord_crash_info_<pid>`) sized to exactly one [`CrashInfo`]; `MapViewOfFile` maps that
/// section into this process and returns a pointer to its bytes. Because both sides open the *same*
/// named section and agree on the `#[repr(C)]` [`CrashInfo`] layout, `view.as_ptr() as *mut
/// CrashInfo` is a valid pointer to a shared `CrashInfo`: what the layer writes here, the monitor
/// reads from outside.
pub struct MonitorChannel {
    /// Auto-reset event the layer sets to signal a crash or init failure; the monitor waits on it.
    crash_event: OwnedHandle,
    /// Auto-reset event the monitor sets once it has handled the signal; the layer waits on it.
    done_event: OwnedHandle,
    /// Manual-reset event the layer sets on a clean shutdown.
    clean_event: OwnedHandle,
    /// The shared-section mapping handle, held so the mapped `view` stays valid.
    _mapping: OwnedHandle,
    /// A mapped view of the shared `CrashInfo` section (see the struct docs above).
    view: OwnedHandle,
}

impl MonitorChannel {
    /// Signals a crash to the monitor and waits for the dump to finish.
    ///
    /// This is allocation-free. It writes the shared section, sets the crash event, and waits on
    /// the done event. It is safe to call from a crash handler.
    ///
    /// # Arguments
    ///
    /// * `thread_id` - the faulting thread.
    /// * `exception_pointers` - the `EXCEPTION_POINTERS` address in this process.
    ///
    /// # Returns
    ///
    /// `true` when the monitor acknowledged the dump within the timeout.
    pub fn signal_crash(&self, thread_id: u32, exception_pointers: usize) -> bool {
        unsafe {
            let info = self.view.as_ptr() as *mut CrashInfo;
            (*info).thread_id = thread_id;
            (*info).kind = KIND_CRASH;
            (*info).exception_pointers = exception_pointers as u64;

            SetEvent(self.crash_event.as_ptr() as HANDLE);
            WaitForSingleObject(self.done_event.as_ptr() as HANDLE, DUMP_ACK_TIMEOUT_MS)
                == WAIT_OBJECT_0
        }
    }

    /// Signals an early layer-initialization failure and waits for the monitor to surface it.
    ///
    /// Reuses the crash channel: it writes the reason into the shared section with the init-failure
    /// kind, sets the crash event, and waits on the done event. The monitor reports it without a
    /// dump. Without this, an init failure exits via `process::exit` → `DLL_PROCESS_DETACH`, which
    /// signals a clean shutdown and the monitor would never know it failed.
    ///
    /// # Arguments
    ///
    /// * `reason` - a human-readable cause, truncated to the shared buffer's capacity.
    ///
    /// # Returns
    ///
    /// `true` when the monitor acknowledged within the timeout.
    pub fn signal_init_failure(&self, reason: &str) -> bool {
        unsafe {
            let info = self.view.as_ptr() as *mut CrashInfo;
            (*info).thread_id = 0;
            (*info).kind = KIND_INIT_FAILURE;
            (*info).exception_pointers = 0;
            let bytes = reason.as_bytes();
            let len = bytes.len().min(REASON_CAPACITY);
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), (*info).reason.as_mut_ptr(), len);
            (*info).reason_len = len as u32;

            SetEvent(self.crash_event.as_ptr() as HANDLE);
            WaitForSingleObject(self.done_event.as_ptr() as HANDLE, DUMP_ACK_TIMEOUT_MS)
                == WAIT_OBJECT_0
        }
    }

    /// Tells the monitor this process is shutting down cleanly.
    ///
    /// Call this on `DLL_PROCESS_DETACH`. A death the monitor sees after this is a normal close,
    /// not a crash or a kill.
    pub fn signal_clean_shutdown(&self) {
        unsafe { SetEvent(self.clean_event.as_ptr() as HANDLE) };
    }
}

/// Registers this process with the monitor and opens the crash channel.
///
/// This runs at layer startup, in a healthy state. It is best-effort. A missing monitor or any
/// failure yields `None`, and the caller falls back to the in-process dump.
///
/// # Arguments
///
/// * `address` - the monitor's TCP endpoint.
/// * `registration` - this process's identity.
///
/// # Returns
///
/// An opened [`MonitorChannel`], or `None` on any failure.
pub fn register(address: SocketAddr, registration: &Registration) -> Option<MonitorChannel> {
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_secs(2)).ok()?;
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;

    write_registration(&mut stream, registration).ok()?;

    let mut ack = [0u8; 1];
    stream.read_exact(&mut ack).ok()?;
    if ack[0] != ACK_READY {
        return None;
    }

    open_channel(registration.pid)
}

/// Opens the per-pid crash objects the monitor created.
fn open_channel(pid: u32) -> Option<MonitorChannel> {
    let crash_name = string_to_u16_buffer(format!("{CRASH_EVENT_PREFIX}{pid}"));
    let done_name = string_to_u16_buffer(format!("{DONE_EVENT_PREFIX}{pid}"));
    let clean_name = string_to_u16_buffer(format!("{CLEAN_EVENT_PREFIX}{pid}"));
    let info_name = string_to_u16_buffer(format!("{INFO_SECTION_PREFIX}{pid}"));

    unsafe {
        // Each `OwnedHandle::{kernel,view}` returns `None` on a null handle, so the `?` cascade
        // frees whatever opened before the failure as those locals drop — no hand-written
        // close cascade.
        let crash_event =
            OwnedHandle::kernel(OpenEventW(EVENT_MODIFY_STATE, FALSE, crash_name.as_ptr()))?;
        let done_event = OwnedHandle::kernel(OpenEventW(SYNCHRONIZE, FALSE, done_name.as_ptr()))?;
        let clean_event =
            OwnedHandle::kernel(OpenEventW(EVENT_MODIFY_STATE, FALSE, clean_name.as_ptr()))?;
        let mapping = OwnedHandle::kernel(OpenFileMappingW(
            FILE_MAP_ALL_ACCESS,
            FALSE,
            info_name.as_ptr(),
        ))?;

        let view = OwnedHandle::view(MapViewOfFile(
            mapping.as_ptr(),
            FILE_MAP_ALL_ACCESS,
            0,
            0,
            std::mem::size_of::<CrashInfo>(),
        ))?;

        Some(MonitorChannel {
            crash_event,
            done_event,
            clean_event,
            _mapping: mapping,
            view,
        })
    }
}
