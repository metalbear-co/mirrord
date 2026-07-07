use std::path::{Path, PathBuf};

use crate::logger::SipLoggerGuard;

/// Paths to system directories that are bundled in our SIP util bundle.
///
/// If an executed binary is located in one of these, we try to find it in the bundle.
pub const BUNDLED_DIRS: [&str; 4] = ["/bin", "/sbin", "/usr/bin", "/usr/sbin"];

/// Fins a non-protected version of the given binary in the pre-built SIP util bundle.
///
/// The bundle ships non-protected binaries commonly found in [`BUNDLED_DIRS`].
/// If the binary to be executed is located in one of these, we try to find it in the bundle.
/// For better compatibility:
/// 1. we only compare the binary name (e.g. `/bin/env` can be bundled under
///    `<bundle-path>/usr/bin/env`)
/// 2. we transparently use `bash` when asked for `sh` (on macOS, `sh` is just `bash`)
///
/// # Params
///
/// * `binary` - path to the binary that is being executed
/// * `bundle` - path to the pre-built SIP util bundle
/// * `logger` - logger to log the search result
pub fn find_in_bundle(
    binary: &Path,
    bundle: &Path,
    logger: &mut SipLoggerGuard<'_>,
) -> Option<PathBuf> {
    let mut binary_suffix = BUNDLED_DIRS
        .into_iter()
        .find_map(|dir| binary.strip_prefix(dir).ok())?;
    if binary_suffix == "sh" {
        binary_suffix = Path::new("bash");
    }

    for dir in BUNDLED_DIRS {
        let dir = dir.strip_prefix("/").unwrap_or(dir);
        let candidate = bundle.join(dir).join(binary_suffix);
        if candidate.exists() {
            logger.log(format_args!(
                "Found pre-built SIP util for {binary:?} at {candidate:?}",
            ));
            return Some(candidate);
        }
    }
    logger.log(format_args!(
        "Pre-built SIP util for {binary:?} was not found at {bundle:?}",
    ));
    None
}

#[cfg(test)]
mod test {
    use std::path::Path;

    use rstest::rstest;
    use tempfile::TempDir;

    use crate::logger::SipLogger;

    /// Creates a dummy bundle in the temp directory with the folllowing structure:
    /// * /bin
    ///     * /bash
    /// * /sbin
    ///     * /ifconfig
    /// * /usr
    ///     * /bin
    ///         * /cat
    ///     * /sbin
    fn make_bundle() -> TempDir {
        let root = tempfile::tempdir().unwrap();

        std::fs::create_dir(root.path().join("bin")).unwrap();
        std::fs::write(root.path().join("bin/bash"), &[]).unwrap();
        std::fs::create_dir(root.path().join("sbin")).unwrap();
        std::fs::write(root.path().join("sbin/ifconfig"), &[]).unwrap();
        std::fs::create_dir(root.path().join("usr")).unwrap();
        std::fs::create_dir(root.path().join("usr/bin")).unwrap();
        std::fs::write(root.path().join("usr/bin/cat"), &[]).unwrap();
        std::fs::create_dir(root.path().join("usr/sbin")).unwrap();

        root
    }

    #[rstest]
    #[case(Path::new("/bin/bash"), true)]
    #[case(Path::new("/usr/bin/cat"), true)]
    #[case(Path::new("/not/system/dir/bash"), false)]
    #[case(Path::new("/bin/sh"), true)]
    #[case(Path::new("/usr/sbin/ifconfig"), true)]
    #[case(Path::new("/usr/sbin/mount"), false)]
    #[case(Path::new("/sbin/ifconfig"), true)]
    #[test]
    fn find_in_bundle(#[case] binary: &Path, #[case] expect_found: bool) {
        let bundle = make_bundle();
        let found = super::find_in_bundle(binary, bundle.path(), &mut SipLogger::noop().lock());
        match (found, expect_found) {
            (Some(found), true) => {
                assert!(found.strip_prefix(bundle.path()).is_ok());
                assert!(found.exists());
            }
            (Some(found), false) => panic!("false positive at {found:?}"),
            (None, true) => panic!("false negative"),
            (None, false) => {}
        }
    }
}
