use std::{net::SocketAddr, sync::OnceLock};

use mirrord_layer_lib::{
    error::ConnectError,
    socket::{
        HookResult, SocketAddrExt,
        ops::{ConnectResult, connect_common},
    },
};
use socket2::SockAddr;
use winapi::{
    ctypes::c_void,
    shared::{
        guiddef::{GUID, IsEqualGUID},
        minwindef::{BOOL, INT},
        ws2def::{SOCKADDR, WSABUF},
    },
    um::{
        minwinbase::OVERLAPPED,
        mswsock::{LPFN_CONNECTEX, WSAID_CONNECTEX},
        winsock2::{SOCKET, WSAGetLastError},
    },
};

type ConnectExFn = unsafe extern "system" fn(
    SOCKET,
    *const SOCKADDR,
    INT,
    *mut c_void,
    u32,
    *mut u32,
    *mut OVERLAPPED,
) -> BOOL;

static CONNECTEX_ORIGINAL: OnceLock<ConnectExFn> = OnceLock::new();

pub fn get_connectex_original() -> Option<ConnectExFn> {
    CONNECTEX_ORIGINAL.get().copied()
}

unsafe fn write_connectex_pointer(buffer: *mut c_void, len: u32, detour: LPFN_CONNECTEX) -> bool {
    if buffer.is_null() || (len as usize) < std::mem::size_of::<LPFN_CONNECTEX>() {
        return false;
    }

    unsafe {
        let target_ptr = buffer as *mut LPFN_CONNECTEX;
        *target_ptr = detour;
    }
    true
}

pub unsafe fn hook_connectex_extension(
    lpv_in_buffer: *mut c_void,
    cb_in_buffer: u32,
    lpv_out_buffer: *mut c_void,
    cb_out_buffer: u32,
    replacement: LPFN_CONNECTEX,
) {
    if lpv_in_buffer.is_null() || cb_in_buffer as usize != std::mem::size_of::<GUID>() {
        tracing::error!(
            "wsa_ioctl_detour -> invalid input buffer for ConnectEx GUID (is_null: {}, size: {})",
            lpv_in_buffer.is_null(),
            cb_in_buffer
        );
        return;
    }

    let requested_guid = unsafe { *(lpv_in_buffer as *const GUID) };
    if !IsEqualGUID(&requested_guid, &WSAID_CONNECTEX) {
        tracing::trace!("wsa_ioctl_detour -> Skipping non-ConnectEx GUID");
        return;
    }

    if (cb_out_buffer as usize) < std::mem::size_of::<LPFN_CONNECTEX>() {
        tracing::warn!(
            "wsa_ioctl_detour -> insufficient output buffer for ConnectEx pointer (size: {})",
            cb_out_buffer
        );
        return;
    }

    let original_ptr = unsafe { *(lpv_out_buffer as *mut LPFN_CONNECTEX) };
    if original_ptr.is_none() {
        tracing::error!("wsa_ioctl_detour -> ConnectEx original pointer is null");
        return;
    }

    CONNECTEX_ORIGINAL.set(original_ptr.unwrap()).unwrap_or_else(|curr_val| {
            tracing::warn!(
                "wsa_ioctl_detour -> ConnectEx original pointer was already set (addr: {:p}), overwriting it.",
                curr_val as *const ()
            );
        });
    tracing::debug!("wsa_ioctl_detour -> captured original ConnectEx address");

    if unsafe { write_connectex_pointer(lpv_out_buffer, cb_out_buffer, replacement) } {
        tracing::trace!("wsa_ioctl_detour -> substituted ConnectEx pointer with detour");
    } else {
        tracing::warn!("wsa_ioctl_detour -> failed to write ConnectEx detour pointer");
    }
}

/// Wrapper around Windows WSABUF array for safe buffer handling
#[derive(Debug)]
pub struct WSABufferData {
    buffers: Vec<(*const u8, u32)>,
    total_length: usize,
}

impl WSABufferData {
    /// Create from raw WSABUF array pointer and count
    pub unsafe fn from_raw(lpBuffers: *const u8, dwBufferCount: u32) -> Option<Self> {
        if lpBuffers.is_null() || dwBufferCount == 0 {
            return None;
        }

        // Prevent excessive buffer counts that could cause DoS
        if dwBufferCount > 64 {
            return None;
        }

        let wsabuf_array = lpBuffers as *const WSABUF;
        let mut buffers = Vec::with_capacity(dwBufferCount as usize);
        let mut total_length = 0usize;

        for i in 0..dwBufferCount {
            // SAFETY: We've verified that wsabuf_array is not null and i is within bounds
            let wsabuf = unsafe { &*wsabuf_array.add(i as usize) };
            if wsabuf.buf.is_null() {
                // Invalid buffer
                return None;
            }

            let buf_ptr = wsabuf.buf as *const u8;
            let buf_len = wsabuf.len;

            // Prevent integer overflow in total_length calculation
            if total_length.saturating_add(buf_len as usize)
                > total_length.wrapping_add(buf_len as usize)
            {
                // Would overflow
                return None;
            }

            buffers.push((buf_ptr, buf_len));
            total_length += buf_len as usize;
        }

        Some(Self {
            buffers,
            total_length,
        })
    }

    /// Get the first buffer for simple single-buffer operations
    pub fn first_buffer(&self) -> Option<(*const u8, u32)> {
        self.buffers.first().copied()
    }

    /// Check if this is a simple single-buffer case
    pub fn is_single_buffer(&self) -> bool {
        self.buffers.len() == 1
    }

    /// Get total data length across all buffers
    pub fn total_length(&self) -> usize {
        self.total_length
    }

    /// Get number of buffers
    pub fn buffer_count(&self) -> usize {
        self.buffers.len()
    }

    /// Create a WSABUF for a single buffer (for calling original functions)
    pub fn create_single_wsabuf(&self, buffer: *const u8, length: u32) -> WSABUF {
        WSABUF {
            len: length,
            buf: buffer as *mut i8,
        }
    }
}

impl TryFrom<(*const u8, u32)> for WSABufferData {
    type Error = &'static str;

    fn try_from((lpBuffers, dwBufferCount): (*const u8, u32)) -> Result<Self, Self::Error> {
        // SAFETY: This is inherently unsafe since we're dealing with raw pointers
        // The caller must ensure the pointers are valid
        unsafe { Self::from_raw(lpBuffers, dwBufferCount) }.ok_or("Invalid WSABUF data")
    }
}

impl TryFrom<(*mut u8, u32)> for WSABufferData {
    type Error = &'static str;

    fn try_from((lpBuffers, dwBufferCount): (*mut u8, u32)) -> Result<Self, Self::Error> {
        // Convert *mut u8 to *const u8 and delegate to the const version
        Self::try_from((lpBuffers as *const u8, dwBufferCount))
    }
}

/// Log connection result and return it
pub fn log_connection_result<T>(result: T, function_name: &str, addr: SockAddr)
where
    T: std::fmt::Display + std::cmp::PartialEq<i32>,
{
    let socket_address = addr.as_socket();
    if result == 0 {
        tracing::info!(
            "{} -> successfully connected to address: {:?}",
            function_name,
            socket_address
        );
    } else {
        tracing::error!(
            "{} -> failed to connect to address: {:?}, error code: {}, wsa_getlasterror: {}",
            function_name,
            addr,
            result,
            unsafe { WSAGetLastError() }
        );
    }
}

/// Complete proxy connection flow that handles validation, conversion, and preparation
///
/// This function encapsulates the entire flow from raw sockaddr to prepared connection:
/// 1. Validates socket for outgoing traffic interception
/// 2. Converts Windows sockaddr to Rust SocketAddr
/// 3. Attempts proxy connection through mirrord
/// 4. Handles connection success and prepares final sockaddr
///
/// Returns either a prepared sockaddr ready for the original connect function,
/// or Fallback to indicate the caller should use the original function.
#[allow(clippy::result_large_err)]
#[mirrord_layer_macro::instrument(level = "trace", skip(connect_fn), ret)]
pub fn connect<F>(
    socket: SOCKET,
    name: *const SOCKADDR,
    namelen: INT,
    function_name: &str,
    connect_fn: F,
) -> HookResult<ConnectResult>
where
    F: FnOnce(SockAddr) -> ConnectResult,
{
    // Convert Windows sockaddr to Rust SocketAddr
    let remote_addr = match unsafe { SocketAddr::try_from_raw(name, namelen) } {
        Some(addr) => addr,
        None => {
            tracing::warn!(
                "{} -> failed to convert sockaddr, falling back to original",
                function_name
            );
            return Err(ConnectError::Fallback.into());
        }
    };

    // Try to connect through the mirrord proxy using layer-lib integration
    connect_common(socket, SockAddr::from(remote_addr), connect_fn)
}
