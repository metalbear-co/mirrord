#![warn(clippy::indexing_slicing)]
#![cfg(target_os = "macos")]

mod codesign;
mod error;
mod whitespace;

mod main {
    use std::{
        collections::HashSet,
        env::temp_dir,
        io::{self, Read},
        os::{macos::fs::MetadataExt, unix::fs::PermissionsExt},
        path::{Path, PathBuf},
    };

    use apple_codesign::{CodeSignatureFlags, MachFile};
    use object::{
        macho::{self, FatHeader, MachHeader64},
        read::macho::{FatArch, MachHeader},
        Architecture, Endianness, FileKind,
    };
    use once_cell::sync::Lazy;
    use tracing::trace;
    use which::which;

    use super::*;
    pub use crate::error::SipError;
    use crate::{
        error::Result,
        SipError::{CyclicShebangs, FileNotFound, UnlikelyError},
    };

    /// Where patched files are stored, relative to the temp dir (`/tmp/mirrord-bin/...`).
    pub const MIRRORD_PATCH_DIR: &str = "mirrord-bin";

    /// The path of mirrord's internal temp binary dir, where we put SIP-patched binaries and
    /// scripts.
    pub static MIRRORD_TEMP_BIN_DIR: Lazy<String> = Lazy::new(|| {
        std::env::temp_dir()
            .join(MIRRORD_PATCH_DIR)
            // lossy: we assume our temp dir path does not contain non-unicode chars.
            .to_string_lossy()
            .to_string()
            .trim_end_matches('/')
            .to_string()
    });

    /// Path of current executable, None if fetching failed.
    pub static CURRENT_EXE: Lazy<Option<String>> = Lazy::new(|| {
        std::env::current_exe()
            .ok()
            .map(|path_buf| path_buf.to_string_lossy().to_string())
    });

    /// Check if a cpu subtype (already parsed with the correct endianness) is arm64e, given its
    /// main cpu type is arm64. We only consider the lowest byte in the check.
    #[cfg(target_arch = "aarch64")]
    fn is_cpu_subtype_arm64e(subtype: u32) -> bool {
        // We only compare the lowest 8 bit since the higher bits may contain "capability bits".
        // For example, usually arm64e would be
        // `macho::CPU_SUBTYPE_ARM64E | macho::CPU_SUBTYPE_PTRAUTH_ABI`.
        // Maybe we could also use arm64e binaries without pointer authentication, but we don't
        // know if that exists in the wild so it was decided not to work with any arm64e
        // binaries for now.
        subtype as u8 == macho::CPU_SUBTYPE_ARM64E as u8
    }

    /// Return whether a binary that is a member of a fat binary is arm64 (not arm64e).
    /// We don't include arm64e because the SIP patching trick does not work with arm64e binaries.
    #[cfg(target_arch = "aarch64")]
    fn is_fat_arm64_arch(arch: &&impl FatArch) -> bool {
        matches!(arch.architecture(), Architecture::Aarch64)
            && !is_cpu_subtype_arm64e(arch.cpusubtype())
    }

    /// Return whether a binary that is a member of a fat binary is x64.
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

        /// Takes the cpu type and subtype and the bytes of a file that is a non-fat Mach-O, and
        /// returns either an Ok BinaryInfo (which points to the whole file, as it is not
        /// part of a fat file) if it is of a supported CPU architecture, or a
        /// `SipError::NoSupportedArchitecture` otherwise.
        fn from_thin_mach_o(
            cpu_type: u32,
            #[cfg(target_arch = "aarch64")] cpu_subtype: u32,
            bytes: &[u8],
        ) -> Result<Self> {
            // When running with apple chips both arm64 and x64 can run.
            #[cfg(target_arch = "aarch64")]
            let is_supported = (cpu_type == macho::CPU_TYPE_X86_64
                || cpu_type == macho::CPU_TYPE_ARM64)
                && !is_cpu_subtype_arm64e(cpu_subtype);

            // When running with intel chips, only x86_64 can run.
            #[cfg(target_arch = "x86_64")]
            let is_supported = cpu_type == macho::CPU_TYPE_X86_64;

            if is_supported {
                Ok(Self::new(0, bytes.len()))
            } else {
                Err(SipError::NoSupportedArchitecture)
            }
        }

        /// Return the `BinaryInfo` of a supported binary from the file: if the file is a "thin"
        /// binary of a supported architecture then the offset is 0 and the length is the length
        /// of the file.
        ///
        /// If the file is a fat binary, then if it contains an arm64 binary (not arm64e) the info
        /// of that binary is returned. If there is no arm64 but there is an x64 binary, then the
        /// info of that binary is returned.
        ///
        /// # Errors
        ///
        /// - [`SipError::UnsupportedFileFormat`] if the file is not a valid Mach-O file.
        /// - [`SipError::NoSupportedArchitecture`] if the file does not contain any binary from a
        ///   supported architecture (arm64, x64).
        fn from_object_bytes(bytes: &[u8]) -> Result<Self> {
            match FileKind::parse(bytes)? {
                FileKind::MachO64 => {
                    let header: &MachHeader64<Endianness> =
                        MachHeader::parse(bytes, 0).map_err(|_| {
                            SipError::UnsupportedFileFormat(
                                "MachO 64 file parsing failed".to_string(),
                            )
                        })?;
                    Self::from_thin_mach_o(
                        header.cputype(Endianness::default()),
                        #[cfg(target_arch = "aarch64")]
                        header.cpusubtype(Endianness::default()),
                        bytes,
                    )
                }

                // This is probably where most fat binaries are handled (see comment above
                // `MachOFat64`). A 32 bit archive (fat Mach-O) can contain 64 bit binaries.
                FileKind::MachOFat32 => {
                    let fat_slice = FatHeader::parse_arch32(bytes).map_err(|_| {
                        SipError::UnsupportedFileFormat("FatMach-O 32-bit".to_string())
                    })?;
                    #[cfg(target_arch = "aarch64")]
                    let found_arch = fat_slice
                        .iter()
                        .find(is_fat_arm64_arch)
                        .or_else(|| fat_slice.iter().find(is_fat_x64_arch));

                    #[cfg(target_arch = "x86_64")]
                    let found_arch = fat_slice.iter().find(is_fat_x64_arch);

                    found_arch
                        .map(|arch| Self::new(arch.offset() as usize, arch.size() as usize))
                        .ok_or(SipError::NoSupportedArchitecture)
                }

                // It seems like 64 bit fat Mach-Os are only used (if at all) when one of the
                // binaries has 2^32 bytes or more (around 4 GB).
                // See https://github.com/Homebrew/ruby-macho/issues/101#issuecomment-403202114
                FileKind::MachOFat64 => {
                    let fat_slice = FatHeader::parse_arch64(bytes).map_err(|_| {
                        SipError::UnsupportedFileFormat("Mach-O 32-bit".to_string())
                    })?;

                    #[cfg(target_arch = "aarch64")]
                    let found_arch = fat_slice
                        .iter()
                        .find(is_fat_arm64_arch)
                        .or_else(|| fat_slice.iter().find(is_fat_x64_arch));

                    #[cfg(target_arch = "x86_64")]
                    let found_arch = fat_slice.iter().find(is_fat_x64_arch);

                    found_arch
                        .map(|arch| Self::new(arch.offset() as usize, arch.size() as usize))
                        .ok_or(SipError::NoSupportedArchitecture)
                }
                other => Err(SipError::UnsupportedFileFormat(format!("{other:?}"))),
            }
        }
    }

    /// Read the contents (or just the x86_64 section in case of a fat file) from the SIP binary at
    /// `path`, write it into `output`, give it the same permissions, and sign the new binary.
    fn patch_binary<P: AsRef<Path>, K: AsRef<Path>>(path: P, output: K) -> Result<()> {
        let data = std::fs::read(path.as_ref())?;

        // Propagate Err if the binary does not contain any supported architecture (x64/arm64).
        let binary_info = BinaryInfo::from_object_bytes(&data)?;

        // Just the thin binary - if the file was a thin binary of a supported architecture to
        // begin with then it's the whole file, if its a fat binary then it's just a part of it.
        let binary = data
            .get(binary_info.offset..binary_info.offset + binary_info.size)
            .expect("invalid SIP binary");
        std::fs::write(output.as_ref(), binary)?;
        // Give the new file the same permissions as the old file.
        std::fs::set_permissions(
            output.as_ref(),
            std::fs::metadata(path.as_ref())?.permissions(),
        )?;
        codesign::sign(output)
    }

    /// Create a new file at `patched_path` with the same contents as `original_path` except for
    /// the shebang which is `new_shebang`.
    fn patch_script<P: AsRef<Path>, K: AsRef<Path>>(
        original_path: P,
        patched_path: K,
        new_shebang: &str,
    ) -> Result<()> {
        read_shebang_from_file(original_path.as_ref())?
            .map(|original_shebang| -> Result<()> {
                let data = std::fs::read(original_path.as_ref())?;
                let contents = data
                    .get(original_shebang.len()..)
                    .expect("original shebang size exceeds file size");
                let mut new_contents = String::from("#!") + new_shebang;
                new_contents.push_str(std::str::from_utf8(contents).map_err(|_utf| {
                    UnlikelyError("Can't read script contents as utf8".to_string())
                })?);
                std::fs::write(patched_path.as_ref(), new_contents)?;

                // We set the permissions of the patched script to be like those of the original
                // script, but allowing the user to write, so that in the next run, when we are here
                // again, we have permission to overwrite the patched script (we rewrite the script
                // every run, in case it is a user script that was changed).
                let mut permissions = std::fs::metadata(&original_path)?.permissions();
                let mode = permissions.mode() | 0o200; // user can write.
                permissions.set_mode(mode);
                std::fs::set_permissions(patched_path.as_ref(), permissions)?;
                Ok(())
            })
            .ok_or_else(|| UnlikelyError("Can't read shebang anymore.".to_string()))?
    }

    const SF_RESTRICTED: u32 = 0x00080000; // entitlement required for writing, from stat.h (macos)

    /// Extract shebang from file contents.
    /// "#!/usr/bin/env bash\n..." -> Some("#!/usr/bin/env")
    fn get_shebang_from_string(file_contents: &str) -> Option<String> {
        const BOM: &str = "\u{feff}";
        let content = file_contents.strip_prefix(BOM).unwrap_or(file_contents);
        let rest = content.strip_prefix("#!")?;

        if whitespace::skip(rest).starts_with('[') {
            None
        } else {
            content
                .split_once('\n')
                .map(|(line, _)| line)
                .unwrap_or(content)
                .split_whitespace()
                .next()
                .map(ToString::to_string)
        }
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

    /// Checks if binary is signed with either `RUNTIME` or `RESTRICTED` flags.
    /// The code ignores error to allow smoother fallbacks.
    fn is_code_signed(path: &PathBuf) -> bool {
        let data = match std::fs::read(path) {
            Ok(data) => data,
            Err(_) => return false,
        };

        if let Ok(mach) = MachFile::parse(data.as_ref()) {
            for macho in mach.into_iter() {
                if let Ok(Some(signature)) = macho.code_signature() {
                    if let Ok(Some(blob)) = signature.code_directory() {
                        if blob
                            .flags
                            .intersects(CodeSignatureFlags::RESTRICT | CodeSignatureFlags::RUNTIME)
                        {
                            return true;
                        }
                    }
                }
            }
        }
        false
    }

    /// Determine status recursively, keep seen_paths, and return an error if there is a cyclical
    /// reference.
    fn get_sip_status_rec(
        path: &str,
        seen_paths: &mut HashSet<PathBuf>,
        patch_binaries: &Vec<String>,
    ) -> Result<SipStatus> {
        // If which fails, try using the given path as is.
        let complete_path = which(path).unwrap_or_else(|_| PathBuf::from(&path));
        if !complete_path.exists() {
            return Err(FileNotFound(complete_path.to_string_lossy().to_string()));
        }
        let canonical_path = complete_path.canonicalize()?;

        // TODO: Don't recursively follow the shebangs, instead only read the first one because on
        //  macOS a shebang cannot lead to a script only to a binary. Then there should be no danger
        //  of recursing over a cycle of shebangs until a stack overflow, so this check and keeping
        //  the seen_paths could all be removed.
        if seen_paths.contains(&canonical_path) {
            return Err(CyclicShebangs(canonical_path.to_string_lossy().to_string()));
        }
        seen_paths.insert(canonical_path);

        // Patch binary if it is in the list of binaries to patch.
        // See `ends_with` docs for understanding better when it returns true.
        if patch_binaries.iter().any(|x| complete_path.ends_with(x)) {
            return Ok(SipStatus::SomeSIP(complete_path, None));
        }

        if is_code_signed(&complete_path) {
            return Ok(SipStatus::SomeSIP(complete_path, None));
        }

        let metadata = std::fs::metadata(&complete_path)?;
        if (metadata.st_flags() & SF_RESTRICTED) > 0 {
            return Ok(SipStatus::SomeSIP(complete_path, None));
        }
        if let Some(shebang) = read_shebang_from_file(&complete_path)? {
            // Start from index 2 of shebang to get only the path.
            let path = shebang.strip_prefix("#!").unwrap_or(&shebang);
            return match get_sip_status_rec(path, seen_paths, patch_binaries)? {
                // The file at the end of the shebang chain is not protected.
                SipStatus::NoSIP => Ok(SipStatus::NoSIP),
                some_sip => Ok(SipStatus::SomeSIP(complete_path, Some(Box::new(some_sip)))),
            };
        }
        Ok(SipStatus::NoSIP)
    }

    /// Checks the SF_RESTRICTED flags on a file (there might be a better check, feel free to
    /// suggest)
    /// If file is a script with shebang, the SipStatus is derived from the the SipStatus of the
    /// file the shebang points to.
    fn get_sip_status(path: &str, patch_binaries: &Vec<String>) -> Result<SipStatus> {
        let mut seen_paths = HashSet::new();
        get_sip_status_rec(path, &mut seen_paths, patch_binaries)
    }

    /// Only call this function on a file that is SomeSIP.
    /// Patch shebang scripts recursively and patch final binary.
    fn patch_some_sip(
        path: &PathBuf,
        shebang_target: Option<Box<SipStatus>>,
        tmp_dir: PathBuf,
    ) -> Result<String> {
        // TODO: Change output to be with hash of the contents, so that old versions of changed
        //       files do not get used. (Also change back existing file logic to always use.)

        // if which does not work, just use the given path as is.
        let complete_path =
            which(path.to_string_lossy().to_string()).unwrap_or_else(|_| path.to_owned());

        // Strip root path from binary path, as when joined it will clear the previous.
        let output = &tmp_dir.join(
            complete_path.strip_prefix("/").unwrap_or(&complete_path), // No prefix - no problem.
        );

        // A string of the path of new created file to run instead of the SIPed file.
        let patched_path_string = output
            .to_str()
            .ok_or_else(|| UnlikelyError("Failed to convert path to string".to_string()))?
            .to_string();

        if output.exists() {
            // TODO: Remove this `if` (leave contents) when we have content hashes in paths.
            //       For now don't use existing scripts because 1. they're usually not as large as
            //       binaries so rewriting them is not as bad, and 2. they're more likely to change.
            if shebang_target.is_none() {
                // Only use existing if binary (not a script).
                trace!(
                    "Using existing SIP-patched version of {:?}: {}",
                    path,
                    patched_path_string
                );
                return Ok(patched_path_string);
            }
        }

        std::fs::create_dir_all(
            output
                .parent()
                .ok_or_else(|| UnlikelyError("Failed to get parent directory".to_string()))?,
        )?;

        match shebang_target {
            None => {
                // The file is a sip protected binary.
                trace!(
                    "{:?} is a SIP protected binary, making non protected version at: {}",
                    path,
                    patched_path_string
                );
                patch_binary(path, output)?;
                Ok(patched_path_string)
            }
            // The file is a script with a shebang. Patch recursively.
            Some(sip_file) => {
                if let SipStatus::SomeSIP(target_path, shebang_target) = *sip_file {
                    trace!(
                        "{:?} is a script with a shebang that leads to a SIP protected binary.",
                        path
                    );
                    trace!(
                        "Shebang points to: {:?}. Patching it recursively and making a version of {:?} with an altered shebang at: {}",
                        target_path,
                        path,
                        patched_path_string,
                    );
                    let new_target = patch_some_sip(&target_path, shebang_target, tmp_dir)?;
                    patch_script(path, output, &new_target)?;
                    Ok(patched_path_string)
                } else {
                    // This function should only be called on a file which has SomeSIP SipStatus.
                    // If the file has a shebang pointing to a file which is NoSIP, this file should
                    // not have SomeSIP status in the first place.
                    Err(UnlikelyError("Internal mirrord error.".to_string()))
                }
            }
        }
    }

    /// Check if the file that the user wants to execute is a SIP protected binary (or a script
    /// starting with a shebang that leads to a SIP protected binary).
    /// If it is, create a non-protected version of the file and return `Ok(Some(patched_path)`.
    /// If it is not, `Ok(None)`.
    /// Propagate errors.
    pub fn sip_patch(binary_path: &str, patch_binaries: &Vec<String>) -> Result<Option<String>> {
        match get_sip_status(binary_path, patch_binaries) {
            Ok(SipStatus::SomeSIP(path, shebang_target)) => {
                let tmp_dir = temp_dir().join(MIRRORD_PATCH_DIR);
                trace!("Using temp dir: {:?} for sip patches", &tmp_dir);
                Some(patch_some_sip(&path, shebang_target, tmp_dir)).transpose()
            }
            Ok(SipStatus::NoSIP) => {
                trace!("No SIP detected on {:?}", binary_path);
                Ok(None)
            }
            Err(err) => {
                trace!(
                    "Checking the SIP status of {binary_path} (or of the binary in its shebang, if \
                    applicable) failed with {err:?}. Continuing without SIP-sidestepping.\
                    This is not necessarily an error."
                );
                // E.g. `env` tries to execute a bunch of non-existing files and fails, and that's
                // just its valid flow.
                Ok(None)
            }
        }
    }

    #[cfg(test)]
    mod tests {

        use std::io::Write;

        use super::*;

        #[test]
        fn is_sip_true() {
            assert!(matches!(
                get_sip_status("/bin/ls", &vec![]),
                Ok(SipStatus::SomeSIP(_, _))
            ));
        }

        #[test]
        fn is_sip_false() {
            let mut f = tempfile::NamedTempFile::new().unwrap();
            let data = std::fs::read("/bin/ls").unwrap();
            f.write_all(&data).unwrap();
            f.flush().unwrap();
            assert!(matches!(
                get_sip_status(f.path().to_str().unwrap(), &vec![]).unwrap(),
                SipStatus::NoSIP
            ));
        }

        #[test]
        fn is_sip_notfound() {
            let err =
                get_sip_status("/donald/duck/was/a/duck/not/a/quack/a/duck", &vec![]).unwrap_err();
            assert!(err.to_string().contains("executable file not found"));
        }

        #[test]
        fn patch_binary_fat() {
            let path = "/bin/ls";
            let output = "/tmp/ls_mirrord_test";
            patch_binary(path, output).unwrap();
            assert!(matches!(
                get_sip_status(output, &vec![]).unwrap(),
                SipStatus::NoSIP
            ));
            // Check DYLD_* features work on it:
            let output = std::process::Command::new(output)
                .env("DYLD_PRINT_LIBRARIES", "1")
                .output()
                .unwrap();
            assert!(String::from_utf8_lossy(&output.stderr).contains("libsystem_kernel.dylib"));
        }

        /// Test that when a fat binary contains an arm64 binary, that binary is used and patching
        /// works.
        ///
        /// This assumes `/usr/bin/file` is present and contains an arm64 binary.
        #[test]
        fn patch_binary_fat_with_arm64() {
            let path = "/usr/bin/file";
            let patched_path = "/tmp/ls_mirrord_test_arm64";
            patch_binary(path, patched_path).unwrap();
            assert!(matches!(
                get_sip_status(patched_path, &vec![]).unwrap(),
                SipStatus::NoSIP
            ));
            // Check DYLD_* features work on it:
            let output = std::process::Command::new(patched_path)
                .env("DYLD_PRINT_LIBRARIES", "1")
                .output()
                .unwrap();
            assert!(String::from_utf8_lossy(&output.stderr).contains("libsystem_kernel.dylib"));

            // Check that the binary was chosen according to the architecture.
            let data = std::fs::read(patched_path).unwrap();
            let file_kind = FileKind::parse(&data[..]).unwrap();
            assert_eq!(file_kind, FileKind::MachO64);
            let header: &MachHeader64<Endianness> = MachHeader::parse(&data[..], 0).unwrap();
            let cpu_type = header.cputype(Endianness::default());
            #[cfg(target_arch = "aarch64")]
            assert_eq!(cpu_type, macho::CPU_TYPE_ARM64);
            #[cfg(target_arch = "x86_64")]
            assert_eq!(cpu_type, macho::CPU_TYPE_X86_64);
        }

        #[test]
        fn patch_script_with_shebang() {
            let mut original_file = tempfile::NamedTempFile::new().unwrap();
            let patched_path = temp_dir().join(original_file.path().strip_prefix("/").unwrap());
            original_file
                .write_all("#!/usr/bin/env bash\n".as_ref())
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
            assert_eq!(
                get_shebang_from_string(&contents).unwrap(),
                "#!/usr/bin/env"
            )
        }

        /// Run `sip_patch` on a script with a shebang that points to `env`, verify that a path to
        /// a new script is returned, in which the shebang points to a patched version of `env`
        /// that is not SIPed.
        #[test]
        fn patch_shebang_and_binary() {
            let mut script = tempfile::NamedTempFile::new().unwrap();
            let script_contents = "#!/usr/bin/env bash\nexit\n";
            script.write_all(script_contents.as_ref()).unwrap();
            script.flush().unwrap();
            let changed_script_path = sip_patch(script.path().to_str().unwrap(), &Vec::new())
                .unwrap()
                .unwrap();
            let new_shebang = read_shebang_from_file(changed_script_path)
                .unwrap()
                .unwrap();
            let patched_env_binary_path = &new_shebang[2..];
            // Check DYLD_* features work on it:
            let output = std::process::Command::new(patched_env_binary_path)
                .env("DYLD_PRINT_LIBRARIES", "1")
                .output()
                .unwrap();
            assert!(String::from_utf8_lossy(&output.stderr).contains("libsystem_kernel.dylib"));
        }

        /// Test that patching the same script twice does not lead to an error.
        /// This is a regression test for a bug where patching a script to which the user did not
        /// have write permissions would fail the second time, because we could not overwrite the
        /// patched script, due to lack of write permissions.
        #[test]
        fn patch_twice() {
            let mut script = tempfile::NamedTempFile::new().unwrap();
            let script_contents = "#!/bin/bash\nexit\n";
            script.write_all(script_contents.as_ref()).unwrap();
            script.flush().unwrap();

            let path = script.path();

            let permissions = std::fs::Permissions::from_mode(0o555); // read and execute, no write.
            std::fs::set_permissions(path, permissions).unwrap();

            let path_str = path.to_str().unwrap();
            let _ = sip_patch(path_str, &Vec::new()).unwrap().unwrap();
            let _ = sip_patch(path_str, &Vec::new()).unwrap().unwrap();
        }

        /// Run `sip_patch` on a file that has a shebang that points to itself and verify that we
        /// don't get stuck in a recursion until the stack overflows.
        #[test]
        fn cyclic_shebangs() {
            let mut script = tempfile::NamedTempFile::new().unwrap();
            let contents = "#!".to_string() + script.path().to_str().unwrap();
            script.write_all(contents.as_bytes()).unwrap();
            script.flush().unwrap();
            let res = sip_patch(script.path().to_str().unwrap(), &Vec::new());
            assert!(matches!(res, Ok(None)));
        }
    }
}

pub use main::*;
