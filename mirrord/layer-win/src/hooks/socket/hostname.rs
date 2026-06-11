//! Hostname-related utilities for Windows socket hooks
//!
//! This module contains all the hostname manipulation, DNS resolution, and caching
//! logic used by the Windows socket hooks, separated from the actual hook implementations.

#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]
#![allow(clippy::result_large_err)]

use mirrord_layer_lib::{
    error::HookResult,
    socket::hostname::{get_remote_netbios_name, remote_hostname_string},
};
use winapi::{
    shared::winerror::{
        ERROR_BUFFER_OVERFLOW, ERROR_INVALID_PARAMETER, ERROR_MORE_DATA, WSAEFAULT,
    },
    um::{errhandlingapi::SetLastError, winsock2::WSASetLastError},
};

/// What [`handle_hostname_ansi`] / [`handle_hostname_unicode`] do when the resolved name is longer
/// than the caller's buffer.
///
/// Most Windows computer-name / hostname APIs fail with "buffer too small" and report the required
/// size. The caller grows its buffer and retries.
///
/// That doesn't suit a caller that passes a fixed buffer and treats failure as fatal. .NET's
/// `Environment.MachineName` is one. It calls `GetComputerNameW` with a 16-wchar buffer and throws
/// on a `FALSE` return.
///
/// A real Windows computer name always fits 16. The remote pod hostname we substitute often does
/// not. So that one path must truncate instead of fail.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NameTooLong {
    /// Write what fits (`buffer_size - 1` units + null) and return success. Used by
    /// `GetComputerNameW` (the .NET path).
    Truncate,
    /// Leave the buffer untouched, set `*nSize` to the required length (incl. null) and the last
    /// error to the given code, and fail -- the standard Windows size-probe contract. Used by
    /// `GetComputerNameExW` (DNS names can be long) and `gethostname`.
    Fail(u32),
}

/// Picks the [`NameTooLong`] policy for a `GetComputerNameExW` name format.
///
/// NetBIOS formats are capped at `MAX_COMPUTERNAME_LENGTH` (15) on Windows. Callers query them with
/// a fixed 16-wchar buffer they can't grow.
///
/// SChannel/SSPI is one such caller, during a TLS handshake (`AcquireCredentialsHandle`). A long
/// pod hostname there fails with `SEC_E_SECPKG_NOT_FOUND` and breaks all outgoing TLS. So NetBIOS
/// formats truncate to fit.
///
/// DNS hostname / fully-qualified formats can legitimately be long. They keep the standard
/// size-probe contract.
pub(super) fn ex_w_policy(name_type: u32) -> NameTooLong {
    use winapi::um::sysinfoapi::{ComputerNameNetBIOS, ComputerNamePhysicalNetBIOS};
    if matches!(name_type, ComputerNameNetBIOS | ComputerNamePhysicalNetBIOS) {
        NameTooLong::Truncate
    } else {
        NameTooLong::Fail(ERROR_MORE_DATA)
    }
}

/// The hostname to return for a `GetComputerNameEx` name format.
///
/// Picks what to return based on the `name_type` (NetBIOS, DNS, domain, ...).
///
/// # Returns
///
/// The name for `name_type`. `None` if the hostname feature is disabled or no hostname is
/// available.
pub fn get_hostname_for_name_type(name_type: u32) -> HookResult<Option<String>> {
    use winapi::um::sysinfoapi::{
        ComputerNameDnsDomain, ComputerNameMax, ComputerNameNetBIOS, ComputerNamePhysicalDnsDomain,
        ComputerNamePhysicalNetBIOS,
    };
    // Handle invalid name_type (ComputerNameMax and beyond)
    if name_type >= ComputerNameMax {
        unsafe {
            SetLastError(ERROR_INVALID_PARAMETER);
        }
        return Ok(None);
    }

    match name_type {
        ComputerNameDnsDomain | ComputerNamePhysicalDnsDomain => {
            // Domain variants - return empty string (no domain info available)
            tracing::debug!("Returning empty domain name for name_type {}", name_type);
            Ok(Some(String::new()))
        }
        ComputerNameNetBIOS | ComputerNamePhysicalNetBIOS => {
            // NetBIOS variants: prefer a Samba-configured NetBIOS name, otherwise fall back to the
            // remote hostname. Length is not capped here -- handle_hostname_unicode/ansi write only
            // what fits the caller's buffer (see `get_computer_name_w_detour`).
            match get_remote_netbios_name() {
                Ok(Some(netbios_name)) if !netbios_name.is_empty() => {
                    Ok(Some(netbios_name.to_uppercase()))
                }
                _ => remote_hostname_string(true),
            }
        }
        // DNS hostname / fully-qualified variants: return the remote hostname unmodified.
        _ => remote_hostname_string(true),
    }
}

/// Write `units` (the encoded name, *without* a trailing null) into the caller's buffer, following
/// the Windows size-probe / truncate contract. `null` is the terminator value to append (`0`).
///
/// This is the byte-for-byte-identical core shared by [`handle_hostname_ansi`] and
/// [`handle_hostname_unicode`]; they differ only in code-unit type (`u8` vs `u16`) and the
/// ANSI-only WinSock error, which stay in the wrappers.
///
/// Behaviour:
/// - If the value + null doesn't fit and the [`NameTooLong`] policy reports size (the standard
///   Windows size-probe contract, which also covers a `*nSize == 0` size query), sets `*nSize` to
///   the required length, sets `SetLastError(err)`, and returns `false`.
/// - Otherwise writes what fits (`buffer_size - 1` units) + the null terminator, sets `*nSize` to
///   the number of units written (excluding the null), and returns `true`.
///
/// We don't cap the buffer size: Windows accepts any buffer, and we only ever write the (short)
/// hostname into it.
///
/// # Safety
///
/// `nSize` must be non-null (the wrappers check this), and `lpBuffer` must point to at least
/// `*nSize` writable units.
unsafe fn write_name_to_caller_buffer<T: Copy>(
    units: &[T],
    null: T,
    lpBuffer: *mut T,
    nSize: *mut u32,
    policy: NameTooLong,
) -> bool {
    let buffer_size = unsafe { *nSize } as usize;
    // `units` is a Rust string's encoded form and carries no terminator, so `+ 1` is the null we
    // write ourselves below -- it is *not* double-counting an existing terminator.
    let needed = units.len() + 1;

    if needed > buffer_size {
        let report_required = match policy {
            NameTooLong::Fail(err) => Some(err),
            // `Truncate` still needs room for the null, so a zero-length buffer reports size too.
            NameTooLong::Truncate if buffer_size == 0 => Some(ERROR_BUFFER_OVERFLOW),
            NameTooLong::Truncate => None,
        };
        if let Some(err) = report_required {
            unsafe {
                *nSize = needed as u32;
                SetLastError(err);
            }
            return false;
        }
    }

    // `buffer_size >= 1` here: write what fits plus the null terminator.
    let copy_len = units.len().min(buffer_size - 1);
    unsafe {
        std::ptr::copy_nonoverlapping(units.as_ptr(), lpBuffer, copy_len);
        *lpBuffer.add(copy_len) = null; // null terminator -- copy_len <= buffer_size - 1
        *nSize = copy_len as u32; // units written, excluding the null
    }
    true
}

/// Generic hostname writer for the ANSI (byte) APIs.
pub fn handle_hostname_ansi<F, H>(
    lpBuffer: *mut i8,
    nSize: *mut u32,
    original_fn: F,
    get_hostname_fn: H,
    function_name: &str,
    ret_vals: (i32, i32),
    policy: NameTooLong,
) -> i32
where
    F: FnOnce() -> i32,
    H: FnOnce() -> HookResult<Option<String>>,
{
    tracing::debug!("{} hook called", function_name);
    let (ret_ok, ret_err) = ret_vals;

    // `nSize` is a required in/out parameter; without it we can neither size nor write.
    if nSize.is_null() {
        unsafe {
            SetLastError(ERROR_INVALID_PARAMETER);
            WSASetLastError(WSAEFAULT.try_into().unwrap());
        }
        return ret_err;
    }

    // No remote hostname (feature disabled, etc.) -> defer to the real OS function.
    let hostname = match get_hostname_fn() {
        Ok(Some(name)) => name,
        _ => {
            tracing::debug!(
                "{}: using original function (hostname feature disabled or no remote hostname)",
                function_name
            );
            return original_fn();
        }
    };

    let bytes = hostname.as_bytes();
    if unsafe { write_name_to_caller_buffer(bytes, 0u8, lpBuffer as *mut u8, nSize, policy) } {
        tracing::debug!("{} returning remote hostname {:?}", function_name, hostname);
        ret_ok
    } else {
        // gethostname-style callers read the WinSock error in addition to GetLastError.
        unsafe { WSASetLastError(WSAEFAULT.try_into().unwrap()) };
        ret_err
    }
}

/// Generic hostname writer for the wide (UTF-16) APIs.
///
/// # Note
///
/// Unlike [`handle_hostname_ansi`], this does not report WinSock errors.
pub fn handle_hostname_unicode<F, H>(
    lpBuffer: *mut u16,
    nSize: *mut u32,
    original_fn: F,
    get_hostname_fn: H,
    function_name: &str,
    policy: NameTooLong,
) -> i32
where
    F: FnOnce() -> i32,
    H: FnOnce() -> HookResult<Option<String>>,
{
    tracing::debug!("{} hook called", function_name);

    // `nSize` is a required in/out parameter; without it we can neither size nor write.
    if nSize.is_null() {
        unsafe { SetLastError(ERROR_INVALID_PARAMETER) };
        return 0;
    }

    // No remote hostname (feature disabled, etc.) -> defer to the real OS function.
    let hostname = match get_hostname_fn() {
        Ok(Some(name)) => name,
        _ => {
            tracing::debug!(
                "{}: using original function (hostname feature disabled or remote resolution failed)",
                function_name
            );
            return original_fn();
        }
    };

    let name: Vec<u16> = hostname.encode_utf16().collect();
    if unsafe { write_name_to_caller_buffer(&name, 0u16, lpBuffer, nSize, policy) } {
        tracing::debug!("{} returning remote hostname {:?}", function_name, hostname);
        // TRUE - Success
        1
    } else {
        0
    }
}

/// Helper function to check if a hostname matches our remote hostname
pub fn is_remote_hostname(hostname: String) -> bool {
    // skip hostname enabled test for is_remote_hostname
    remote_hostname_string(false)
        .is_ok_and(|opt| opt.is_some_and(|remote_hostname| remote_hostname == hostname))
        || get_remote_netbios_name()
            .is_ok_and(|opt| opt.is_some_and(|remote_netbios| remote_netbios == hostname))
}

/// Trim a NetBIOS name to its usable length (<=15 bytes).
///
/// A NetBIOS name occupies a 16-byte field. That's up to 15 usable characters plus a trailing
/// resource-type *suffix* byte (a service identifier, e.g. `0x20` for the Server service). The
/// suffix is not a null terminator, so the usable name is capped at 15 bytes.
///
/// Used by `GetAddrInfoExW`'s `NS_NETBT` path, where names resolve in the NetBIOS namespace and
/// must obey that limit.
///
/// The computer-name APIs do not cap here. They write whatever fits the caller's buffer (see
/// [`handle_hostname_unicode`] / [`handle_hostname_ansi`]).
pub(super) fn trim_netbios_name(name: &mut String) {
    const NETBIOS_MAX_BYTES: usize = 15;
    // `floor_char_boundary` rounds 15 down to the nearest UTF-8 char boundary, so `truncate` caps
    // the field at 15 bytes without panicking on a multi-byte char that straddles it. NetBIOS names
    // are ASCII/OEM, so for real names this is just the first 15 bytes.
    name.truncate(name.floor_char_boundary(NETBIOS_MAX_BYTES));
}

#[cfg(test)]
mod tests {
    use winapi::shared::winerror::ERROR_MORE_DATA;

    use super::*;

    // A hostname longer than the 16-wchar buffer .NET's Environment.MachineName passes. Synthetic
    // and ASCII -- the exact value doesn't matter, only that it exceeds 15 chars.
    const LONG: &str = "a-long-synthetic-hostname-0123";

    /// Drive `handle_hostname_unicode` with a `cap`-wchar buffer; returns (ret, written, out_size).
    fn unicode(name: &str, cap: usize, policy: NameTooLong) -> (i32, String, u32) {
        let owned = name.to_owned();
        let mut buf = vec![0u16; cap];
        let mut size = cap as u32;
        let ret = handle_hostname_unicode(
            buf.as_mut_ptr(),
            &mut size as *mut u32,
            || -999, // original_fn must not run when a hostname is available
            || Ok(Some(owned.clone())),
            "test",
            policy,
        );
        let n = (size as usize).min(buf.len());
        let written = buf.get(..n).expect("n is clamped to buf.len()");
        (ret, String::from_utf16_lossy(written), size)
    }

    /// Drive `handle_hostname_ansi` with a `cap`-byte buffer; returns (ret, written, out_size).
    fn ansi(name: &str, cap: usize, policy: NameTooLong) -> (i32, String, u32) {
        let owned = name.to_owned();
        let mut buf = vec![0i8; cap];
        let mut size = cap as u32;
        let ret = handle_hostname_ansi(
            buf.as_mut_ptr(),
            &mut size as *mut u32,
            || -999,
            || Ok(Some(owned.clone())),
            "test",
            (1, 0), // (ok, err)
            policy,
        );
        let n = (size as usize).min(buf.len());
        let written = buf.get(..n).expect("n is clamped to buf.len()");
        let bytes: Vec<u8> = written.iter().map(|&b| b as u8).collect();
        (ret, String::from_utf8_lossy(&bytes).into_owned(), size)
    }

    #[test]
    fn name_that_fits_is_returned_whole() {
        assert_eq!(
            unicode("host", 16, NameTooLong::Truncate),
            (1, "host".into(), 4)
        );
        assert_eq!(
            ansi("host", 16, NameTooLong::Fail(ERROR_MORE_DATA)),
            (1, "host".into(), 4)
        );
    }

    #[test]
    fn truncate_policy_fits_value_to_buffer() {
        // Too-long hostname into a 16-unit buffer -> first 15 chars + null, success (.NET path).
        let expected: String = LONG.chars().take(15).collect();
        assert_eq!(
            unicode(LONG, 16, NameTooLong::Truncate),
            (1, expected.clone(), 15)
        );
        assert_eq!(ansi(LONG, 16, NameTooLong::Truncate), (1, expected, 15));
    }

    #[test]
    fn truncate_leaves_room_for_null_when_name_equals_buffer() {
        // len == cap: doesn't fit with a null, so truncate to cap - 1.
        assert_eq!(
            unicode("abcdefghijklmnop", 16, NameTooLong::Truncate), // 16 chars
            (1, "abcdefghijklmno".into(), 15)
        );
    }

    #[test]
    fn fail_policy_reports_required_size_and_fails() {
        // Ex / gethostname contract: too-long name -> FALSE + required size (incl. null).
        let required = (LONG.len() + 1) as u32; // value + null
        let (ret, _written, size) = unicode(LONG, 16, NameTooLong::Fail(ERROR_MORE_DATA));
        assert_eq!((ret, size), (0, required));
        let (ret, _written, size) = ansi(LONG, 16, NameTooLong::Fail(ERROR_MORE_DATA));
        assert_eq!((ret, size), (0, required));
    }

    #[test]
    fn zero_size_buffer_acts_as_a_size_probe() {
        // The documented GetComputerNameExW idiom: call with *nSize == 0 to learn the required
        // size, then allocate and call again. Must report the size, not ERROR_INVALID_PARAMETER.
        let required = ("host".len() + 1) as u32;
        assert_eq!(
            unicode("host", 0, NameTooLong::Fail(ERROR_MORE_DATA)),
            (0, String::new(), required)
        );
        assert_eq!(
            ansi("host", 0, NameTooLong::Fail(ERROR_MORE_DATA)),
            (0, String::new(), required)
        );
    }

    #[test]
    fn buffer_larger_than_256_is_accepted_and_gets_the_full_name() {
        // Windows doesn't cap the caller's buffer (e.g. NI_MAXHOST = 1025); only the name is
        // bounded, and we only ever write the short name. 1025 > the old REASONABLE_BUFFER_LIMIT.
        let len = LONG.len() as u32;
        assert_eq!(
            unicode(LONG, 1025, NameTooLong::Fail(ERROR_MORE_DATA)),
            (1, LONG.to_owned(), len)
        );
        assert_eq!(
            ansi(LONG, 1025, NameTooLong::Truncate),
            (1, LONG.to_owned(), len)
        );
    }

    #[test]
    fn ex_w_policy_truncates_netbios_keeps_size_probe_for_dns() {
        use winapi::um::sysinfoapi::{
            ComputerNameDnsFullyQualified, ComputerNameDnsHostname, ComputerNameNetBIOS,
            ComputerNamePhysicalNetBIOS,
        };
        // NetBIOS formats are queried with a fixed 16-wchar buffer the caller can't grow (e.g.
        // SChannel/SSPI during a TLS handshake), so they must truncate to fit rather than fail.
        assert_eq!(ex_w_policy(ComputerNameNetBIOS), NameTooLong::Truncate);
        assert_eq!(
            ex_w_policy(ComputerNamePhysicalNetBIOS),
            NameTooLong::Truncate
        );
        // DNS formats can be long, so they keep the size-probe (report-required) contract.
        assert_eq!(
            ex_w_policy(ComputerNameDnsHostname),
            NameTooLong::Fail(ERROR_MORE_DATA)
        );
        assert_eq!(
            ex_w_policy(ComputerNameDnsFullyQualified),
            NameTooLong::Fail(ERROR_MORE_DATA)
        );
    }
}
