#[cfg(target_os = "macos")]
mod codesign;
#[cfg(target_os = "macos")]
mod error;
#[cfg(target_os = "macos")]
mod whitespace;

#[cfg(target_os = "macos")]
mod main {
    use std::{
        env::temp_dir,
        fs::Permissions,
        io::{self, Read},
        os::{macos::fs::MetadataExt, unix::prelude::PermissionsExt},
        path::{Path, PathBuf},
    };

    use object::{
        macho::{FatHeader, MachHeader32, MachHeader64, CPU_TYPE_X86_64},
        read::macho::{FatArch, MachHeader},
        Architecture, Endianness, FileKind,
    };
    use tracing::debug;
    use which::which;

    use super::*;
    use crate::error::Result;
    pub use crate::error::SipError;

    fn is_fat_x64_arch(arch: &&impl FatArch) -> bool {
        matches!(arch.architecture(), Architecture::X86_64)
    }

    struct BinaryInfo {
        offset: usize,
        size: usize,
    }

    impl BinaryInfo {
        fn new(offset: usize, size: usize) -> Self {
            Self { offset, size }
        }

        /// Gets x64 binary offset from a Mach-O fat binary or returns 0 if is x64 macho binary.
        fn from_object_bytes(bytes: &[u8]) -> Result<Self> {
            match FileKind::parse(bytes)? {
                FileKind::MachO32 => {
                    let header: &MachHeader32<Endianness> =
                        MachHeader::parse(bytes, 0).map_err(|_| {
                            SipError::UnsupportedFileFormat(
                                "MachO 32 file parsing failed".to_string(),
                            )
                        })?;
                    if header.cputype(Endianness::default()) == CPU_TYPE_X86_64 {
                        Ok(Self::new(0, bytes.len()))
                    } else {
                        Err(SipError::NoX64Arch)
                    }
                }
                FileKind::MachO64 => {
                    let header: &MachHeader64<Endianness> =
                        MachHeader::parse(bytes, 0).map_err(|_| {
                            SipError::UnsupportedFileFormat(
                                "MachO 64 file parsing failed".to_string(),
                            )
                        })?;
                    if header.cputype(Endianness::default()) == CPU_TYPE_X86_64 {
                        Ok(Self::new(0, bytes.len()))
                    } else {
                        Err(SipError::NoX64Arch)
                    }
                }
                FileKind::MachOFat32 => FatHeader::parse_arch32(bytes)
                    .map_err(|_| SipError::UnsupportedFileFormat("FatMach-O 32-bit".to_string()))?
                    .iter()
                    .find(is_fat_x64_arch)
                    .map(|arch| Self::new(arch.offset() as usize, arch.size() as usize))
                    .ok_or(SipError::NoX64Arch),
                FileKind::MachOFat64 => FatHeader::parse_arch64(bytes)
                    .map_err(|_| SipError::UnsupportedFileFormat("Mach-O 32-bit".to_string()))?
                    .iter()
                    .find(is_fat_x64_arch)
                    .map(|arch| Self::new(arch.offset() as usize, arch.size() as usize))
                    .ok_or(SipError::NoX64Arch),
                other => Err(SipError::UnsupportedFileFormat(format!("{:?}", other))),
            }
        }
    }

    /// Patches a binary to disable SIP.
    /// Right now it extracts x64 binary from fat/MachO binary and patches it.
    fn patch_binary<P: AsRef<Path>, K: AsRef<Path>>(path: P, output: K) -> Result<()> {
        let data = std::fs::read(path.as_ref())?;
        let binary_info = BinaryInfo::from_object_bytes(&data)?;

        let x64_binary = &data[binary_info.offset..binary_info.offset + binary_info.size];
        std::fs::write(output.as_ref(), x64_binary)?;
        std::fs::set_permissions(output.as_ref(), Permissions::from_mode(0o755))?;
        codesign::sign(output)
    }

    fn patch_script<P: AsRef<Path>, K: AsRef<Path>>(
        original_path: P,
        patched_path: K,
        new_shebang: &str,
    ) -> Result<()> {
        let data = std::fs::read(original_path.as_ref())?;
        let original_shebang_size = data
            .iter()
            .position(|&c| c == u8::try_from('\n').unwrap())
            .unwrap_or(0); // Leaving the original shebang there in the second line is fine.
        let contents = &data[original_shebang_size..];
        let mut new_contents = (String::from("#!") + new_shebang + "\n")
            .as_bytes()
            .to_vec();
        new_contents.extend(contents);
        std::fs::write(patched_path.as_ref(), new_contents)?;
        std::fs::set_permissions(patched_path.as_ref(), Permissions::from_mode(0o755))?;
        codesign::sign(patched_path) // TODO: is this necessary for scripts?
    }

    const SF_RESTRICTED: u32 = 0x00080000; // entitlement required for writing, from stat.h (macos)

    fn read_shebang<P: AsRef<Path>>(path: P) -> Result<Option<String>> {
        let mut f = std::fs::File::open(path)?;
        let mut buffer = String::new();
        match f.read_to_string(&mut buffer) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::InvalidData => return Ok(None),
            Err(e) => return Err(SipError::IO(e)),
        }

        const BOM: &str = "\u{feff}";
        let mut content: &str = &buffer;
        if content.starts_with(BOM) {
            content = &content[BOM.len()..];
        }

        let mut shebang = None;
        if content.starts_with("#!") {
            let rest = whitespace::skip(&content[2..]);
            if !rest.starts_with('[') {
                let full_shebang = if let Some(idx) = content.find('\n') {
                    &content[..idx]
                } else {
                    content
                };
                shebang = full_shebang
                    .split_whitespace()
                    .next()
                    .map(|s| s.to_string());
            }
        }
        Ok(shebang)
    }

    enum SipStatus {
        /// The binary that ends up being executed is SIP protected.
        /// The Option is `Some(SipStatus)` when this file is not a SIP binary but a file with a
        /// shebang which points to a SIP binary (possibly via a chain of files with shebangs).
        SomeSIP(PathBuf, Option<Box<SipStatus>>),
        /// The binary that ends up being executed is not SIP protected.
        NoSIP,
    }

    /// Checks the SF_RESTRICTED flags on a file (there might be a better check, feel free to
    /// suggest)
    fn get_sip_status<P: AsRef<Path> + AsRef<std::ffi::OsStr>>(path: P) -> Result<SipStatus> {
        let complete_path = which(&path)?;
        let metadata = std::fs::metadata(&complete_path)?;
        if (metadata.st_flags() & SF_RESTRICTED) > 0 {
            return Ok(SipStatus::SomeSIP(complete_path, None));
        }
        if let Some(target_path) = read_shebang(&complete_path)? {
            // On a circular shebang graph, we would recurse until the stack overflows
            // but so would running without mirrord, right?
            return match get_sip_status(target_path)? {
                // The file at the end of the shebang chain is not protected.
                SipStatus::NoSIP => Ok(SipStatus::NoSIP),
                some_sip => Ok(SipStatus::SomeSIP(complete_path, Some(Box::new(some_sip)))),
            };
        }
        Ok(SipStatus::NoSIP)
    }

    /// Only call this function on a file that is SomeSIP.
    /// Patch shebang scripts recursively and patch final binary.
    fn patch_some_sip(path: &PathBuf, shebang_target: Option<Box<SipStatus>>) -> Result<String> {
        // Strip root path from binary path, as when joined it will clear the previous.
        let output = temp_dir().join("mirrord-bin").join(
            &path
                .strip_prefix("/")
                .map_err(|e| SipError::UnlikelyError(e.to_string()))?,
        );

        // A string of the path of new created file to run instead of the SIPed file.
        let patched_path_string = output
            .to_str()
            .ok_or_else(|| SipError::UnlikelyError("Failed to convert path to string".to_string()))?
            .to_string();

        if output.exists() {
            debug!(
                "Using existing SIP-patched version of {:?}: {}",
                path, patched_path_string
            );
            return Ok(patched_path_string);
        }

        std::fs::create_dir_all(output.parent().ok_or_else(|| {
            SipError::UnlikelyError("Failed to get parent directory".to_string())
        })?)?;

        return match shebang_target {
            None => {
                // The file is a sip protected binary.
                debug!(
                    "{:?} is a SIP protected binary, making non protected version at: {}",
                    path, patched_path_string
                );
                patch_binary(&path, &output)?;
                Ok(patched_path_string)
            }
            // The file is a script with a shebang. Patch recursively.
            Some(sip_file) => {
                if let SipStatus::SomeSIP(target_path, shebang_target) = *sip_file {
                    debug!(
                        "{:?} is a script with a shebang that leads to a SIP protected binary.",
                        path
                    );
                    debug!(
                        "Patching {:?} recursively and making a version of the script with an altered shebang at: {}",
                        target_path,
                        patched_path_string,
                    );
                    let new_target = patch_some_sip(&target_path, shebang_target)?;
                    patch_script(&target_path, &output, &new_target)?;
                    Ok(patched_path_string)
                } else {
                    // This function should only be called on a file which has SomeSIP SipStatus.
                    // If the file has a shebang pointing to a file which is NoSIP, this file should
                    // not have SomeSIP status in the first place.
                    Err(SipError::UnlikelyError(
                        "Internal mirrord error.".to_string(),
                    ))
                }
            }
        };
    }

    /// Check if the file that the user wants to execute is a SIP protected binary (or a script
    /// starting with a shebang that leads to a SIP protected binary). If it is, create a
    /// non-protected version of the file and return the path to it. If it is not, the original
    /// path is copied and returned.
    pub fn sip_patch(binary_path: &str) -> Result<String> {
        if let SipStatus::SomeSIP(path, shebang_target) = get_sip_status(&binary_path)? {
            return patch_some_sip(&path, shebang_target);
        }
        Ok(String::from(binary_path))
    }

    #[cfg(test)]
    mod tests {

        use std::io::Write;

        use super::*;

        #[test]
        fn is_sip_true() {
            assert!(get_sip_status("/bin/ls").unwrap());
        }

        #[test]
        fn is_sip_false() {
            let mut f = tempfile::NamedTempFile::new().unwrap();
            let data = std::fs::read("/bin/ls").unwrap();
            f.write(&data).unwrap();
            f.flush().unwrap();
            assert!(!get_sip_status(f.path().to_str().unwrap()).unwrap());
        }

        #[test]
        fn is_sip_notfound() {
            let err = get_sip_status("/donald/duck/was/a/duck/not/a/quack/a/duck").unwrap_err();
            assert!(err.to_string().contains("No such file or directory"));
        }

        #[test]
        fn patch_binary_fat() {
            let path = "/bin/ls";
            let output = "/tmp/ls_mirrord_test";
            patch_binary(path, output).unwrap();
            assert!(!get_sip_status(output).unwrap());
            // Check DYLD_* features work on it:
            let output = std::process::Command::new(output)
                .env("DYLD_PRINT_LIBRARIES", "1")
                .output()
                .unwrap();
            assert!(String::from_utf8_lossy(&output.stderr).contains("libsystem_kernel.dylib"));
        }
    }
}

#[cfg(target_os = "macos")]
pub use main::*;
