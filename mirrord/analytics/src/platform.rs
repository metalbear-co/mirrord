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
/// kernel release elsewhere (`6.8`).
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

/// The release string [`os_version`] is parsed from.
///
/// On macOS the kernel release is the Darwin version, which does not identify the product
/// release users know, so the product version is used there.
fn os_release() -> Option<String> {
    if cfg!(target_os = "macos") {
        sysinfo::System::os_version()
    } else {
        sysinfo::System::kernel_version()
    }
}
