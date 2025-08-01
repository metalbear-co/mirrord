#![feature(iter_intersperse)]
#![warn(clippy::indexing_slicing)]
#![cfg(target_os = "macos")]
#![deny(unused_crate_dependencies)]

mod codesign;
mod error;
mod rpath;

/// Concerns MacOS' SIP (System Integrity Protection) mechanism and how to sidestep it.
///
/// ### Why?
///
/// SIP makes it such that the `mirrord-layer` lib cannot be dynamically loaded into the user's
/// process on MacOS machines, as it blocks the modification of processes ([Wikipedia page](https://en.wikipedia.org/wiki/System_Integrity_Protection)). It does this by removing the relevant environment variables (`DYLD_INSERT_LIBRARIES`) when the process starts.
///
/// Also see Apple's relevant docs: [Hardened Runtime](https://developer.apple.com/documentation/security/hardened-runtime), [Entitlements](https://developer.apple.com/documentation/bundleresources/entitlements) and [Code Signing](https://developer.apple.com/documentation/security/code-signing-services).
///
/// To get around this, the user's process is patched by creating a new unprotected version and
/// signing while mirrord is running. Then the layer is loaded dynamically into the copy without
/// SIP and executes as usual.
///
/// - If the process is a **script**, the shebang must be changed to point to the patched version of
///   the interpreter.
///
/// ### Usage
///
/// Used by `mirrord-cli` and `mirrord-layer`. Called into before the user process is
/// run, during setup on MacOS machines and in the exec hooks in case the program
/// runs a protected binary - see mirrord::execution::MirrordExecution::start_internal,
/// mirrord_layer::exec_hooks::hooks::execve_detour and mirrord_layer::exec_utils::patch_if_sip.
///
/// The entry point when calling this crate is [`main::sip_patch`].
///
/// ### Gotchas and spike pits
///
/// - The directory where patched files exist is not randomly named, but uses the const
///   [`main::MIRRORD_PATCH_DIR`] due to an issue with the temp dir changing between executions.
/// - A shebang is added to scripts without one in order to point it to the patched binary.
/// - When checking a script, only the first line of the file is checked for a shebang, in case the
///   script is encoded unusually.
/// - Some errors may prevent logs from printing out, making it harder to diagnose the issue. In
///   this case, use the `experimental.sip_log_destination` config to print SIP logs to a file.
mod main {
    use std::{
        env,
        ffi::{OsStr, OsString},
        fs::{File, OpenOptions},
        io::{self, BufRead, ErrorKind, Write},
        os::{macos::fs::MetadataExt, unix::fs::PermissionsExt},
        path::{Path, PathBuf},
        str::from_utf8,
        time::{SystemTime, UNIX_EPOCH},
    };

    use apple_codesign::{CodeSignatureFlags, MachFile};
    use fs4::fs_std::FileExt;
    use object::{
        macho::{self, MachHeader64, LC_RPATH},
        read::{
            self,
            macho::{FatArch, LoadCommandVariant::Rpath, MachHeader},
        },
        Architecture, Endianness, FileKind,
    };
    use once_cell::sync::Lazy;
    use tracing::{trace, warn};
    use which::which;

    use super::*;
    pub use crate::error::SipError;
    use crate::{
        error::Result,
        main::SipStatus::{NoSip, SipBinary, SipScript},
        SipError::{FileNotFound, UnlikelyError},
    };

    /// Where patched files are stored, relative to the temp dir (`/tmp/mirrord-bin/...`).
    ///
    /// We added some random characters to the end so we'll be able to identify dir better
    /// in situations where $TMPDIR changes between execs, leading to strip not working
    /// <https://github.com/metalbear-co/mirrord/issues/2500#issuecomment-2160026642>
    ///
    /// The version of mirrord is also added to the dir name, to force mirrord to re-patch files
    /// when there is a version change. This is to prevent badly patched files being re-used
    /// <https://github.com/metalbear-co/mirrord/issues/3245>
    pub static MIRRORD_PATCH_DIR: &str =
        concat!("mirrord-bin-ghu3278mz-", env!("CARGO_PKG_VERSION"));

    pub const FRAMEWORKS_ENV_VAR_NAME: &str = "DYLD_FALLBACK_FRAMEWORK_PATH";

    /// The path of mirrord's internal temp binary dir, where we put SIP-patched binaries and
    /// scripts. Uses `temp_dir()`/mirrord/ because this is where the layer is extracted
    pub static MIRRORD_TEMP_BIN_DIR_PATH_BUF: Lazy<PathBuf> =
        Lazy::new(|| env::temp_dir().join("mirrord").join(MIRRORD_PATCH_DIR));

    /// Get the `PathBuf` of the `mirrord-bin` dir, and return a `String` prefix to remove, without
    /// a trailing `/`, so that the stripped path starts with a `/`
    fn get_temp_bin_str_prefix(path: &Path) -> String {
        // lossy: we assume our temp dir path does not contain non-unicode chars.
        path.to_string_lossy()
            .to_string()
            .trim_end_matches('/')
            .to_string()
    }

    /// The string path of mirrord's internal temp binary dir, where we put SIP-patched binaries and
    /// scripts, without a trailing `/`.
    pub static MIRRORD_TEMP_BIN_DIR_STRING: Lazy<String> =
        Lazy::new(|| get_temp_bin_str_prefix(&MIRRORD_TEMP_BIN_DIR_PATH_BUF));

    /// Check if a cpu subtype (already parsed with the correct endianness) is arm64e, given its
    /// main cpu type is arm64. We only consider the lowest byte in the check.
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

    /// Options for the [main::sip_patch] function.
    #[derive(Default)]
    pub struct SipPatchOptions<'a> {
        /// A list of binaries to patch.
        pub patch: &'a [String],
        /// A list of binaries to skip patching.
        pub skip: &'a [String],
    }

    /// Info for logging to `config.experimental.sip_log_destination`
    pub struct SipLogInfo<'a> {
        /// The file destination to write logs to
        pub log_destination: &'a Path,
        /// Args to the binary, if available
        pub args: Option<&'a [OsString]>,
        /// The load type, if available
        pub load_type: Option<&'a str>,
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
        fn from_thin_mach_o(cpu_type: u32, cpu_subtype: u32, bytes: &[u8]) -> Result<Self> {
            if cpu_type == macho::CPU_TYPE_X86_64
                || (cpu_type == macho::CPU_TYPE_ARM64 && !is_cpu_subtype_arm64e(cpu_subtype))
            {
                // The binary has an architecture we know how to patch, so proceed.
                // We don't check if this is a thin arm binary on an intel chip because that would
                // not run regardless of SIP.
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
                        header.cpusubtype(Endianness::default()),
                        bytes,
                    )
                }

                // This is probably where most fat binaries are handled (see comment above
                // `MachOFat64`). A 32 bit archive (fat Mach-O) can contain 64 bit binaries.
                FileKind::MachOFat32 => {
                    let fat_slice = read::macho::MachOFatFile32::parse(bytes).map_err(|_| {
                        SipError::UnsupportedFileFormat("FatMach-O 32-bit".to_string())
                    })?;
                    #[cfg(target_arch = "aarch64")]
                    let found_arch = fat_slice
                        .arches()
                        .iter()
                        .find(is_fat_arm64_arch)
                        .or_else(|| fat_slice.arches().iter().find(is_fat_x64_arch));

                    #[cfg(target_arch = "x86_64")]
                    let found_arch = fat_slice.arches().iter().find(is_fat_x64_arch);

                    found_arch
                        .map(|arch| Self::new(arch.offset() as usize, arch.size() as usize))
                        .ok_or(SipError::NoSupportedArchitecture)
                }

                // It seems like 64 bit fat Mach-Os are only used (if at all) when one of the
                // binaries has 2^32 bytes or more (around 4 GB).
                // See https://github.com/Homebrew/ruby-macho/issues/101#issuecomment-403202114
                FileKind::MachOFat64 => {
                    let fat_slice = read::macho::MachOFatFile64::parse(bytes).map_err(|_| {
                        SipError::UnsupportedFileFormat("Mach-O 32-bit".to_string())
                    })?;

                    #[cfg(target_arch = "aarch64")]
                    let found_arch = fat_slice
                        .arches()
                        .iter()
                        .find(is_fat_arm64_arch)
                        .or_else(|| fat_slice.arches().iter().find(is_fat_x64_arch));

                    #[cfg(target_arch = "x86_64")]
                    let found_arch = fat_slice.arches().iter().find(is_fat_x64_arch);

                    found_arch
                        .map(|arch| Self::new(arch.offset() as usize, arch.size() as usize))
                        .ok_or(SipError::NoSupportedArchitecture)
                }
                other => Err(SipError::UnsupportedFileFormat(format!("{other:?}"))),
            }
        }
    }

    /// Get a vector of strings, with a string for each `LC_RPATH` command in `binary`.
    /// Returns errors if the binary is not a 64 bit thin binary, or if the load commands could not
    /// be parsed correctly.
    ///
    /// # Arguments
    /// * `binary`: a slice of just the bytes in a thin 64 bit binary. If the file is a thin binary
    ///   in the first place, the slice will be the whole file. If the file is a fat binary, the
    ///   slice will be just the thin binary we're going to use out of the fat binary.
    fn get_rpath_entries(binary: &[u8]) -> Result<Vec<&str>> {
        let mach_header: &MachHeader64<Endianness> = // we don't support 32 bit binaries.
            // offset 0 - `binary` should only be the thin binary (not the containing fat one).
            MachHeader::parse(binary, 0).map_err(|_| {
                SipError::UnsupportedFileFormat(
                    "MachO 64 file parsing failed".to_string(),
                )
            })?;
        let mut load_commands = mach_header.load_commands(Endianness::default(), binary, 0)?;
        let mut rpath_entries = Vec::new();
        while let Some(load_command) = load_commands.next()? {
            if load_command.cmd() == LC_RPATH {
                if let Ok(Rpath(rpath_command)) = load_command.variant() {
                    rpath_entries.push(from_utf8(
                        load_command.string(Endianness::default(), rpath_command.path)?,
                    )?)
                }
            }
        }
        Ok(rpath_entries)
    }

    /// For each rpath entry in the original path, if that path is in `@executable_path` or
    /// `@loader_path`, add a new rpath entry to the file, with that special path replaced with the
    /// directory of the original binary.
    /// E.g. if the original binary is in `/bin/original/binary`, and it has 2 rpath entries:
    /// - `@loader_path/.`
    /// - `@loader_path/../lib`
    ///
    /// We'll add to the binary at the output path the following rpath entries:
    /// - `/bin/original/.`
    /// - `/bin/original/../lib`
    ///
    /// So the output binary has all 4 entries.
    fn add_rpath_entries(
        original_entries: &[&str],
        original_path: &Path,
        output_path: &Path,
    ) -> Result<()> {
        let parent_path_str = original_path
            .parent()
            .unwrap_or(original_path)
            .to_string_lossy()
            .to_string();
        let new_entries = original_entries
            .iter()
            .filter_map(|path| {
                path.strip_prefix("@executable_path")
                    .or_else(|| path.strip_prefix("@loader_path"))
            })
            .map(|stripped_path| parent_path_str.clone() + stripped_path)
            .collect();
        rpath::add_rpaths(output_path, new_entries)
    }

    /// Read the contents (or just the x86_64 section in case of a fat file) from the SIP binary at
    /// `path`, write it into `output`, give it the same permissions, and sign the new binary.
    fn patch_binary(path: &Path) -> Result<PathBuf> {
        set_fallback_frameworks_path_if_mac_app(path);

        let output = get_output_path(path)?;

        if output.exists() {
            trace!(
                "Using existing SIP-patched version of {:?}: {:?}",
                path,
                output
            );
            return Ok(output);
        }

        // If the same file is executed in parallel, parallel signing could fail. So do the work on
        // a temp file, and then move it to its final destination once ready and signed.
        let temp_binary = tempfile::NamedTempFile::new()?;

        trace!(
            "{:?} is a SIP protected binary, making non protected version at: {:?}",
            path,
            output
        );
        let data = std::fs::read(path)?;

        // Propagate Err if the binary does not contain any supported architecture (x64/arm64).
        let binary_info = BinaryInfo::from_object_bytes(&data)?;

        // Just the thin binary - if the file was a thin binary of a supported architecture to
        // begin with then it's the whole file, if its a fat binary then it's just a part of it.
        let binary = data
            .get(binary_info.offset..binary_info.offset + binary_info.size)
            .expect("invalid SIP binary");

        std::fs::write(&temp_binary, binary)?;

        if let Err(err) = get_rpath_entries(binary)
            .and_then(|rpath_entries| add_rpath_entries(&rpath_entries, path, temp_binary.as_ref()))
        {
            warn!("Adding Rpath loader commands to SIP-patched binary failed with {err:?}.")
            // Not stopping the SIP-patching as most binaries don't need the rpath fix.
        }

        let signed_temp_file = tempfile::NamedTempFile::new()?;
        codesign::sign(&temp_binary, &signed_temp_file)?;

        // Give the new file the same permissions as the old file.
        // This needs to happen after the sign because it might change the permissions.
        trace!("Setting permissions for {temp_binary:?}");
        std::fs::set_permissions(&signed_temp_file, std::fs::metadata(path)?.permissions())?;

        // Move the temp binary into its final location if no other process/thread already did.
        if let Err(err) = signed_temp_file.persist_noclobber(&output) {
            if err.error.kind() != std::io::ErrorKind::AlreadyExists {
                return Err(SipError::BinaryMoveFailed(err.error));
            }
        }
        Ok(output)
    }

    /// Create a new file at `patched_path` with the same contents as `original_path` except for
    /// the shebang which is `new_shebang`.
    fn patch_script(
        original_path: &Path,
        shebang: ScriptShebang,
        new_shebang: &str,
    ) -> Result<PathBuf> {
        let patched_path = get_output_path(original_path)?;

        trace!(
            "Shebang points to: {:?}. Patching the interpreter and making a version of {:?} with an altered shebang at: {:?}",
            shebang.interpreter_path,
            original_path,
            patched_path,
        );

        let data = std::fs::read(original_path)?;
        let contents = data
            .get(shebang.start_of_rest_of_file..)
            .expect("original shebang size exceeds file size");
        let mut new_contents = String::from("#!") + new_shebang;
        if shebang.start_of_rest_of_file == 0 {
            // trailing newline is needed for scripts without an original shebang
            new_contents.push('\n');
        }
        let mut bytes = new_contents.into_bytes();
        bytes.extend_from_slice(contents);

        std::fs::write(&patched_path, bytes)?;

        // We set the permissions of the patched script to be like those of the original
        // script, but allowing the user to write, so that in the next run, when we are here
        // again, we have permission to overwrite the patched script (we rewrite the script
        // every run, in case it is a user script that was changed).
        let mut permissions = std::fs::metadata(original_path)?.permissions();
        let mode = permissions.mode() | 0o200; // user can write.
        permissions.set_mode(mode);
        std::fs::set_permissions(&patched_path, permissions)?;
        Ok(patched_path)
    }

    const SF_RESTRICTED: u32 = 0x00080000; // entitlement required for writing, from stat.h (macos)

    /// Extract shebang from first line of file.
    fn get_shebang_from_string(first_line: &str) -> Option<ScriptShebang> {
        let rest = first_line.strip_prefix("#!")?;

        let mut char_iter = rest.char_indices().skip_while(|(_, c)| c.is_whitespace()); // any whitespace directly after #!
        let (start_of_path, _first_char_of_path) = char_iter.next()?;
        let mut path_char_iter = char_iter.skip_while(|(_, c)| !c.is_whitespace()); // Any non-whitespace characters after that (path)

        let (interpreter, len_with_whitespace) =
            if let Some((path_len, _next_char)) = path_char_iter.next() {
                let total_len = path_len + 2; // +2 for #! because the index is in `rest`
                (first_line.get(start_of_path + 2..total_len)?, total_len)
            } else {
                // There is no next character after the interpreter, so the whole file is just
                // magic, whitespace and path.
                (first_line.get(start_of_path + 2..)?, first_line.len())
            };
        Some(ScriptShebang {
            interpreter_path: PathBuf::from(interpreter),
            start_of_rest_of_file: len_with_whitespace,
        })
    }

    /// Including '#!', just until whitespace, no arguments.
    /// will not be called on object files
    fn read_shebang_from_file<P: AsRef<Path>>(path: P) -> Result<Option<ScriptShebang>> {
        let f = std::fs::File::open(path)?;
        let reader = io::BufReader::new(f);
        let mut lines = reader.lines();
        let Some(first_line_res) = lines.next() else {
            trace!("No lines in script.");
            return Ok(None);
        };

        match first_line_res {
            Err(e) if e.kind() == io::ErrorKind::InvalidData => {
                trace!("First line of file is not utf-8. Can't read a shebang.");
                Ok(None)
            }
            Err(e) => Err(SipError::IO(e)),
            Ok(line_string) => Ok(get_shebang_from_string(&line_string)),
        }
    }

    #[derive(Debug)]
    struct ScriptShebang {
        interpreter_path: PathBuf,

        /// The index right after where the path of the interpreter ends.
        /// This is equal to the length of the the magic (`#!`) + any whitespaces + interpreter
        /// path. E.g.:
        ///
        /// !# /usr/bin/env bash
        ///                ^-- Rest of the file starts at index 15.
        start_of_rest_of_file: usize,
    }

    #[derive(Debug)]
    enum SipStatus {
        /// The executable is a script with a shebang that points to a SIP-protected binary.
        SipScript {
            path: PathBuf,
            shebang: ScriptShebang,
        },
        /// The executable is a SIP-protected binary.
        SipBinary(PathBuf),
        /// The binary that ends up being executed is not SIP protected.
        NoSip,
    }

    /// Checks if binary is signed with either `RUNTIME` or `RESTRICTED` flags.
    /// The code ignores error to allow smoother fallbacks.
    fn is_code_signed(data: &[u8]) -> bool {
        if let Ok(mach) = MachFile::parse(data) {
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

    /// SIP check for binaries.
    fn is_binary_sip(path: &Path, data: &[u8], opts: SipPatchOptions) -> Result<bool> {
        // Skip patching binary if it is in the list of binaries to skip.
        if opts.skip.iter().any(|x| path.ends_with(x)) {
            trace!("Skipping SIP patch for {:?}", path);
            return Ok(false);
        }
        // Patch binary if it is in the list of binaries to patch.
        // See `ends_with` docs for understanding better when it returns true.
        Ok(opts.patch.iter().any(|x| path.ends_with(x))
            || is_code_signed(data)
            || (std::fs::metadata(path)?.st_flags() & SF_RESTRICTED) > 0)
    }

    fn get_complete_path<P: AsRef<OsStr> + std::marker::Copy>(path: P) -> Result<PathBuf> {
        // If which fails, try using the given path as is.
        let complete_path = which(path).unwrap_or_else(|_| PathBuf::from(&path));
        if !complete_path.exists() {
            return Err(FileNotFound(complete_path.to_string_lossy().to_string()));
        }
        Ok(complete_path)
    }

    fn is_in_mirrord_tmp_dir(path: &Path) -> Result<bool> {
        let canonical_path = path.canonicalize()?;
        Ok(MIRRORD_TEMP_BIN_DIR_PATH_BUF
            .canonicalize()
            .map(|x| canonical_path.starts_with(x))
            // path might be non-existent yet.
            .unwrap_or_default())
    }

    /// Checks the SF_RESTRICTED flags on a file (there might be a better check, feel free to
    /// suggest)
    /// If file is a script with shebang, the SipStatus is derived from the SipStatus of the
    /// file the shebang points to.
    fn get_sip_status(path: &str, opts: SipPatchOptions) -> Result<SipStatus> {
        let complete_path = get_complete_path(path)?;
        // If the binary is in our temp bin dir, it's not SIP protected.
        if is_in_mirrord_tmp_dir(&complete_path)? {
            return Ok(NoSip);
        }

        // determine if file is object file
        let data = std::fs::read(&complete_path)?;
        if MachFile::parse(data.as_ref()).is_ok() {
            // file is an object file
            is_binary_sip(&complete_path, &data, opts).map(|is_sip| {
                if is_sip {
                    SipBinary(complete_path)
                } else {
                    NoSip
                }
            })
        } else {
            let shebang = read_shebang_from_file(&complete_path)?.unwrap_or_else(|| {
                trace!("Did not find a shebang, defaulting to $SHELL.");
                ScriptShebang {
                    interpreter_path: PathBuf::from(
                        env::var("SHELL").expect("$SHELL should be present"),
                    ),
                    start_of_rest_of_file: 0,
                }
            });

            let interpreter_complete_path = get_complete_path(&shebang.interpreter_path)?;
            if is_in_mirrord_tmp_dir(&interpreter_complete_path)? {
                return Ok(NoSip);
            }
            let data = std::fs::read(&interpreter_complete_path)?;
            is_binary_sip(&interpreter_complete_path, &data, opts).map(|is_sip| {
                if is_sip {
                    SipScript {
                        path: complete_path,
                        shebang,
                    }
                } else {
                    // The interpreter the shebang points to is not protected.
                    NoSip
                }
            })
        }
    }

    /// When patching a bundled mac application, it try to load libraries from its frameworks
    /// directory. The patch might cause it to search under the `mirrord-bin` temp dir.
    ///
    /// To make sure it can find the libraries, we set (or add to) the
    /// `DYLD_FALLBACK_FRAMEWORK_PATH` env var.
    ///
    /// Example, if we're running `/Applications/Postman.app/Contents/MacOS/Postman`, we'll add
    /// `/Applications/Postman.app/Contents/Frameworks` to that env var.
    fn set_fallback_frameworks_path_if_mac_app(path: &Path) {
        for ancestor in path.ancestors() {
            if ancestor
                .extension()
                .map(|ext| ext == "app")
                .unwrap_or_default()
            {
                let frameworks_dir = ancestor
                    .join("Contents/Frameworks")
                    .to_string_lossy()
                    .to_string();
                let new_value = if let Ok(existing_value) = env::var(FRAMEWORKS_ENV_VAR_NAME) {
                    format!("{existing_value}:{frameworks_dir}")
                } else {
                    frameworks_dir
                };
                env::set_var(FRAMEWORKS_ENV_VAR_NAME, new_value);
                break;
            }
        }
    }

    /// Get new path for patched version, both as PathBuf and as a string, and make the dir
    /// of the path, recursively.
    fn get_output_path(path: &Path) -> Result<PathBuf> {
        // TODO: Change output to be with hash of the contents, so that old versions of changed
        //       files do not get used. (Also change back existing file logic to always use.)

        let output = MIRRORD_TEMP_BIN_DIR_PATH_BUF.join(
            // Strip root path from binary path, as when joined it will clear the previous.
            path.strip_prefix("/").unwrap_or(path), // No prefix - no problem.
        );

        std::fs::create_dir_all(
            output
                .parent()
                .ok_or_else(|| UnlikelyError("Failed to get parent directory".to_string()))?,
        )?;

        Ok(output)
    }

    /// Write a log to the log file, if the file exists, in a thread-safe way
    fn try_write_start_log_to_file(
        file: &mut File,
        binary_path: &str,
        status: &Result<SipStatus>,
        log_info: &SipLogInfo,
    ) {
        let _ = file
            .lock_exclusive()
            .map_err(|error| eprintln!("Failed to lock SIP logfile: {error}"));
        let _ = writeln!(
            file,
            "[{}] (pid {}, binary: {binary_path}, args: {:?}) SIP Status: {status:?}, layer load type: {:?}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("")
                .as_secs(),
            std::process::id(),
            log_info.args,
            log_info.load_type
        )
            .map_err(|error| eprintln!("Couldn't log SIP status to file: {error}"));
        let _ = FileExt::unlock(file)
            .map_err(|error| eprintln!("Failed to unlock SIP logfile: {error}"));
    }

    fn try_write_result_to_file(file: &mut File, result: &Result<Option<String>>) {
        let _ = file
            .lock_exclusive()
            .map_err(|error| eprintln!("Failed to lock SIP logfile: {error}"));
        let _ = writeln!(
            file,
            "[{}] (pid {}) SIP patch result: {result:?}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("")
                .as_secs(),
            std::process::id()
        )
        .map_err(|error| eprintln!("Couldn't log SIP status to file: {error}"));
        let _ = FileExt::unlock(file)
            .map_err(|error| eprintln!("Failed to unlock SIP logfile: {error}"));
    }

    /// Check if the file that the user wants to execute is a SIP protected binary
    ///
    /// (or a script starting with a shebang that leads to a SIP protected binary).
    /// If it is, create a non-protected version of the file and return `Ok(Some(patched_path)`.
    /// If it is not, `Ok(None)`.
    /// Propagate errors.
    pub fn sip_patch(
        binary_path: &str,
        opts: SipPatchOptions,
        log_info: Option<SipLogInfo>,
    ) -> Result<Option<String>> {
        // set up logging to a file if present in config
        let mut log_file = if let Some(log_info) = &log_info {
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_info.log_destination)
                .inspect_err(|error| eprintln!("Fail to open SIP logfile: {error}"))
                .ok()
        } else {
            None
        };

        let status = get_sip_status(binary_path, opts);
        if let (Some(log_info), Some(log_file)) = (log_info, log_file.as_mut()) {
            try_write_start_log_to_file(log_file, binary_path, &status, &log_info);
        };

        //    ______________________________
        //  / \                             \
        // |   |  ATTENTION! DANGER AHEAD!  |
        //  \_ |                            |
        //     |  ATTENTION! DANGER AHEAD!  |
        //     |                            |
        //     |  ATTENTION! DANGER AHEAD!  |
        //     |   _________________________|___
        //     |  /                            /
        //     \_/____________________________/
        //
        // DO NOT INTRODUCE NEW TRACING LOGS OR CHANGE THE LEVEL OF EXISTING LOGS - tracing logs are
        // NOT fork safe, and have been known to cause issues, including forcing the user's
        // machine to restart.
        let patch_result = match status {
            Ok(SipScript { path, shebang }) => {
                let patched_interpreter = patch_binary(&shebang.interpreter_path)?;
                let patched_script = patch_script(
                    &path,
                    shebang,
                    patched_interpreter.to_string_lossy().as_ref(),
                )
                .map(|path| path.to_string_lossy().to_string());
                Some(patched_script).transpose()
            }
            Ok(SipBinary(binary)) => {
                let patched_binary =
                    patch_binary(&binary).map(|path| path.to_string_lossy().to_string());
                Some(patched_binary).transpose()
            }
            Ok(NoSip) => {
                trace!("No SIP detected on {:?}", binary_path);
                Ok(None)
            }
            Err(SipError::IO(err)) if err.raw_os_error() == Some(24) => {
                // The full error is: `{ code: 24, kind: Uncategorized, message: "Too many open
                // files" }`. This error was encountered in the past when using mirrord with Air
                // (hot reloader).
                Err(SipError::TooManyFilesOpen(binary_path.to_string()))
            }
            Err(SipError::IO(err)) if err.kind() == ErrorKind::NotFound => {
                // In the future, if we know there are other kind of error we can ignore, add them
                // to this match arm
                trace!(
                    "Checking the SIP status of {binary_path} (or of the binary in its shebang, if \
                    applicable) failed with {err:?}. Continuing without SIP-sidestepping.\
                    This type of `error` happens during normal execution flow."
                );
                // E.g. `env` tries to execute a bunch of non-existing files and fails, and
                // that's just its valid flow.
                Ok(None)
            }
            Err(err) => {
                trace!(
                    "Checking the SIP status of {binary_path} (or of the binary in its shebang, if \
                    applicable) failed with {err:?}. Continuing without SIP-sidestepping.\
                    This is not necessarily an error."
                );
                // We know that `NotFound` errors may occur here as part of normal execution, and
                // that other errors may also occur normally, hence why we discard this error. In
                // the case that tracing logs are not being printed, the configuration for
                // `experimental.sip_log_destination` can be used to print this error to a file.
                Ok(None)
            }
        };

        if let Some(mut log_file) = log_file {
            try_write_result_to_file(&mut log_file, &patch_result);
        }

        patch_result
    }

    #[cfg(test)]
    mod tests {
        use std::io::Write;

        use super::*;

        #[test]
        fn is_sip_true() {
            assert!(matches!(
                get_sip_status("/bin/ls", SipPatchOptions::default()),
                Ok(SipBinary(_))
            ));
        }

        #[test]
        fn is_sip_false() {
            let mut f = tempfile::NamedTempFile::new().unwrap();
            let data = std::fs::read("/bin/ls").unwrap();
            f.write_all(&data).unwrap();
            f.flush().unwrap();
            assert!(matches!(
                get_sip_status(f.path().to_str().unwrap(), SipPatchOptions::default()).unwrap(),
                NoSip
            ));
        }

        #[test]
        fn is_sip_notfound() {
            let err = get_sip_status(
                "/donald/duck/was/a/duck/not/a/quack/a/duck",
                SipPatchOptions::default(),
            )
            .unwrap_err();
            assert!(err.to_string().contains("executable file not found"));
        }

        /// Verify the SIP-patch worked.
        ///
        /// Run the given binary with `DYLD_PRINT_LIBRARIES=1`. This env var asks dyld to print all
        /// the mach-o images loaded to the process. However, it is stripped away by SIP, so we can
        /// use it to test the patch worked. If we're getting libsystem_kernel.dylib printed, it
        /// means the binary is not SIP protected.
        fn run_and_verify_dyld_print(patched_bin_path: &Path) {
            let output = std::process::Command::new(patched_bin_path)
                .env("DYLD_PRINT_LIBRARIES", "1")
                .output()
                .unwrap();
            assert!(String::from_utf8_lossy(&output.stderr).contains("libsystem_kernel.dylib"));
        }

        /// Call `patch_binary` directly (it's a private function), verify the patched binary
        /// is no longer protected, and DYLD_PRINT_LIBRARIES is respected when running with it.
        fn patch_binary_and_verify_dyld_print(bin_path: &str) {
            let patched_bin_path = patch_binary(bin_path.as_ref()).unwrap();
            assert!(matches!(
                get_sip_status(
                    patched_bin_path.to_str().unwrap(),
                    SipPatchOptions::default()
                )
                .unwrap(),
                NoSip
            ));
            // Check DYLD_* features work on patched binary:
            run_and_verify_dyld_print(&patched_bin_path);
        }

        /// Call `sip_patch` (it's the public function this crate exposes), verify the patched
        /// binary is no longer protected, and DYLD_PRINT_LIBRARIES is respected when running with
        /// it.
        fn patch_sip_and_verify_dyld_print(executable_path: &str) {
            let patched_bin_path = sip_patch(executable_path, SipPatchOptions::default(), None)
                .unwrap()
                .unwrap();
            assert!(matches!(
                get_sip_status(&patched_bin_path, SipPatchOptions::default()).unwrap(),
                NoSip
            ));
            // Check DYLD_* features work on patched binary:
            run_and_verify_dyld_print(patched_bin_path.as_ref());
        }

        #[test]
        fn patch_binary_fat() {
            patch_binary_and_verify_dyld_print("/bin/ls");
        }

        #[test]
        fn patch_entitled_binary() {
            patch_sip_and_verify_dyld_print("/usr/bin/aa");
        }

        /// Test that after patching we can Successfully use DYLD features on a binary that had
        /// [the runtime flag](https://developer.apple.com/documentation/security/seccodesignatureflags/runtime)
        /// set before the patch.
        /// No binary in `/bin/` has any flag set, so we first do a kind of an opposite patch to
        /// create a binary with that flag, then we do our normal patch and test that it worked.
        #[test]
        fn patch_binary_with_runtime_flag() {
            let signed_temp_file = tempfile::NamedTempFile::new().unwrap();

            let mut settings = apple_codesign::SigningSettings::default();

            settings.set_code_signature_flags(
                apple_codesign::SettingsScope::Main,
                CodeSignatureFlags::ADHOC | CodeSignatureFlags::RUNTIME,
            );
            settings.set_binary_identifier(
                apple_codesign::SettingsScope::Main,
                signed_temp_file
                    .path()
                    .file_name()
                    .unwrap()
                    .to_string_lossy(),
            );

            let signer = apple_codesign::UnifiedSigner::new(settings);
            // `wait4path` chosen because it's the lightest file in /bin/ that doesn't do anything
            // when called without arguments.
            signer
                .sign_path("/bin/wait4path", signed_temp_file.path())
                .unwrap();

            patch_sip_and_verify_dyld_print(signed_temp_file.path().to_str().unwrap());
        }

        /// Test that when a fat binary contains an arm64 binary, that binary is used and patching
        /// works.
        ///
        /// This assumes `/usr/bin/file` is present and contains an arm64 binary.
        #[test]
        fn patch_binary_fat_with_arm64() {
            let path = "/usr/bin/file";
            let patched_path_buf = patch_binary(path.as_ref()).unwrap();
            let patched_path = patched_path_buf.to_str().unwrap();
            assert!(matches!(
                get_sip_status(patched_path, SipPatchOptions::default()).unwrap(),
                SipStatus::NoSip
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

        fn test_patch_script(script_contents: &str) {
            let mut original_file = tempfile::NamedTempFile::new().unwrap();
            original_file.write_all(script_contents.as_ref()).unwrap();
            original_file.flush().unwrap();
            let permissions = std::fs::Permissions::from_mode(0o700);
            std::fs::set_permissions(&original_file, permissions).unwrap();
            let patched_path = sip_patch(
                original_file.path().to_str().unwrap(),
                SipPatchOptions::default(),
                None,
            )
            .unwrap()
            .unwrap();
            // Check DYLD_* features work on it:
            let output = std::process::Command::new(patched_path)
                .env("DYLD_PRINT_LIBRARIES", "1")
                .output()
                .unwrap();
            assert!(String::from_utf8_lossy(&output.stderr).contains("libsystem_kernel.dylib"));
        }

        #[test]
        fn sip_patch_for_script_with_shebang() {
            test_patch_script("#!/usr/bin/env bash\necho hello\n")
        }

        #[test]
        fn sip_patch_for_script_with_no_shebang() {
            test_patch_script("echo hello\n")
        }

        #[test]
        fn sip_patch_for_script_with_spaced_shebang() {
            test_patch_script("#! /usr/bin/env bash\necho hello\n")
        }

        #[test]
        fn sip_patch_for_script_with_many_spaces_shebang() {
            test_patch_script("#!    /usr/bin/env bash\necho hello\n")
        }

        #[test]
        fn shebang_from_string() {
            let contents = "#!/usr/bin/env bash\n".to_string();
            assert_eq!(
                get_shebang_from_string(&contents)
                    .unwrap()
                    .interpreter_path
                    .to_str()
                    .unwrap(),
                "/usr/bin/env"
            )
        }

        #[test]
        fn shebang_from_string_with_space() {
            let contents = "#! /usr/bin/env bash\n".to_string();
            assert_eq!(
                get_shebang_from_string(&contents)
                    .unwrap()
                    .interpreter_path
                    .to_str()
                    .unwrap(),
                "/usr/bin/env"
            );
            let contents = "#!     /usr/bin/env bash\n".to_string();
            assert_eq!(
                get_shebang_from_string(&contents)
                    .unwrap()
                    .interpreter_path
                    .to_str()
                    .unwrap(),
                "/usr/bin/env"
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
            let changed_script_path = sip_patch(
                script.path().to_str().unwrap(),
                SipPatchOptions::default(),
                None,
            )
            .unwrap()
            .unwrap();
            let new_shebang = read_shebang_from_file(changed_script_path)
                .unwrap()
                .unwrap();
            let new_interpreter_path = new_shebang.interpreter_path.to_str().unwrap();
            // Check DYLD_* features work on it:
            let output = std::process::Command::new(new_interpreter_path)
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
            let _ = sip_patch(path_str, SipPatchOptions::default(), None)
                .unwrap()
                .unwrap();
            let _ = sip_patch(path_str, SipPatchOptions::default(), None)
                .unwrap()
                .unwrap();
        }

        /// Run `sip_patch` on a file that has a shebang that points to itself and verify that we
        /// don't get stuck in a recursion until the stack overflows.
        #[test]
        fn cyclic_shebangs() {
            let mut script = tempfile::NamedTempFile::new().unwrap();
            let contents = "#!".to_string() + script.path().to_str().unwrap();
            script.write_all(contents.as_bytes()).unwrap();
            script.flush().unwrap();
            let res = sip_patch(
                script.path().to_str().unwrap(),
                SipPatchOptions::default(),
                None,
            );
            assert!(matches!(res, Ok(None)));
        }

        #[test]
        fn set_fallback_frameworks_path() {
            let example_path = "/Applications/Postman.app/Contents/MacOS/Postman";
            let frameworks_path = "/Applications/Postman.app/Contents/Frameworks";
            let is_frameworks_path = |path: &str| path == frameworks_path;

            // Verify that the path was not there before.
            assert!(!env::var(FRAMEWORKS_ENV_VAR_NAME)
                .map(|value| value.split(':').any(is_frameworks_path))
                .unwrap_or_default());

            set_fallback_frameworks_path_if_mac_app(Path::new(example_path));

            // Verify that the path is there after.
            assert!(env::var(FRAMEWORKS_ENV_VAR_NAME)
                .unwrap()
                .split(':')
                .any(is_frameworks_path));
        }

        #[test]
        fn skip_patch() {
            let signed_temp_file = tempfile::NamedTempFile::new().unwrap();
            let signed_temp_file_path = signed_temp_file.path().to_str().unwrap();

            let mut settings = apple_codesign::SigningSettings::default();

            settings.set_code_signature_flags(
                apple_codesign::SettingsScope::Main,
                CodeSignatureFlags::ADHOC | CodeSignatureFlags::RESTRICT,
            );
            settings.set_binary_identifier(
                apple_codesign::SettingsScope::Main,
                signed_temp_file
                    .path()
                    .file_name()
                    .unwrap()
                    .to_string_lossy(),
            );
            let signer = apple_codesign::UnifiedSigner::new(settings);
            signer
                .sign_path("/bin/ls", signed_temp_file.path())
                .unwrap();
            assert!(matches!(
                get_sip_status(signed_temp_file_path, SipPatchOptions::default()).unwrap(),
                SipStatus::SipBinary(_),
            ));
            assert!(matches!(
                get_sip_status(
                    signed_temp_file_path,
                    SipPatchOptions {
                        patch: &[],
                        skip: &[signed_temp_file_path.to_string()],
                    }
                )
                .unwrap(),
                SipStatus::NoSip,
            ));
        }
    }
}

pub use main::*;
