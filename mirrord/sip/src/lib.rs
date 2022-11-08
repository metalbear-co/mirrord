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
    use tracing::{debug, trace};
    use which::which;

    use super::*;
    pub use crate::error::SipError;
    use crate::{
        error::Result,
        SipError::{FileNotFound, UnlikelyError},
    };

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

    /// Creates a new file at `patched_path` which has the same contents as `original_path` except
    /// for the shebang which is `new_shebang`.
    fn patch_script<P: AsRef<Path>, K: AsRef<Path>>(
        original_path: P,
        patched_path: K,
        new_shebang: &str,
    ) -> Result<()> {
        if let Some(original_shebang) = read_shebang_from_file(&original_path)? {
            debug!("original_shebang: {}", original_shebang); // TODO: DELETE
            let data = std::fs::read(original_path.as_ref())?;
            let contents = &data[original_shebang.len()..];
            let mut new_contents = String::from("#!") + new_shebang;
            new_contents.push_str(
                std::str::from_utf8(contents).map_err(|_utf| {
                    UnlikelyError("Can't read script contents as utf8".to_string())
                })?,
            );
            std::fs::write(patched_path.as_ref(), new_contents)?;
            std::fs::set_permissions(patched_path.as_ref(), Permissions::from_mode(0o755))?;
            codesign::sign(patched_path) // TODO: is this necessary for scripts?
        } else {
            // This should never happen, if we're in this function the file is a script that starts
            // with a shebang.
            Err(UnlikelyError("Can't read shebang anymore.".to_string()))
        }
    }

    const SF_RESTRICTED: u32 = 0x00080000; // entitlement required for writing, from stat.h (macos)

    /// Extract shebang from file contexts.
    /// "#!/usr/bin/env bash\n..." -> Some("#!/usr/bin/env")
    fn get_shebang_from_string(file_contents: &str) -> Option<String> {
        const BOM: &str = "\u{feff}";
        let mut content: &str = &file_contents;
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
        shebang
    }

    /// Including '#!', just until whitespace, no arguments.
    fn read_shebang_from_file<P: AsRef<Path>>(path: P) -> Result<Option<String>> {
        let mut f = std::fs::File::open(path)?;
        let mut buffer = String::new();
        match f.read_to_string(&mut buffer) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::InvalidData => return Ok(None),
            Err(e) => return Err(SipError::IO(e)),
        }

        Ok(get_shebang_from_string(&buffer))
    }

    #[derive(Debug)]
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
    /// If file is a script with shebang, the SipStatus is derived from the the SipStatus of the
    /// file the shebang points to.
    fn get_sip_status(path: &str) -> Result<SipStatus> {
        trace!("which {}", path);
        // If which fails, try using the given path as is.
        let complete_path = which(&path).unwrap_or(PathBuf::from(&path));
        if !complete_path.exists() {
            return Err(FileNotFound(complete_path.to_string_lossy().to_string()));
        }
        let metadata = std::fs::metadata(&complete_path)?;
        if (metadata.st_flags() & SF_RESTRICTED) > 0 {
            return Ok(SipStatus::SomeSIP(complete_path, None));
        }
        if let Some(shebang) = read_shebang_from_file(&complete_path)? {
            // On a circular shebang graph, we would recurse until the stack overflows
            // but so would running without mirrord, right?
            // Start from index 2 of shebang to get only the path.
            return match get_sip_status(&shebang[2..])? {
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
                        "Shebang points to: {:?}. Patching it recursively and making a version of {:?} with an altered shebang at: {}",
                        target_path,
                        path,
                        patched_path_string,
                    );
                    let new_target = patch_some_sip(&target_path, shebang_target)?;
                    patch_script(&path, &output, &new_target)?;
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
            assert!(matches!(
                get_sip_status("/bin/ls"),
                Ok(SipStatus::SomeSIP(_, _))
            ));
        }

        #[test]
        fn is_sip_false() {
            let mut f = tempfile::NamedTempFile::new().unwrap();
            let data = std::fs::read("/bin/ls").unwrap();
            f.write(&data).unwrap();
            f.flush().unwrap();
            assert!(matches!(
                get_sip_status(f.path().to_str().unwrap()).unwrap(),
                SipStatus::NoSIP
            ));
        }

        #[test]
        fn is_sip_notfound() {
            let err = get_sip_status("/donald/duck/was/a/duck/not/a/quack/a/duck").unwrap_err();
            assert!(err.to_string().contains("executable file not found"));
        }

        #[test]
        fn patch_binary_fat() {
            let path = "/bin/ls";
            let output = "/tmp/ls_mirrord_test";
            patch_binary(path, output).unwrap();
            assert!(matches!(get_sip_status(output).unwrap(), SipStatus::NoSIP));
            // Check DYLD_* features work on it:
            let output = std::process::Command::new(output)
                .env("DYLD_PRINT_LIBRARIES", "1")
                .output()
                .unwrap();
            assert!(String::from_utf8_lossy(&output.stderr).contains("libsystem_kernel.dylib"));
        }

        #[test]
        fn patch_script_with_shebang() {
            let mut original_file = tempfile::NamedTempFile::new().unwrap();
            let patched_path = temp_dir().join(original_file.path().strip_prefix("/").unwrap());
            original_file
                .write("#!/usr/bin/env bash\n".as_ref())
                .unwrap();
            original_file.flush().unwrap();
            std::fs::create_dir_all(patched_path.parent().unwrap()).unwrap();
            patch_script(original_file.path(), &patched_path, "/test/shebang").unwrap();
            let new_contents = std::fs::read(&patched_path).unwrap();
            assert_eq!(new_contents, "#!/test/shebang bash\n".as_bytes())
        }

        #[test]
        fn shebang_from_string() {
            let contents = "#!/usr/bin/env bash\n".to_string();
            assert_eq!(get_shebang_from_string(&contents).unwrap(), "#!/usr/bin/env")
        }
    }
}

#[cfg(target_os = "macos")]
pub use main::*;
