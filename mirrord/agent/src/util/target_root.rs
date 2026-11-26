//! Confines paths to the target's filesystem.
//!
//! tl;dr:
//! 1. When `mirrord-protocol` requires us to open a file descriptor in a target's filesystem,
//!    **always** do it through [`TargetRoot`];
//! 2. When `mirrord-protocol` requires us to act on a path in the target's filesystem,
//!    **always** obtain the file descriptor with [`TargetRoot`],
//!    then use the path returned from [`proc_fd_path`].
//!
//! # Rationale
//!
//! The agent opens paths it can't trust: those of file requests
//! ([`FileManager`](crate::file::FileManager)) and Unix socket addresses (`outgoing`), which a
//! client can set to anything. The agent itself can reach
//! far more than the target: a targeted agent runs with `hostPID`, so `/proc/1/root` is the node's
//! root, and it mounts the node's `/run` and `/var`. A path may be absolute, climb with `..`, or
//! lead through symlinks in the target's filesystem, and none of these should reach a file outside
//! the target's root. Paths from the agent's own configuration, such as the TLS steal config's
//! certificates, go through here too, since they lead through the same symlinks.
//!
//! [`TargetRoot`] is the **only** place that turns such a path into a file descriptor. It opens
//! paths with `openat2(2)` and `RESOLVE_IN_ROOT`, so the kernel resolves `..`, absolute paths and
//! absolute symlinks against the target's root, and with `RESOLVE_NO_MAGICLINKS`, so procfs links
//! such as `/proc/<pid>/root` or `/proc/<pid>/fd/<n>` can't jump out of it. The kernel resolves
//! the whole path in one syscall, so a concurrent rename or symlink swap in the target can't
//! redirect it. `openat2(2)` needs Linux 5.6, and on older kernels we use [`userspace`] instead.
//!
//! Callers act on the returned descriptors with `f*` syscalls, with `*at` syscalls on the single
//! final component that [`TargetRoot::open_parent`] returns, or through [`proc_fd_path`] for APIs
//! that only take a path. They never pass the path itself to a path-based syscall.
//!
//! # Alternatives
//!
//! File access could instead run with its root changed to the target's, with `chroot(2)` into
//! `/proc/<pid>/root`, in a separate process or in a thread that first leaves the process's
//! shared filesystem attributes with `unshare(CLONE_FS)`. The kernel would then resolve `..`,
//! absolute paths and absolute symlinks against the target's root in every syscall, with no
//! special resolution on our side, and on any kernel.
//!
//! That approach was rejected because `chroot(2)` requires `CAP_SYS_CHROOT`, a capability the
//! agent doesn't otherwise need. mirrord doesn't request it for the agent's container, and
//! container runtimes such as CRI-O don't grant it by default.

mod userspace;

use std::{
    ffi::{OsStr, OsString},
    fs::{File, ReadDir},
    io,
    ops::Not,
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd},
        unix::{ffi::OsStrExt, fs::DirEntryExt},
    },
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use nix::{
    errno::Errno,
    fcntl::{self, AT_FDCWD, AtFlags, OFlag, OpenHow, ResolveFlag},
    sys::stat::{FileStat, Mode, fstat, fstatat},
};
use tracing::warn;

/// A directory a client opened, which relative paths in its later requests resolve against.
#[derive(Debug)]
pub(crate) struct Directory {
    pub(crate) file: File,
}

impl Directory {
    /// Lists the entries of [`Self::file`].
    pub(crate) fn read_dir(&self) -> io::Result<ReadDir> {
        std::fs::read_dir(proc_fd_path(&self.file))
    }
}

fn same_file(a: &FileStat, b: &FileStat) -> bool {
    (a.st_dev, a.st_ino) == (b.st_dev, b.st_ino)
}

/// The path of `fd` in the agent's own `/proc/self/fd`, for APIs that only take a path, such as
/// [`std::fs::read_dir`] or `connect(2)` on a Unix socket.
///
/// The kernel follows it to the file that `fd` holds, without resolving a path in the target
/// again. It's also short, unlike `/proc/<pid>/root` and a path in the target, which may not fit
/// in the 108 bytes of a Unix socket address.
pub(crate) fn proc_fd_path(fd: &impl AsRawFd) -> PathBuf {
    PathBuf::from(format!("/proc/self/fd/{}", fd.as_raw_fd()))
}

/// Where a resolution may go, like the `RESOLVE_*` flag of the same name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// The starting directory is the root: `..` stops at it, and absolute paths and symlinks
    /// start from it.
    InRoot,
    /// The resolution fails with `EXDEV` if it would leave the starting directory, with `..`,
    /// an absolute path or an absolute symlink.
    Beneath,
}

/// What resolves the paths of [`TargetRoot`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Resolver {
    /// `openat2(2)`.
    Kernel,
    /// [`userspace`], for kernels without `openat2(2)`, or where a seccomp profile blocks it.
    Userspace,
}

impl Resolver {
    /// How many times a resolution is retried after the kernel aborts it with `EAGAIN`, which
    /// `RESOLVE_IN_ROOT` and `RESOLVE_BENEATH` do when a concurrent rename races a `..`.
    const EAGAIN_RETRIES: usize = 8;

    /// Picks [`Self::Kernel`] when `openat2(2)` works, once per process.
    ///
    /// Tests take their kernel case from here, so that where `openat2(2)` doesn't work they
    /// exercise the userspace walk, as the agent would, rather than fail.
    pub(crate) fn detect() -> Self {
        static DETECTED: OnceLock<Resolver> = OnceLock::new();

        *DETECTED.get_or_init(|| {
            let how = OpenHow::new()
                .flags(OFlag::O_PATH | OFlag::O_CLOEXEC)
                .resolve(ResolveFlag::RESOLVE_IN_ROOT | ResolveFlag::RESOLVE_NO_MAGICLINKS);

            match fcntl::openat2(AT_FDCWD, "/", how) {
                Ok(_) => Self::Kernel,
                Err(error) => {
                    warn!(
                        %error,
                        "openat2(2) is unavailable, file requests resolve their paths in userspace"
                    );
                    Self::Userspace
                }
            }
        })
    }

    fn open(
        self,
        dirfd: BorrowedFd<'_>,
        path: &Path,
        flags: OFlag,
        mode: Mode,
        scope: Scope,
    ) -> io::Result<OwnedFd> {
        let flags = flags | OFlag::O_CLOEXEC;

        if self == Self::Userspace {
            return userspace::open(dirfd, path, flags, mode, scope);
        }

        // Unlike `openat(2)`, `openat2(2)` fails with `EINVAL` on a `mode` it wouldn't use.
        let mode = if flags.contains(OFlag::O_CREAT) {
            mode
        } else {
            Mode::empty()
        };
        let resolve = match scope {
            Scope::InRoot => ResolveFlag::RESOLVE_IN_ROOT,
            Scope::Beneath => ResolveFlag::RESOLVE_BENEATH,
        };
        let how = OpenHow::new()
            .flags(flags)
            .mode(mode)
            .resolve(resolve | ResolveFlag::RESOLVE_NO_MAGICLINKS);

        let mut retries = Self::EAGAIN_RETRIES;
        loop {
            match fcntl::openat2(dirfd, path, how) {
                Err(Errno::EAGAIN) if retries > 0 => retries -= 1,
                result => return result.map_err(io::Error::from),
            }
        }
    }
}

/// The root of the filesystem that the agent confines paths to.
///
/// The agent builds one, and shares clones of it, which are cheap: they share the descriptor of
/// the root.
#[derive(Debug, Clone)]
pub(crate) struct TargetRoot(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    /// `/proc/<pid>/root` for a target container, `/` for the agent's own filesystem.
    path: PathBuf,
    /// [`Self::path`], opened when the [`TargetRoot`] is built.
    ///
    /// Opening it right away pins the target's root before the target can exit and another
    /// process reuse its PID, after which `/proc/<pid>/root` would be that process's root. When
    /// the open fails, every open that needs the root tries again. Opening `/proc/<pid>/root`
    /// needs ptrace access to the target, which an agent without `CAP_SYS_PTRACE` may not have,
    /// and then only what needs the root fails, not the whole agent.
    fd: OnceLock<OwnedFd>,
    resolver: Resolver,
}

impl TargetRoot {
    /// The root of the container of the process `pid`, or the agent's own root without one.
    pub(crate) fn for_target(pid: Option<u64>) -> Self {
        Self::new(match pid {
            Some(pid) => PathBuf::from(format!("/proc/{pid}/root")),
            None => PathBuf::from("/"),
        })
    }

    pub(crate) fn new(path: PathBuf) -> Self {
        Self::with_resolver(path, Resolver::detect())
    }

    pub(crate) fn with_resolver(path: PathBuf, resolver: Resolver) -> Self {
        let fd = OnceLock::new();
        match Self::open_root(&path) {
            Ok(opened) => {
                let _ = fd.set(opened);
            }
            Err(error) => warn!(
                %error,
                ?path,
                "Failed to open the target's root, opens in it will try again"
            ),
        }

        Self(Arc::new(Inner { path, fd, resolver }))
    }

    fn open_root(path: &Path) -> io::Result<OwnedFd> {
        // The agent picks this path, and `/proc/<pid>/root` is a magic link that has to be
        // followed.
        Ok(fcntl::open(
            path,
            OFlag::O_PATH | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?)
    }

    fn fd(&self) -> io::Result<BorrowedFd<'_>> {
        if let Some(fd) = self.0.fd.get() {
            return Ok(fd.as_fd());
        }

        let fd = Self::open_root(&self.0.path)?;

        // A concurrent open may have won, and then `fd` is closed.
        Ok(self.0.fd.get_or_init(|| fd).as_fd())
    }

    /// Opens `path` inside the root, with `flags` and `mode` as `openat(2)` takes them.
    ///
    /// A relative `path` resolves from `base`. An absolute `path` always resolves from the
    /// root, the way `openat(2)` ignores its `dirfd` for one.
    ///
    /// A relative `path` without a `base` fails with `EINVAL`: the layer resolves relative
    /// paths locally, unless it can send them with a remote directory, so only a broken or
    /// hostile client sends one.
    pub(crate) fn open(
        &self,
        base: Option<&Directory>,
        path: &Path,
        flags: OFlag,
        mode: Mode,
    ) -> io::Result<OwnedFd> {
        let resolver = self.0.resolver;

        match base {
            Some(base) if path.is_relative() => {
                // Resolving from the directory's descriptor reaches it even after it was renamed
                // or its old path was replaced. Beneath it, a path that leaves it fails with
                // `EXDEV`, and is then resolved from the root through the directory's current
                // path, so that its `..` and absolute symlinks stop at the target's root rather
                // than at the directory.
                match resolver.open(base.file.as_fd(), path, flags, mode, Scope::Beneath) {
                    Err(error) if error.raw_os_error() == Some(libc::EXDEV) => resolver.open(
                        self.fd()?,
                        &self.path_of(base)?.join(path),
                        flags,
                        mode,
                        Scope::InRoot,
                    ),
                    result => result,
                }
            }
            _ if path.is_relative() => Err(io::Error::from_raw_os_error(libc::EINVAL)),
            _ => resolver.open(self.fd()?, path, flags, mode, Scope::InRoot),
        }
    }

    /// Opens `path` from the root, even when it's relative.
    ///
    /// For paths that never come with a directory, such as a Unix socket address or a path in
    /// the agent's configuration, where a relative path has always meant one from the root.
    pub(crate) fn open_from_root(&self, path: &Path, flags: OFlag) -> io::Result<OwnedFd> {
        self.open(None, &Path::new("/").join(path), flags, Mode::empty())
    }

    /// Opens the directory that holds the last component of `path`, and returns it with that
    /// component, for the `*at` syscalls that act on a name in a directory, such as
    /// `mkdirat(2)`, `unlinkat(2)` and `renameat(2)`.
    ///
    /// The component is a single name, possibly with trailing slashes, which those syscalls
    /// treat the way they would at the end of a full path. A single name can't leave the
    /// directory, and these syscalls never follow a symlink in it. A path with no last
    /// component, such as `/`, returns `.`.
    pub(crate) fn open_parent<'p>(
        &self,
        base: Option<&Directory>,
        path: &'p Path,
    ) -> io::Result<(OwnedFd, &'p OsStr)> {
        let bytes = path.as_os_str().as_bytes();
        let name_end = bytes
            .iter()
            .rposition(|byte| *byte != b'/')
            .map_or(0, |index| index + 1);

        let (parent, name) = if name_end == 0 {
            (path, OsStr::new("."))
        } else {
            let name_start = bytes
                .iter()
                .take(name_end)
                .rposition(|byte| *byte == b'/')
                .map_or(0, |index| index + 1);
            let (parent, name) = bytes.split_at(name_start);
            let parent = if parent.is_empty() {
                Path::new(".")
            } else {
                Path::new(OsStr::from_bytes(parent))
            };

            (parent, OsStr::from_bytes(name))
        };

        let parent = self.open(
            base,
            parent,
            OFlag::O_PATH | OFlag::O_DIRECTORY,
            Mode::empty(),
        )?;

        Ok((parent, name))
    }

    /// The path of `directory` inside the root as it is now, which may differ from the path
    /// that opened it, after a rename.
    ///
    /// Found the way `getcwd(3)` finds it: by opening `..` from the directory until the root,
    /// and looking up each directory's name in its parent. The kernel resolves those `..`
    /// against the agent's root, not the target's, and a directory that something outside the
    /// target moved out of a mount the target sees only part of (such as a `subPath`) climbs
    /// past the target's root without meeting it. So the parents are only ever read, never
    /// acted on, and the path is resolved from the root again. A climb that reaches the top of
    /// its filesystem without meeting the root fails with `ENOENT`, since the directory is no
    /// longer in the target.
    fn path_of(&self, directory: &Directory) -> io::Result<PathBuf> {
        let root = fstat(self.fd()?)?;
        let mut dir = directory.file.try_clone()?;
        let mut dir_stat = fstat(&dir)?;
        let mut names = Vec::new();
        let mut length = 0;

        while same_file(&dir_stat, &root).not() {
            let parent = File::from(fcntl::openat(
                &dir,
                "..",
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
                Mode::empty(),
            )?);
            let parent_stat = fstat(&parent)?;
            if same_file(&parent_stat, &dir_stat) {
                return Err(io::Error::from_raw_os_error(libc::ENOENT));
            }

            let name = Self::name_in(&parent, &dir_stat)?;
            // Bounds the climb, which concurrent renames could otherwise keep going.
            length += name.len() + 1;
            if length > libc::PATH_MAX as usize {
                return Err(io::Error::from_raw_os_error(libc::ENAMETOOLONG));
            }
            names.push(name);

            (dir, dir_stat) = (parent, parent_stat);
        }

        Ok(names
            .into_iter()
            .rev()
            .fold(PathBuf::from("/"), |path, name| path.join(name)))
    }

    /// The name of `child` in the directory `parent`.
    ///
    /// The entry's inode finds it, unless the entry doesn't carry the inode that `fstatat(2)`
    /// gives: a mount point's has the inode of the directory it covers, and overlayfs, which most
    /// container images are on, may give another one. Then every directory in `parent` is
    /// compared by `fstatat(2)`.
    fn name_in(parent: &File, child: &FileStat) -> io::Result<OsString> {
        let is_child = |name: &OsStr| {
            fstatat(parent, name, AtFlags::AT_SYMLINK_NOFOLLOW)
                .is_ok_and(|stat| same_file(&stat, child))
        };

        for entry in std::fs::read_dir(proc_fd_path(parent))? {
            let entry = entry?;
            if entry.ino() == child.st_ino && is_child(&entry.file_name()) {
                return Ok(entry.file_name());
            }
        }

        for entry in std::fs::read_dir(proc_fd_path(parent))? {
            let entry = entry?;
            if entry.file_type()?.is_dir() && is_child(&entry.file_name()) {
                return Ok(entry.file_name());
            }
        }

        Err(io::Error::from_raw_os_error(libc::ENOENT))
    }

    /// Opens the parent of `directory`, or `directory` itself when it's the root, the way `..`
    /// resolves inside the root.
    ///
    /// Goes from [`Directory::file`], so it reaches the parent of the directory that was opened
    /// even after a rename. The kernel resolves this `..` against the agent's root, and a
    /// directory moved out of the target (see [`Self::path_of`]) has its parent outside it, so
    /// the result is only good for the inode number of a `..` directory entry, never for acting
    /// on.
    pub(crate) fn open_parent_dir(&self, directory: &Directory) -> io::Result<OwnedFd> {
        let parent = if same_file(&fstat(self.fd()?)?, &fstat(&directory.file)?) {
            "."
        } else {
            ".."
        };

        Ok(fcntl::openat(
            &directory.file,
            parent,
            OFlag::O_PATH | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?)
    }
}
