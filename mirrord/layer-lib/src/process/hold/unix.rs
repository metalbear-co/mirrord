//! The Unix release gate: a FIFO the IDE writes one byte to.
//!
//! On Unix the layer holds the program from its own constructor, so the process that waits is the
//! held one, and only a release ends the wait. The FIFO sits in a directory only this user can
//! enter, and its name carries a per-launch uuid, so an IDE that kept an old path can never
//! release a later program that reused the pid.

use std::{
    fs::{self, DirBuilder, File},
    io,
    ops::Not,
    os::{
        fd::OwnedFd,
        unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt},
    },
    path::{Path, PathBuf},
};

use nix::{
    errno::Errno,
    fcntl::{FcntlArg, OFlag, fcntl, open, openat},
    sys::{signal::kill, stat::Mode},
    unistd::{Pid, UnlinkatFlags, mkfifo, read, unlinkat},
};
use uuid::Uuid;

use super::{HoldError, HoldOutcome, ReleaseGate};

/// Where the per-user directory goes when `XDG_RUNTIME_DIR` isn't set, as on macOS.
const FALLBACK_RUNTIME_DIR: &str = "/tmp";

/// Why the release FIFO could not be made, or waited on.
#[derive(Debug, thiserror::Error)]
pub enum GateError {
    #[error("failed to create the debug release directory {path}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to inspect the debug release directory {path}")]
    InspectDir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("the debug release directory {path} is not a directory (a symlink is refused too)")]
    NotADirectory { path: PathBuf },

    #[error("the debug release directory {path} belongs to uid {owner}, not to this user")]
    DirNotOwned { path: PathBuf, owner: u32 },

    #[error("the debug release directory {path} has mode {mode:o}, so other users can enter it")]
    DirOpenToOthers { path: PathBuf, mode: u32 },

    #[error(
        "the debug release path {path} has whitespace or isn't UTF-8, so an IDE can't read it from the announcement"
    )]
    UnusablePath { path: PathBuf },

    #[error("the debug release FIFO {path} already exists")]
    Exists { path: PathBuf },

    #[error("failed to create the debug release FIFO {path}")]
    CreateFifo {
        path: PathBuf,
        #[source]
        source: Errno,
    },

    #[error("failed to open the debug release FIFO {path}")]
    Open {
        path: PathBuf,
        #[source]
        source: Errno,
    },

    #[error("failed to inspect the debug release FIFO {path}")]
    InspectFifo {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("the debug release path {path} opened as something other than a FIFO")]
    NotAFifo { path: PathBuf },

    #[error("failed to make reads of the debug release FIFO {path} blocking")]
    SetBlocking {
        path: PathBuf,
        #[source]
        source: Errno,
    },

    #[error("failed to read the debug release FIFO {path}")]
    Read {
        path: PathBuf,
        #[source]
        source: Errno,
    },

    #[error("the debug release FIFO {path} reached its end without a release")]
    Closed { path: PathBuf },
}

/// The per-user directory, checked through this open descriptor rather than by its path.
#[derive(Debug)]
struct PrivateDir {
    fd: OwnedFd,
    path: PathBuf,
}

/// A FIFO in the private directory, removed when dropped.
#[derive(Debug)]
struct Fifo {
    dir: PrivateDir,
    name: String,
    /// The announced path: `dir`'s path joined with `name`. Always UTF-8 and free of whitespace.
    path: String,
}

impl Drop for Fifo {
    fn drop(&mut self) {
        if let Err(error) = unlinkat(&self.dir.fd, self.name.as_str(), UnlinkatFlags::NoRemoveDir) {
            tracing::debug!(%error, path = self.path, "failed to remove the debug release FIFO");
        }
    }
}

/// The release gate of one held program: a FIFO that one written byte releases.
#[derive(Debug)]
pub struct Gate {
    pid: u32,
    fifo: Fifo,
    /// The end [`Gate::wait`] reads the release byte from.
    read: OwnedFd,
    /// The gate's own write end. With a writer always open, an IDE's `open(O_WRONLY)` never blocks
    /// waiting for a reader, and a writer that closes without writing never gives `read` an end
    /// of file.
    _write: OwnedFd,
}

impl Gate {
    /// Creates the gate for `pid` at `<runtime_dir>/mirrord-debug-<uid>/release-<pid>-<uuid>`.
    fn create_in(runtime_dir: &Path, pid: u32) -> Result<Self, GateError> {
        let dir = private_dir(runtime_dir)?;
        remove_stale_fifos(&dir);
        Self::at(
            dir,
            format!("release-{pid}-{}", Uuid::new_v4().simple()),
            pid,
        )
    }

    /// Makes the FIFO `name` in `dir` and opens both its ends.
    ///
    /// Both ends are open before [`hold`](super::hold) announces the path, so an IDE that writes
    /// as soon as it reads the announcement never blocks on the FIFO, even when a debugger pauses
    /// the program right after the announcement.
    fn at(dir: PrivateDir, name: String, pid: u32) -> Result<Self, GateError> {
        // The IDE matches `release=\S+` in the announcement.
        let path = match dir.path.join(&name).into_os_string().into_string() {
            Ok(path) if !path.contains(char::is_whitespace) => path,
            Ok(path) => return Err(GateError::UnusablePath { path: path.into() }),
            Err(path) => return Err(GateError::UnusablePath { path: path.into() }),
        };

        // A FIFO that's already there may belong to another launch, or already hold a byte that
        // would release this program before anyone attached.
        match mkfifo(path.as_str(), Mode::S_IRUSR | Mode::S_IWUSR) {
            Ok(()) => {}
            Err(Errno::EEXIST) => return Err(GateError::Exists { path: path.into() }),
            Err(source) => {
                return Err(GateError::CreateFifo {
                    path: path.into(),
                    source,
                });
            }
        }
        let fifo = Fifo { dir, name, path };

        // TRICK: `openat` through the checked directory's descriptor only finds entries inside
        // that directory, whatever its path names by now. So if another user swapped the path
        // after the check, the FIFO `mkfifo` made is elsewhere, and this open fails instead of
        // using it. (`mkfifoat` would close the gap earlier, but nix has none on macOS.)
        //
        // Non-blocking, since a blocking open of either end waits for the other one.
        let open_end = |access: OFlag| {
            openat(
                &fifo.dir.fd,
                fifo.name.as_str(),
                access | OFlag::O_NONBLOCK | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                Mode::empty(),
            )
            .map_err(|source| GateError::Open {
                path: PathBuf::from(&fifo.path),
                source,
            })
        };
        let read = File::from(open_end(OFlag::O_RDONLY)?);
        match read.metadata() {
            Ok(metadata) if metadata.file_type().is_fifo() => {}
            Ok(_) => {
                return Err(GateError::NotAFifo {
                    path: PathBuf::from(&fifo.path),
                });
            }
            Err(source) => {
                return Err(GateError::InspectFifo {
                    path: PathBuf::from(&fifo.path),
                    source,
                });
            }
        }
        let read = OwnedFd::from(read);
        let write = open_end(OFlag::O_WRONLY)?;

        Ok(Self {
            pid,
            fifo,
            read,
            _write: write,
        })
    }

    /// Blocks until the IDE writes a byte.
    fn read_release(&self) -> Result<(), GateError> {
        let path = || PathBuf::from(&self.fifo.path);

        let flags = fcntl(&self.read, FcntlArg::F_GETFL)
            .map(OFlag::from_bits_truncate)
            .map_err(|source| GateError::SetBlocking {
                path: path(),
                source,
            })?;
        fcntl(
            &self.read,
            FcntlArg::F_SETFL(flags.difference(OFlag::O_NONBLOCK)),
        )
        .map_err(|source| GateError::SetBlocking {
            path: path(),
            source,
        })?;

        let mut byte = [0; 1];
        loop {
            match read(&self.read, &mut byte) {
                Ok(0) => return Err(GateError::Closed { path: path() }),
                Ok(_) => return Ok(()),
                Err(Errno::EINTR) => continue,
                Err(source) => {
                    return Err(GateError::Read {
                        path: path(),
                        source,
                    });
                }
            }
        }
    }
}

impl ReleaseGate for Gate {
    /// Nothing: the process that waits is the held one, so it can't exit while it waits.
    type Holder<'a> = ();

    fn create(pid: u32) -> Result<Self, HoldError> {
        let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .unwrap_or_else(|| PathBuf::from(FALLBACK_RUNTIME_DIR));

        Self::create_in(&runtime_dir, pid).map_err(|source| HoldError::Create { pid, source })
    }

    fn release_name(&self) -> &str {
        &self.fifo.path
    }

    fn wait(&self, (): ()) -> Result<HoldOutcome, HoldError> {
        self.read_release()
            .map(|()| HoldOutcome::Released)
            .map_err(|source| HoldError::Wait {
                pid: self.pid,
                source,
            })
    }
}

/// Removes the release FIFOs in `dir` whose program is gone.
///
/// A program killed while held (SIGKILL, or `mirrord up` stopping it) never runs [`Fifo`]'s drop,
/// so its FIFO stays behind. The next gate in the directory cleans those up. One whose pid was
/// reused since is kept, which is harmless: nothing announces it any more.
fn remove_stale_fifos(dir: &PrivateDir) {
    let Ok(entries) = fs::read_dir(&dir.path) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(release_pid) else {
            continue;
        };
        let is_fifo = entry.file_type().is_ok_and(|kind| kind.is_fifo());
        // Probing with no signal: `ESRCH` means no process has the pid.
        if is_fifo && matches!(kill(pid, None), Err(Errno::ESRCH)) {
            // Through the checked directory's descriptor, so only an entry inside it goes.
            if let Err(error) = unlinkat(&dir.fd, name.as_os_str(), UnlinkatFlags::NoRemoveDir) {
                tracing::debug!(%error, ?name, "failed to remove a stale debug release FIFO");
            }
        }
    }
}

/// The pid a release FIFO named `release-<pid>-<uuid>` was made for, or `None` for any other
/// name. Never 0 or below, which `kill` reads as process groups.
fn release_pid(name: &str) -> Option<Pid> {
    let (pid, _uuid) = name.strip_prefix("release-")?.split_once('-')?;
    let pid = pid.parse::<i32>().ok().filter(|pid| *pid > 0)?;
    Some(Pid::from_raw(pid))
}

/// Creates (or reuses) `<runtime_dir>/mirrord-debug-<uid>`, a directory only this user can enter.
///
/// A path that isn't a real directory, owned by this user and closed to everyone else, is refused
/// rather than repaired: in a shared `/tmp`, another user may have made it first.
///
/// The directory is opened once, and checked and used only through that descriptor, so a path
/// swapped after the check can't redirect the FIFO.
fn private_dir(runtime_dir: &Path) -> Result<PrivateDir, GateError> {
    // SAFETY: `getuid` has no preconditions and always succeeds.
    let uid = unsafe { libc::getuid() };
    let path = runtime_dir.join(format!("mirrord-debug-{uid}"));

    match DirBuilder::new().mode(0o700).create(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(source) => return Err(GateError::CreateDir { path, source }),
    }

    // `O_NOFOLLOW` refuses a symlink planted at the path, and `O_DIRECTORY` anything else that
    // isn't a directory.
    let dir = match open(
        &path,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    ) {
        Ok(fd) => File::from(fd),
        Err(Errno::ELOOP | Errno::ENOTDIR) => return Err(GateError::NotADirectory { path }),
        Err(source) => {
            return Err(GateError::InspectDir {
                path,
                source: source.into(),
            });
        }
    };
    let metadata = match dir.metadata() {
        Ok(metadata) => metadata,
        Err(source) => return Err(GateError::InspectDir { path, source }),
    };
    if metadata.file_type().is_dir().not() {
        return Err(GateError::NotADirectory { path });
    }
    if metadata.uid() != uid {
        return Err(GateError::DirNotOwned {
            path,
            owner: metadata.uid(),
        });
    }
    let mode = metadata.mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(GateError::DirOpenToOthers { path, mode });
    }

    Ok(PrivateDir {
        fd: dir.into(),
        path,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{File, OpenOptions},
        io::Write,
        os::unix::fs::{FileTypeExt, PermissionsExt, symlink},
        sync::mpsc::{self, RecvTimeoutError},
        thread,
        time::Duration,
    };

    use super::*;

    /// What an IDE does to release the program: open the announced path and write one byte.
    fn release(path: &str) {
        OpenOptions::new()
            .write(true)
            .open(path)
            .expect("the gate holds a reader, so the IDE's open succeeds at once")
            .write_all(b"x")
            .expect("a FIFO with a reader accepts a byte");
    }

    fn expect_released(outcome: Result<HoldOutcome, HoldError>, why: &str) {
        match outcome {
            Ok(HoldOutcome::Released) => {}
            Ok(HoldOutcome::ProcessExited) => panic!("{why}, but the wait said the process exited"),
            Err(error) => panic!("{why}, but the wait failed: {error:?}"),
        }
    }

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("a temporary runtime directory can be created")
    }

    /// A byte from the IDE releases a program that's already waiting.
    #[test]
    fn a_byte_releases_a_waiting_gate() {
        let runtime_dir = temp_dir();
        let gate = Gate::create_in(runtime_dir.path(), 42).expect("the gate is created");

        thread::scope(|scope| {
            let waiter = scope.spawn(|| gate.wait(()));
            release(gate.release_name());
            expect_released(
                waiter.join().expect("the waiting thread doesn't panic"),
                "the IDE wrote a byte",
            );
        });
    }

    /// An IDE can release the program before the program starts waiting: the byte stays in the
    /// FIFO until the wait reads it.
    #[test]
    fn a_byte_written_before_the_wait_still_releases() {
        let runtime_dir = temp_dir();
        let gate = Gate::create_in(runtime_dir.path(), 42).expect("the gate is created");

        release(gate.release_name());

        expect_released(gate.wait(()), "the IDE wrote a byte before the wait");
    }

    /// Opening the FIFO is not a release: an IDE that checks the path and closes it again must
    /// not let the program run.
    #[test]
    fn a_writer_that_closes_without_writing_does_not_release() {
        let runtime_dir = temp_dir();
        let gate = Gate::create_in(runtime_dir.path(), 42).expect("the gate is created");
        let gate = &gate;

        thread::scope(|scope| {
            let (outcome_tx, outcome_rx) = mpsc::channel();
            scope.spawn(move || outcome_tx.send(gate.wait(())));

            drop(
                OpenOptions::new()
                    .write(true)
                    .open(gate.release_name())
                    .expect("the gate holds a reader, so the open succeeds at once"),
            );
            assert!(
                matches!(
                    outcome_rx.recv_timeout(Duration::from_millis(500)),
                    Err(RecvTimeoutError::Timeout)
                ),
                "a writer that closed without writing must leave the program held"
            );

            release(gate.release_name());
            expect_released(
                outcome_rx.recv().expect("the waiting thread reports"),
                "the IDE wrote a byte after the empty writer",
            );
        });
    }

    /// Whatever is already at the path is refused and left alone, never reused as the gate.
    #[test]
    fn an_existing_path_is_refused() {
        let runtime_dir = temp_dir();
        let dir = private_dir(runtime_dir.path()).expect("the private directory is created");
        let path = dir.path.join("release-42-taken");
        File::create(&path).expect("the file in the way is created");

        let result = Gate::at(dir, "release-42-taken".to_owned(), 42);

        assert!(
            matches!(result, Err(GateError::Exists { .. })),
            "an existing path must be refused: {result:?}"
        );
        assert!(
            fs::symlink_metadata(&path)
                .expect("the file in the way is still there")
                .file_type()
                .is_file(),
            "the file in the way is left alone"
        );
    }

    /// The IDE reads the path up to the first whitespace, so a path with whitespace is refused
    /// before anything is created.
    #[test]
    fn a_path_with_whitespace_is_refused() {
        let runtime_dir = temp_dir();
        let dir = private_dir(runtime_dir.path()).expect("the private directory is created");
        let path = dir.path.join("release 42");

        let result = Gate::at(dir, "release 42".to_owned(), 42);

        assert!(
            matches!(result, Err(GateError::UnusablePath { .. })),
            "a path with whitespace must be refused: {result:?}"
        );
        assert!(
            fs::symlink_metadata(&path).is_err(),
            "nothing is created at a refused path"
        );
    }

    /// The FIFO lives exactly as long as the gate.
    #[test]
    fn the_fifo_is_removed_on_drop() {
        let runtime_dir = temp_dir();
        let gate = Gate::create_in(runtime_dir.path(), 42).expect("the gate is created");
        let path = PathBuf::from(gate.release_name());
        assert!(
            fs::symlink_metadata(&path)
                .expect("the FIFO exists while the gate does")
                .file_type()
                .is_fifo(),
            "the gate is a FIFO"
        );

        drop(gate);

        assert!(
            fs::symlink_metadata(&path).is_err(),
            "the FIFO is removed with the gate"
        );
    }

    /// A symlink planted where the private directory goes is refused, so the FIFO can't be made
    /// in a directory someone else chose.
    #[test]
    fn a_symlink_in_place_of_the_private_dir_is_refused() {
        let runtime_dir = temp_dir();
        let elsewhere = runtime_dir.path().join("elsewhere");
        DirBuilder::new()
            .mode(0o700)
            .create(&elsewhere)
            .expect("the symlink's target is created");
        // SAFETY: `getuid` has no preconditions and always succeeds.
        let uid = unsafe { libc::getuid() };
        symlink(
            &elsewhere,
            runtime_dir.path().join(format!("mirrord-debug-{uid}")),
        )
        .expect("the symlink is planted");

        let result = Gate::create_in(runtime_dir.path(), 42);

        assert!(
            matches!(result, Err(GateError::NotADirectory { .. })),
            "a symlink must be refused: {result:?}"
        );
    }

    /// A private directory that other users can enter is refused rather than used.
    #[test]
    fn a_private_dir_open_to_others_is_refused() {
        let runtime_dir = temp_dir();
        // SAFETY: `getuid` has no preconditions and always succeeds.
        let uid = unsafe { libc::getuid() };
        let dir = runtime_dir.path().join(format!("mirrord-debug-{uid}"));
        fs::create_dir(&dir).expect("the directory is created");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755))
            .expect("the directory is opened to others");

        let result = Gate::create_in(runtime_dir.path(), 42);

        assert!(
            matches!(result, Err(GateError::DirOpenToOthers { mode: 0o755, .. })),
            "a directory others can enter must be refused: {result:?}"
        );
    }

    /// A private directory swapped for another after its check is never used: the FIFO is opened
    /// through the checked directory, so a FIFO made at the swapped path is refused.
    #[test]
    fn a_private_dir_swapped_after_its_check_is_never_used() {
        let runtime_dir = temp_dir();
        let dir = private_dir(runtime_dir.path()).expect("the private directory is created");
        let checked = runtime_dir.path().join("checked");
        fs::rename(&dir.path, &checked).expect("the checked directory is moved away");
        DirBuilder::new()
            .mode(0o700)
            .create(&dir.path)
            .expect("another directory takes its path");

        let result = Gate::at(dir, "release-42-swapped".to_owned(), 42);

        assert!(
            matches!(
                result,
                Err(GateError::Open {
                    source: Errno::ENOENT,
                    ..
                })
            ),
            "the FIFO made at the swapped path must not be opened: {result:?}"
        );
        assert!(
            fs::read_dir(&checked)
                .expect("the checked directory is readable")
                .next()
                .is_none(),
            "nothing is made in the checked directory"
        );
    }

    /// A program killed while held leaves its FIFO behind; the next gate removes it, and keeps the
    /// FIFO of a program that's still running.
    #[test]
    fn a_new_gate_removes_the_fifos_of_programs_that_are_gone() {
        let runtime_dir = temp_dir();
        let dir = private_dir(runtime_dir.path()).expect("the private directory is created");
        // Above every system's pid limit, so no process ever has it.
        let gone_pid = i32::MAX;
        let stale = dir.path.join(format!("release-{gone_pid}-stale"));
        let live = dir
            .path
            .join(format!("release-{}-live", std::process::id()));
        for path in [&stale, &live] {
            mkfifo(path, Mode::S_IRUSR | Mode::S_IWUSR).expect("a leftover FIFO is made");
        }

        let _gate = Gate::create_in(runtime_dir.path(), 42).expect("the gate is created");

        assert!(
            fs::symlink_metadata(&stale).is_err(),
            "process {gone_pid} is gone, so its FIFO is removed"
        );
        assert!(
            fs::symlink_metadata(&live).is_ok(),
            "this process still runs, so its FIFO is kept"
        );
    }
}
