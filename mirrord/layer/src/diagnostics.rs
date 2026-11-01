//! Startup log of the libc and kernel the layer runs on, on Linux.
//!
//! Which libc symbols an application calls depends on two glibc versions, and they often differ:
//!
//! - the glibc the executable was linked against picks the symbols it imports. Linked before 2.33,
//!   `fstat` in source becomes an import of `__fxstat64`; from 2.33 on, an import of `fstat`.
//! - the glibc the process runs on picks which of those symbols exist for the layer to hook.
//!
//! A hook that never fires because the application imports the other name is invisible otherwise,
//! so [`log_environment`] records both versions, plus the kernel, once per process.

use std::{error::Error, ffi::CStr, fs::File};

use libc::c_char;
use nix::sys::utsname::uname;
use object::{Endianness, ReadCache, elf::FileHeader64, read::elf::FileHeader};

/// Logs the runtime libc, the executable's link-time glibc and the kernel release at info level.
///
/// Every field falls back to a readable placeholder, so this never fails layer startup. `tracing`
/// evaluates the fields only when the event is enabled, so a process without layer logs doesn't
/// read its executable.
pub(crate) fn log_environment() {
    tracing::info!(
        libc = %runtime_libc(),
        executable_glibc = %executable_glibc(),
        kernel = %kernel_release(),
        "Linux environment",
    );
}

/// The glibc version the process runs on, or `not glibc` (in practice musl).
///
/// `gnu_get_libc_version` is looked up at runtime instead of linked, so this also works where it
/// doesn't exist.
fn runtime_libc() -> String {
    // SAFETY: `RTLD_DEFAULT` and a NUL-terminated name are valid arguments to `dlsym`.
    let symbol = unsafe { libc::dlsym(libc::RTLD_DEFAULT, c"gnu_get_libc_version".as_ptr()) };
    if symbol.is_null() {
        return "not glibc".to_owned();
    }

    // SAFETY: glibc declares `const char *gnu_get_libc_version(void)`.
    let gnu_get_libc_version: extern "C" fn() -> *const c_char =
        unsafe { std::mem::transmute(symbol) };
    // SAFETY: it returns a pointer to a static NUL-terminated string.
    let version = unsafe { CStr::from_ptr(gnu_get_libc_version()) };
    format!("glibc {}", version.to_string_lossy())
}

/// The highest `GLIBC_x.y` version the executable requires, from its `.gnu.version_r` section.
///
/// `none` means it requires no glibc version, e.g. a static or musl executable.
fn executable_glibc() -> String {
    match executable_glibc_floor() {
        Ok(Some(version)) => version,
        Ok(None) => "none".to_owned(),
        Err(error) => format!("unknown ({error})"),
    }
}

/// Reads only the ELF headers and the version sections, not the whole executable, which can be
/// over 100 MB (Node is).
fn executable_glibc_floor() -> Result<Option<String>, Box<dyn Error>> {
    let cache = ReadCache::new(File::open("/proc/self/exe")?);
    let header = FileHeader64::<Endianness>::parse(&cache)?;
    let endian = header.endian()?;
    let sections = header.sections(endian, &cache)?;
    let Some((verneeds, strings_index)) = sections.gnu_verneed(endian, &cache)? else {
        return Ok(None);
    };
    let strings = sections.strings(endian, &cache, strings_index)?;

    let mut floor: Option<(Vec<u32>, &str)> = None;
    for verneed in verneeds {
        let (_, vernauxs) = verneed?;
        for vernaux in vernauxs {
            let name = vernaux?.name(endian, strings)?;
            let Some(version) = name
                .strip_prefix(b"GLIBC_")
                .and_then(|version| std::str::from_utf8(version).ok())
            else {
                continue;
            };
            // Skips non-numeric versions such as `GLIBC_PRIVATE` and `GLIBC_ABI_DT_RELR`.
            let Ok(parts) = version
                .split('.')
                .map(str::parse)
                .collect::<Result<Vec<u32>, _>>()
            else {
                continue;
            };

            if floor.as_ref().is_none_or(|(highest, _)| parts > *highest) {
                floor = Some((parts, version));
            }
        }
    }

    Ok(floor.map(|(_, version)| version.to_owned()))
}

/// The kernel release, e.g. `6.8.0-45-generic`.
fn kernel_release() -> String {
    match uname() {
        Ok(name) => name.release().to_string_lossy().into_owned(),
        Err(errno) => format!("unknown ({errno})"),
    }
}
