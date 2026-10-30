//! Holding a launched program before its first line runs, until an IDE releases it, so a debugger
//! can attach first (`mirrord up --debug`).
//!
//! Every platform runs the same flow, [`hold`]: create a gate only the IDE can release, announce
//! it in one line the IDE matches, and wait. What the gate is, and what else ends the wait, is the
//! platform's [`ReleaseGate`]: a named event on Windows, set by the IDE, and a FIFO on Linux and
//! macOS, written to by the IDE.
//!
//! Who holds differs too. On Windows `mirrord exec` holds the program it created suspended, once
//! its layer is ready. On Linux and macOS the layer holds the program it is loaded in, from its
//! constructor, once it has started. Either way the program's entry point and its own initializers
//! have not run when the debugger attaches, though the initializers of the libraries it depends on
//! may have.

use std::{fmt, io::Write};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;

/// The gate of the platform this is built for.
mod platform {
    #[cfg(unix)]
    pub use super::unix::{Gate, GateError};
    #[cfg(windows)]
    pub use super::windows::{Gate, GateError};
}

pub use platform::{Gate, GateError};

/// The line an IDE matches to find a held program and release it:
/// `debug-hold pid=<pid> release=<release>`.
///
/// `mirrord up` prefixes it with the service's name, like the rest of the service's output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Announcement<'a> {
    /// The held program, which the debugger attaches to.
    pub pid: u32,
    /// What the IDE opens to release it. See [`ReleaseGate::release_name`].
    pub release: &'a str,
}

impl fmt::Display for Announcement<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "debug-hold pid={} release={}", self.pid, self.release)
    }
}

/// How a hold ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldOutcome {
    /// The IDE released the program, so it may run.
    Released,
    /// The program ended while it was held, so there is nothing left to release.
    ProcessExited,
}

/// What a platform provides to [`hold`]: something only the IDE can release.
pub trait ReleaseGate: Sized {
    /// What ends the wait besides a release: the held process on Windows, nothing on Unix, where
    /// the process holding is the held one.
    type Holder<'a>;

    /// Creates the gate for `pid`, unique to this launch, so an IDE that kept an old name can
    /// never release a later program that reused the pid.
    fn create(pid: u32) -> Result<Self, HoldError>;

    /// What the IDE opens to release: an event name, or a FIFO path. Never has whitespace, since
    /// the IDE matches `release=\S+`.
    fn release_name(&self) -> &str;

    /// Waits for the release, or for `holder` to end the wait. Never times out: a person is
    /// attaching a debugger.
    fn wait(&self, holder: Self::Holder<'_>) -> Result<HoldOutcome, HoldError>;
}

/// The one hold every platform runs: create the gate, announce it, wait for the release.
///
/// The announcement goes out in one `write_all`, then a flush, before the wait begins: a writer
/// that writes straight through keeps the line whole, and the IDE never waits on a line still in a
/// buffer. One that cannot be written means no IDE will ever release the program, so it is an
/// error, and nothing waits.
///
/// # Arguments
///
/// * `pid` - the held program.
/// * `holder` - what else ends the wait. See [`ReleaseGate::Holder`].
/// * `out` - where the IDE reads the announcement: the program's stdout, which `mirrord up`
///   forwards.
pub fn hold<G: ReleaseGate>(
    pid: u32,
    holder: G::Holder<'_>,
    out: &mut impl Write,
) -> Result<HoldOutcome, HoldError> {
    let gate = G::create(pid)?;
    let announcement = Announcement {
        pid,
        release: gate.release_name(),
    };
    out.write_all(format!("{announcement}\n").as_bytes())
        .and_then(|()| out.flush())
        .map_err(|source| HoldError::Announce { pid, source })?;
    tracing::debug!(
        pid,
        release = gate.release_name(),
        "debug hold: waiting for the release"
    );
    gate.wait(holder)
}

/// Why a program could not be held for a debugger.
#[derive(Debug, thiserror::Error)]
pub enum HoldError {
    #[error("failed to create the debug release gate for process {pid}")]
    Create {
        pid: u32,
        #[source]
        source: GateError,
    },

    #[error("failed to announce the debug hold of process {pid}")]
    Announce {
        pid: u32,
        #[source]
        source: std::io::Error,
    },

    #[error("failed to wait for the debug release of process {pid}")]
    Wait {
        pid: u32,
        #[source]
        source: GateError,
    },
}

#[cfg(test)]
mod tests {
    use std::{cell::RefCell, io};

    use super::*;

    /// What the fake gate and the fake output saw, in one place, so a test can tell what had
    /// reached the IDE when the wait began.
    #[derive(Default)]
    struct Journal {
        /// Everything flushed to the output so far.
        flushed: String,
        /// What had been flushed when the wait began, or `None` when nothing waited.
        flushed_at_wait: Option<String>,
    }

    /// What the fake gate's wait observes, and how it ends.
    struct FakeHolder<'a> {
        journal: &'a RefCell<Journal>,
        outcome: HoldOutcome,
    }

    /// A gate that releases at once, with the outcome its holder names.
    struct FakeGate {
        name: String,
    }

    impl ReleaseGate for FakeGate {
        type Holder<'a> = FakeHolder<'a>;

        fn create(pid: u32) -> Result<Self, HoldError> {
            Ok(Self {
                name: format!("fake-release-{pid}"),
            })
        }

        fn release_name(&self) -> &str {
            &self.name
        }

        fn wait(&self, holder: FakeHolder<'_>) -> Result<HoldOutcome, HoldError> {
            let mut journal = holder.journal.borrow_mut();
            journal.flushed_at_wait = Some(journal.flushed.clone());
            Ok(holder.outcome)
        }
    }

    /// An output that, like stdout, keeps what is written until it is flushed, or refuses every
    /// write, like a closed pipe.
    struct Output<'a> {
        journal: &'a RefCell<Journal>,
        buffered: Vec<u8>,
        broken: bool,
        /// How many writes it took, each of which a writer straight to a pipe sends on its own.
        writes: usize,
    }

    impl<'a> Output<'a> {
        fn new(journal: &'a RefCell<Journal>) -> Self {
            Self {
                journal,
                buffered: Vec::new(),
                broken: false,
                writes: 0,
            }
        }
    }

    impl Write for Output<'_> {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if self.broken {
                return Err(io::Error::from(io::ErrorKind::BrokenPipe));
            }
            self.writes += 1;
            self.buffered.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            let flushed = String::from_utf8(std::mem::take(&mut self.buffered)).expect("utf-8");
            self.journal.borrow_mut().flushed.push_str(&flushed);
            Ok(())
        }
    }

    /// The IDE reads the line before anything waits: one still in a buffer would leave both
    /// sides waiting on each other.
    #[test]
    fn the_announcement_reaches_the_ide_before_the_wait() {
        let journal = RefCell::new(Journal::default());
        let holder = FakeHolder {
            journal: &journal,
            outcome: HoldOutcome::Released,
        };

        hold::<FakeGate>(42, holder, &mut Output::new(&journal)).expect("hold");

        assert_eq!(
            journal.borrow().flushed_at_wait.as_deref(),
            Some("debug-hold pid=42 release=fake-release-42\n"),
            "the IDE matches this exact line, and it must be flushed when the wait begins"
        );
    }

    /// A writer straight to the program's stdout sends each write as it comes, so a line split
    /// across writes could interleave with the program's own output.
    #[test]
    fn the_announcement_leaves_in_one_write() {
        let journal = RefCell::new(Journal::default());
        let holder = FakeHolder {
            journal: &journal,
            outcome: HoldOutcome::Released,
        };
        let mut output = Output::new(&journal);

        hold::<FakeGate>(42, holder, &mut output).expect("hold");

        assert_eq!(
            output.writes, 1,
            "the whole line goes out at once, with no buffer to keep it together"
        );
    }

    /// No IDE can release a hold it never heard of, so a failed announcement must not wait.
    #[test]
    fn a_failed_announcement_never_waits() {
        let journal = RefCell::new(Journal::default());
        let holder = FakeHolder {
            journal: &journal,
            outcome: HoldOutcome::Released,
        };
        let mut output = Output {
            broken: true,
            ..Output::new(&journal)
        };

        let result = hold::<FakeGate>(42, holder, &mut output);

        assert!(
            matches!(result, Err(HoldError::Announce { pid: 42, .. })),
            "the announcement failed, so the hold does: {result:?}"
        );
        assert_eq!(
            journal.borrow().flushed_at_wait,
            None,
            "nothing waits for a release nobody can send"
        );
    }

    /// The caller resumes a released program and hands back an exited one as it is, so the gate's
    /// answer must reach it unchanged.
    #[test]
    fn the_gates_outcome_is_the_holds_outcome() {
        for outcome in [HoldOutcome::Released, HoldOutcome::ProcessExited] {
            let journal = RefCell::new(Journal::default());
            let holder = FakeHolder {
                journal: &journal,
                outcome,
            };

            let held = hold::<FakeGate>(42, holder, &mut Output::new(&journal)).expect("hold");

            assert_eq!(held, outcome, "the gate said {outcome:?}");
        }
    }
}
