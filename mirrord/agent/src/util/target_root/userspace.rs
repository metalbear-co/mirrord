//! Manually resolves paths the way `openat2(2)` does with `RESOLVE_IN_ROOT` or
//! `RESOLVE_BENEATH`, and `RESOLVE_NO_MAGICLINKS`. Used as a fallback on kernels older than
//! Linux 5.6, and when `openat2(2)` fails with `EACCES`.
//!
//! The walk opens one component at a time with `O_PATH | O_NOFOLLOW`, relative to the
//! descriptor of the previous one, so the kernel never resolves more than a single name and never
//! follows a symlink. Every descriptor on the walk comes from one inside the starting directory,
//! which keeps the walk inside it:
//!
//! - A symlink is read, and its target walked the same way, from the root when it's absolute.
//! - `..` goes back to the previous directory on the walk instead of asking the kernel, and stops
//!   at the starting directory.
//! - A symlink on procfs whose text is absolute or names no file (`socket:[<inode>]`) fails with
//!   `ELOOP`, since that's what magic links read as. The kernel never follows a magic link here,
//!   but its text isn't a path in the target, so walking it would reach the wrong file. The plain
//!   symlinks there, such as `/proc/self` and `/proc/mounts`, are walked like any other.
//!
//! The kernel can tell when a concurrent rename moves a directory on the walk, and this can't.
//! Such a race may resolve a different path inside the root than the kernel would have, but
//! never one outside it.

use std::{
    collections::VecDeque,
    ffi::OsStr,
    io,
    os::{
        fd::{AsFd, BorrowedFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::Path,
};

use nix::{
    fcntl::{self, OFlag},
    sys::{
        stat::{Mode, fstat},
        statfs::{PROC_SUPER_MAGIC, fstatfs},
    },
};

use super::Scope;

/// How many symlinks one resolution may follow, as in the kernel.
const MAX_SYMLINKS: usize = 40;

fn error(errno: i32) -> io::Error {
    io::Error::from_raw_os_error(errno)
}

/// The names in `path`, including `.`, which the walk skips.
///
/// A trailing `.` stays until then, so that the name before it isn't the last component: it's
/// followed, has to be a directory, and is never created.
fn components(path: &[u8]) -> impl Iterator<Item = Vec<u8>> + '_ {
    path.split(|byte| *byte == b'/')
        .filter(|name| !name.is_empty())
        .map(<[u8]>::to_vec)
}

/// Opens `path` from `dirfd` like `openat2(2)` with `scope` and `RESOLVE_NO_MAGICLINKS`.
pub(super) fn open(
    dirfd: BorrowedFd<'_>,
    path: &Path,
    flags: OFlag,
    mode: Mode,
    scope: Scope,
) -> io::Result<OwnedFd> {
    let path = path.as_os_str().as_bytes();
    if path.is_empty() {
        return Err(error(libc::ENOENT));
    }
    if scope == Scope::Beneath && path.starts_with(b"/") {
        return Err(error(libc::EXDEV));
    }

    let start = dirfd.try_clone_to_owned()?;
    // The directories walked into from `start`, so that `..` can go back without the kernel.
    let mut walk: Vec<OwnedFd> = Vec::new();
    let mut pending: VecDeque<Vec<u8>> = components(path).collect();
    // A trailing slash makes the last component follow symlinks and requires a directory.
    let mut trailing_slash = path.ends_with(b"/");
    let mut symlinks = MAX_SYMLINKS;

    loop {
        let current = walk.last().unwrap_or(&start).as_fd();

        let Some(name) = pending.pop_front() else {
            return open_last(current, OsStr::new("."), flags, mode, trailing_slash);
        };

        if name == b"." {
            continue;
        }

        if name == b".." {
            if walk.pop().is_none() && scope == Scope::Beneath {
                return Err(error(libc::EXDEV));
            }
            continue;
        }

        let name = OsStr::from_bytes(&name);
        let last = pending.is_empty();
        let follow = !last
            || trailing_slash
            || !(flags.contains(OFlag::O_NOFOLLOW)
                || flags.contains(OFlag::O_CREAT | OFlag::O_EXCL));

        let entry = match fcntl::openat(
            current,
            name,
            OFlag::O_PATH | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        ) {
            Ok(entry) => entry,
            // The last component may not exist yet, for `O_CREAT`.
            Err(nix::errno::Errno::ENOENT) if last => {
                return open_last(current, name, flags, mode, trailing_slash);
            }
            Err(errno) => return Err(errno.into()),
        };

        let file_type = fstat(&entry)?.st_mode & libc::S_IFMT;

        if file_type == libc::S_IFLNK && follow {
            symlinks = symlinks.checked_sub(1).ok_or_else(|| error(libc::ELOOP))?;

            let target = fcntl::readlinkat(&entry, "")?;
            let target = target.as_bytes();
            if target.is_empty() {
                return Err(error(libc::ENOENT));
            }
            if (target.starts_with(b"/") || target.contains(&b':'))
                && fstatfs(&entry)?.filesystem_type() == PROC_SUPER_MAGIC
            {
                return Err(error(libc::ELOOP));
            }
            if target.starts_with(b"/") {
                if scope == Scope::Beneath {
                    return Err(error(libc::EXDEV));
                }
                walk.clear();
            }
            if last && target.ends_with(b"/") {
                trailing_slash = true;
            }

            for name in components(target).collect::<Vec<_>>().into_iter().rev() {
                pending.push_front(name);
            }
        } else if last {
            return open_last(current, name, flags, mode, trailing_slash);
        } else if file_type == libc::S_IFDIR {
            walk.push(entry);
        } else {
            return Err(error(libc::ENOTDIR));
        }
    }
}

/// Opens `name` in `parent` with the caller's `flags`, once the walk found that following it
/// isn't needed.
///
/// `O_NOFOLLOW` keeps a symlink swapped in since from being followed. The trailing slash becomes
/// `O_DIRECTORY` rather than staying on `name`, since a trailing slash makes the kernel follow a
/// symlink in spite of `O_NOFOLLOW`.
fn open_last(
    parent: BorrowedFd<'_>,
    name: &OsStr,
    flags: OFlag,
    mode: Mode,
    trailing_slash: bool,
) -> io::Result<OwnedFd> {
    let flags = if trailing_slash {
        if flags.contains(OFlag::O_CREAT) {
            return Err(error(libc::EISDIR));
        }
        flags | OFlag::O_DIRECTORY
    } else {
        flags
    };

    Ok(fcntl::openat(
        parent,
        name,
        flags | OFlag::O_NOFOLLOW,
        mode,
    )?)
}
