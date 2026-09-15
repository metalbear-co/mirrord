//! Keeps other processes' stderr off the interface.
//!
//! `kube` runs the kubeconfig's auth exec plugin (`gke-gcloud-auth-plugin`, `kubelogin`, ...) with
//! our own stderr inherited, unless the kubeconfig opted out of interactive mode. Those plugins log
//! freely: an expired credential turns into a multi-line klog complaint written straight to the
//! terminal. With the alternate screen up and the terminal in raw mode, it lands stair-stepped
//! across whatever ratatui drew last, and it stays there - ratatui only repaints the cells it
//! believes have changed, and it has no idea someone else wrote over them.
//!
//! So for as long as the interface owns the terminal, fd 2 points at a pipe instead. A reader
//! thread drains it and keeps the last lines, because for a failed credential plugin that text
//! ("Please run: $ gcloud auth login") is usually the only actionable part of the whole failure -
//! [`recent`] is what puts it in front of the user, in the connection error dialog.
//!
//! What the drain thread must never do is *log* those lines. Under `mirrord tui` the CLI's tracing
//! subscriber writes to `std::io::stderr`, which is this very pipe: one captured line would be
//! re-emitted into the pipe, read back, re-emitted again, and so on without bound. Keeping the
//! lines in memory and showing them in the dialog is the whole delivery mechanism.

#[cfg(unix)]
mod platform {
    use std::{
        collections::VecDeque,
        io::{BufRead, BufReader},
        os::fd::FromRawFd,
        sync::{
            Mutex, OnceLock,
            atomic::{AtomicI32, Ordering},
        },
    };

    /// How many captured lines to keep for [`recent`]. Enough for a plugin's multi-line complaint,
    /// bounded so a chatty one cannot grow this without limit.
    const KEEP_LINES: usize = 32;

    /// The captured lines, oldest first.
    fn kept() -> &'static Mutex<VecDeque<String>> {
        static KEPT: OnceLock<Mutex<VecDeque<String>>> = OnceLock::new();

        KEPT.get_or_init(Default::default)
    }

    /// The last lines written to the captured stderr, oldest first.
    pub fn recent() -> Vec<String> {
        kept()
            .lock()
            .map(|lines| lines.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Records one captured line.
    ///
    /// Deliberately does not log: see the module docs - anything that reaches a subscriber writing
    /// to stderr comes straight back down this pipe.
    fn keep(line: String) {
        let line = line.trim_end().to_owned();
        if line.is_empty() {
            return;
        }

        if let Ok(mut lines) = kept().lock() {
            while lines.len() >= KEEP_LINES {
                lines.pop_front();
            }
            lines.push_back(line);
        }
    }

    /// The terminal's own stderr, kept aside so [`restore`] can put it back. `-1` means stderr was
    /// never redirected, which also makes `restore` a no-op when `capture` gave up.
    static SAVED: AtomicI32 = AtomicI32::new(-1);

    /// Points fd 2 at a pipe and drains it into the log.
    ///
    /// Best-effort: if any of the file descriptor juggling fails there is nothing useful to do
    /// about it, so stderr keeps going to the terminal, as it would have without this module.
    pub fn capture() {
        if SAVED.load(Ordering::SeqCst) >= 0 {
            return;
        }

        // SAFETY: every call is checked, and each descriptor is closed on exactly one path.
        let read = unsafe {
            let saved = libc::dup(libc::STDERR_FILENO);
            if saved < 0 {
                return;
            }

            let mut ends = [0 as libc::c_int; 2];
            if libc::pipe(ends.as_mut_ptr()) != 0 {
                libc::close(saved);
                return;
            }
            let [read, write] = ends;

            // Children inherit fd 2 - that is the whole point - but they have no business holding
            // the read end open, which would keep the drain thread alive past our own exit.
            libc::fcntl(read, libc::F_SETFD, libc::FD_CLOEXEC);

            if libc::dup2(write, libc::STDERR_FILENO) < 0 {
                libc::close(saved);
                libc::close(read);
                libc::close(write);
                return;
            }

            // fd 2 is now the only handle on the write end, so restoring it closes the pipe and
            // the drain thread sees the end of the stream.
            libc::close(write);
            SAVED.store(saved, Ordering::SeqCst);

            std::fs::File::from_raw_fd(read)
        };

        let drain = std::thread::Builder::new()
            .name("stderr-drain".to_owned())
            .spawn(move || {
                for line in BufReader::new(read).lines().map_while(Result::ok) {
                    keep(line);
                }
            });

        if drain.is_err() {
            // Nothing would empty the pipe, and a child that filled it would block forever.
            restore();
        }
    }

    /// Puts the terminal's stderr back on fd 2.
    ///
    /// Idempotent, and safe to call from a panic hook - which is where it matters most, since the
    /// panic message itself goes to stderr and has to reach the terminal rather than the pipe.
    pub fn restore() {
        let saved = SAVED.swap(-1, Ordering::SeqCst);
        if saved < 0 {
            return;
        }

        // SAFETY: `saved` came from `dup` above, and the swap means only one caller can get it.
        unsafe {
            libc::dup2(saved, libc::STDERR_FILENO);
            libc::close(saved);
        }
    }
}

#[cfg(not(unix))]
mod platform {
    /// Redirecting stderr is unix-only for now; elsewhere a plugin's output still reaches the
    /// terminal, and the interface repaints over it on the next full redraw.
    pub fn capture() {}

    /// Counterpart of [`capture`], with nothing to undo.
    pub fn restore() {}

    /// Always empty: without [`capture`] there is nothing to have captured.
    pub fn recent() -> Vec<String> {
        Vec::new()
    }
}

pub use platform::{capture, recent, restore};
