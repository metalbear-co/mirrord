//! Why a program `mirrord exec` launches on Linux or macOS won't load the mirrord layer.
//!
//! On these platforms the layer reports `Ready!` and holds the program for a debugger, so `exec`
//! needs to know up front when it never loads: it then says `Ready!` itself, as before, and
//! refuses a debug hold instead of letting the program run unheld.

use std::{fmt, fs, os::unix::fs::MetadataExt, path::Path};

use nix::unistd::{getgid, getuid};

/// The setuid mode bit, the same on every Unix.
const SET_UID: u32 = 0o4000;
/// The setgid mode bit, the same on every Unix.
const SET_GID: u32 = 0o2000;

/// A reason the program won't load the mirrord layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NoLayer {
    /// It's statically linked, so no dynamic loader runs to load the layer.
    #[cfg(target_os = "linux")]
    Static,
    /// It runs as another user or group (setuid or setgid), so the loader ignores mirrord's
    /// preload variable.
    ChangesIdentity,
    /// It's SIP-protected and wasn't patched (`skip_sip`), so macOS strips mirrord's library.
    #[cfg(target_os = "macos")]
    UnpatchedSip,
}

impl fmt::Display for NoLayer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            #[cfg(target_os = "linux")]
            Self::Static => "it is statically linked",
            Self::ChangesIdentity => "it runs as another user or group (setuid or setgid)",
            #[cfg(target_os = "macos")]
            Self::UnpatchedSip => "it is SIP-protected and mirrord didn't patch it",
        })
    }
}

/// Whether running `binary_path` switches to another user or group: a setuid or setgid file
/// owned by someone else. The loader then runs in secure mode, and ignores `LD_PRELOAD` paths
/// (glibc's `AT_SECURE`) and `DYLD_INSERT_LIBRARIES` alike.
///
/// A file mirrord can't inspect reads as `false`, as does a setuid file on a `nosuid` mount,
/// which can't be told apart here.
pub(crate) fn changes_identity(binary_path: &Path) -> bool {
    fs::metadata(binary_path).is_ok_and(|file| {
        switches_identity(
            file.mode(),
            (file.uid(), file.gid()),
            (getuid().as_raw(), getgid().as_raw()),
        )
    })
}

/// Whether a file with `mode`, owned by `owner` (uid, gid), runs as someone other than `runner`
/// (uid, gid). The setuid bit only switches users when the owner is someone else, and setgid
/// likewise for groups.
fn switches_identity(mode: u32, owner: (u32, u32), runner: (u32, u32)) -> bool {
    (mode & SET_UID != 0 && owner.0 != runner.0) || (mode & SET_GID != 0 && owner.1 != runner.1)
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    const ME: (u32, u32) = (1000, 1000);

    #[rstest]
    #[case::setuid_root_like_su(0o4755, (0, 0), true)]
    #[case::setuid_but_mine(0o4755, ME, false)]
    #[case::setgid_another_group(0o2755, (1000, 5), true)]
    #[case::setgid_but_my_group(0o2755, (0, 1000), false)]
    // The odd one out: owned by root, but with neither bit it runs as whoever starts it.
    #[case::plain_root_file(0o0755, (0, 0), false)]
    fn the_layer_drops_only_when_the_identity_switches(
        #[case] mode: u32,
        #[case] owner: (u32, u32),
        #[case] expected: bool,
    ) {
        assert_eq!(
            switches_identity(mode, owner, ME),
            expected,
            "mode {mode:o} owned by {owner:?}, run by {ME:?}"
        );
    }
}
