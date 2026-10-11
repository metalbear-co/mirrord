//! Details of the machine mirrord runs on, beyond the operating system name that every report
//! already carries.
//!
//! The layer's behavior depends on them: which libc symbols exist to hook differs between
//! architectures and operating system releases, so usage data cannot be read without them.

/// CPU architecture this binary was built for.
///
/// Reported as the numeric `arch` property. The numeric mapping keeps the analytics schema
/// stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum Arch {
    Other = 0,
    X86_64 = 1,
    Aarch64 = 2,
}

impl Arch {
    pub fn current() -> Self {
        match std::env::consts::ARCH {
            "x86_64" => Self::X86_64,
            "aarch64" => Self::Aarch64,
            _ => Self::Other,
        }
    }
}

/// Major and minor version of the operating system: the product version on macOS (`15.3`), the
/// kernel release elsewhere on Unix (`6.8`).
pub fn os_version() -> Option<(u32, u32)> {
    parse_version(&os_release()?)
}

/// Takes the first two numeric components of `release`, ignoring whatever follows them, as in
/// the `6.8.0-45-generic` that Linux reports.
fn parse_version(release: &str) -> Option<(u32, u32)> {
    let mut components = release.split('.').map(|component| {
        let digits = component
            .find(|c: char| !c.is_ascii_digit())
            .map_or(component, |end| &component[..end]);
        digits.parse().ok()
    });

    Some((components.next()??, components.next()??))
}

#[cfg(target_os = "macos")]
fn os_release() -> Option<String> {
    let mut buffer = [0u8; 32];
    let mut length = buffer.len();

    // SAFETY: `buffer` is valid for `length` bytes, and the kernel writes at most that many.
    let result = unsafe {
        libc::sysctlbyname(
            c"kern.osproductversion".as_ptr(),
            buffer.as_mut_ptr().cast(),
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if result != 0 {
        return None;
    }

    let release = std::ffi::CStr::from_bytes_until_nul(buffer.get(..length)?).ok()?;
    Some(release.to_string_lossy().into_owned())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn os_release() -> Option<String> {
    let mut name = std::mem::MaybeUninit::<libc::utsname>::uninit();

    // SAFETY: `uname` fills the whole struct on success, with a nul-terminated `release`.
    let release = unsafe {
        if libc::uname(name.as_mut_ptr()) != 0 {
            return None;
        }
        std::ffi::CStr::from_ptr(name.assume_init_ref().release.as_ptr())
    };

    Some(release.to_string_lossy().into_owned())
}

#[cfg(not(unix))]
fn os_release() -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::parse_version;

    #[test]
    fn parses_release_strings() {
        assert_eq!(parse_version("15.3.1"), Some((15, 3)));
        assert_eq!(parse_version("6.8.0-45-generic"), Some((6, 8)));
        assert_eq!(parse_version("26.0"), Some((26, 0)));
        assert_eq!(parse_version("5.15-rc1"), Some((5, 15)));
        assert_eq!(parse_version("rolling"), None);
        assert_eq!(parse_version("7"), None);
    }
}
