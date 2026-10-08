//! Shared logging initialization for layer (unix) and layer-win (windows).
//!
//! # Warning: this code runs on threads that Rust does not own
//!
//! The layer is a library inside somebody else's process, and its hooks run on every thread of
//! that process, at every stage of a thread's life. On Windows the layer's `DllMain` and its
//! thread-attach notifications run there too, including on threads that Rust is still starting.
//!
//! Rust keeps a handle and an id for each thread, and a thread that Rust spawns records its own as
//! its first act. If anything recorded one on that thread before, Rust aborts the whole process
//! ("current thread handle already set during thread spawn"), and nothing can catch or prevent
//! that. Asking Rust for the current thread's handle or id records one. The Windows layer spawns
//! Rust threads of its own and runs code on threads Rust is still starting, so on Windows, do not
//! use any of these in this file, in any hook, or on the `DllMain` path:
//!
//! - `std::thread::current` and `std::thread::park`.
//! - `tracing_subscriber`'s `with_thread_ids` and `with_thread_names`, which call it. Use
//!   `OsThreadId` instead.
//! - `std::io::stderr` and `std::io::stdout`, and the `println!`, `print!`, `eprintln!`, `eprint!`
//!   and `dbg!` macros, which take a reentrant lock that asks for the thread id. Use
//!   [`report_to_stderr`] instead, which is `eprintln!` on unix and a raw standard-error write on
//!   Windows.
//!
//! Writing to a [`File`] is safe, and so is allocating and formatting.
//!
//! The unix layer has no `DllMain` and no thread-attach callbacks, so these calls stay available
//! there; code shared by both layers goes through the same helpers so that it is safe on either.
//!
//! `layer-lib/clippy.toml` rejects these calls in this crate, so a new one fails CI instead of
//! the application's process. The unix-only sites that keep them carry an `#[allow]` with the
//! reason.

use std::{
    fs::{File, OpenOptions},
    io,
    path::{Path, PathBuf},
    sync::{
        OnceLock,
        atomic::{AtomicBool, Ordering},
    },
};

use chrono::Local;
use tracing_subscriber::{EnvFilter, filter::Directive, fmt::format::FmtSpan, prelude::*};
#[cfg(windows)]
use {
    crate::detour::LastErrorGuard,
    std::{
        any::TypeId,
        io::Write as _,
        panic::{AssertUnwindSafe, catch_unwind},
    },
    tracing::{Dispatch, Event, Metadata, Subscriber, span, subscriber::Interest},
    tracing_subscriber::{
        Layer,
        filter::LevelFilter,
        fmt::{
            FmtContext, FormatEvent, FormatFields, MakeWriter,
            format::{Format, Writer},
        },
        layer::{Context, Filter},
        registry::LookupSpan,
    },
    winapi::um::{
        fileapi::WriteFile, processenv::GetStdHandle, processthreadsapi::GetCurrentThreadId,
        winbase::STD_ERROR_HANDLE,
    },
};

/// Environment variable for specifying layer log directory path
pub const MIRRORD_LAYER_LOG_PATH: &str = "MIRRORD_LAYER_LOG_PATH";

/// The prefix of every layer log file name.
///
/// On Windows the crash monitor opens a registered log only under a name of this shape, see
/// `utils_win::diagnostics::monitor::is_log_name`.
const LOG_FILE_PREFIX: &str = "mirrord-layer_";

/// Address of a running mirrord-console. When set, the console owns the layer's logs.
const MIRRORD_CONSOLE_ADDR: &str = "MIRRORD_CONSOLE_ADDR";

/// Filter for the log file when `MIRRORD_LOG` is not set.
///
/// On Windows the CLI sets [`MIRRORD_LAYER_LOG_PATH`] on every run the user did not set it for,
/// so that a crash bundle always carries layer logs. A file that exists but holds nothing is of
/// no use to anyone, so the file sink gets its own default. `info` holds the early snapshot, the
/// module inventory, and every warning and error, and it keeps the crash bundle small enough to
/// send.
///
/// stderr gets no default, so layer events stay off the user's terminal unless `MIRRORD_LOG`
/// asks for them. The file sink still names its file there once per process, see
/// [`build_log_file_path`].
#[cfg(windows)]
const DEFAULT_FILE_DIRECTIVE: Option<&str> = Some("mirrord=info");

/// Filter for the log file when `MIRRORD_LOG` is not set.
///
/// Outside Windows only the user sets [`MIRRORD_LAYER_LOG_PATH`], and the file follows
/// `MIRRORD_LOG` alone, the same as stderr.
#[cfg(not(windows))]
const DEFAULT_FILE_DIRECTIVE: Option<&str> = None;

/// The layer log file chosen at init, when file logging is active.
static LOG_FILE_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Set by the first [`install_sinks`], so the log file is opened and the subscriber installed
/// once per process, whichever path gets there first.
///
/// An atomic flag rather than a `Once`: a `Once` that another thread is running parks the waiter,
/// and parking asks for the current thread handle.
static SINKS_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Returns the layer's log file path, when file logging is active.
///
/// The crash diagnostics register this with the monitor, so a crash bundles the exact file instead
/// of globbing by pid (which could match a prior session's log).
///
/// # Returns
///
/// The log file path, or `None` when logs only go to stderr.
pub fn current_log_file() -> Option<PathBuf> {
    LOG_FILE_PATH.get().cloned()
}

/// Windows-only support for the layer's own logging.
///
/// The layer is a library inside somebody else's process, and its hooks run on that process's
/// threads at every point of their life. That rules out parts of the ordinary logging path, so
/// this module supplies replacements: a thread id that needs no Rust state, a writer that takes
/// no lock, a filter that stays quiet on a thread whose storage is gone, sinks whose panics stay
/// inside them, and a subscriber that leaves the thread's last error as it found it.
#[cfg(windows)]
mod windows_support {
    use super::*;

    /// Stamps the operating system thread id in front of each event, then delegates.
    ///
    /// Replaces `with_thread_ids`. See the warning at the top of this module. The operating system
    /// id needs no Rust state, and it is the id a debugger or ETW shows for the same thread.
    pub(super) struct OsThreadId<F>(F);

    impl<S, N, F> FormatEvent<S, N> for OsThreadId<F>
    where
        F: FormatEvent<S, N>,
        S: Subscriber + for<'lookup> LookupSpan<'lookup>,
        N: for<'writer> FormatFields<'writer> + 'static,
    {
        fn format_event(
            &self,
            context: &FmtContext<'_, S, N>,
            mut writer: Writer<'_>,
            event: &Event<'_>,
        ) -> std::fmt::Result {
            write!(writer, "tid({:>5}) ", unsafe { GetCurrentThreadId() })?;
            self.0.format_event(context, writer.by_ref(), event)
        }
    }

    /// Writes to the standard error handle without going through [`std::io::stderr`].
    ///
    /// See the warning at the top of this module. Writing straight to the handle keeps the console
    /// output and touches no Rust thread state.
    #[derive(Clone, Copy)]
    pub(super) struct StderrHandle;

    impl io::Write for StderrHandle {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            let mut written = 0u32;
            let wrote = unsafe {
                WriteFile(
                    GetStdHandle(STD_ERROR_HANDLE),
                    buffer.as_ptr().cast(),
                    buffer.len() as u32,
                    &mut written,
                    std::ptr::null_mut(),
                )
            };

            match wrote {
                0 => Err(io::Error::last_os_error()),
                _ => Ok(written as usize),
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'writer> MakeWriter<'writer> for StderrHandle {
        type Writer = StderrHandle;

        fn make_writer(&'writer self) -> Self::Writer {
            *self
        }
    }

    /// Event format for both sinks, with the thread id taken from the operating system.
    ///
    /// No ANSI codes in either sink: the file must not carry escape sequences, and the stderr sink
    /// writes through a raw handle that no terminal has prepared.
    pub(super) fn event_format() -> OsThreadId<Format<tracing_subscriber::fmt::format::Compact>> {
        OsThreadId(
            tracing_subscriber::fmt::format()
                .compact()
                .with_ansi(false)
                .with_file(true)
                .with_line_number(true)
                .with_target(true),
        )
    }

    thread_local! {
        /// Probe for whether this thread can still reach thread-local storage.
        ///
        /// It carries a `Drop`, so the runtime destroys it with the thread's other destructible
        /// thread-locals. Those are destroyed last-registered-first, and this one registers at
        /// the first event on the thread, so values registered after it (`tracing`'s formatting
        /// buffer among them) can already be gone while it still answers "usable". It catches a
        /// thread whose teardown is well under way, not every moment of it.
        static THREAD_STORAGE: StorageProbe = const { StorageProbe };
    }

    /// Empty value whose only job is to be destroyed with the rest of the thread's storage.
    pub(super) struct StorageProbe;

    impl Drop for StorageProbe {
        fn drop(&mut self) {}
    }

    /// Whether this thread can still reach thread-local storage.
    ///
    /// # Returns
    ///
    /// `false` once the thread's storage is gone, when no event may be emitted.
    fn thread_storage_usable() -> bool {
        THREAD_STORAGE.try_with(|_probe| ()).is_ok()
    }

    /// Wraps a filter so that nothing is emitted on a thread whose storage is gone.
    ///
    /// The Windows layer is a library inside somebody else's process, and its hooks run on that
    /// process's threads at every point of their life, including while one is being torn down.
    /// `tracing` reads a thread-local for every event, and reading one after the thread's storage
    /// is destroyed panics with `AccessError`. A panic that leaves a detour ends the process,
    /// so the answer cannot be to catch it: the event must not be built at all.
    ///
    /// A filter is the one place that covers every call site. It is asked before the event is
    /// constructed, so neither the fields nor the formatting layer are reached when it says no, and
    /// no hook has to remember anything.
    ///
    /// Everything else is forwarded to the inner filter, so span directives such as
    /// `MIRRORD_LOG=[connect_detour]=trace` keep working: `EnvFilter` learns which spans they
    /// match when the callsite registers and when the span is created and entered.
    pub(super) struct SkipWhenStorageGone<F>(pub(super) F);

    impl<S, F> Filter<S> for SkipWhenStorageGone<F>
    where
        F: Filter<S>,
    {
        fn enabled(&self, metadata: &Metadata<'_>, context: &Context<'_, S>) -> bool {
            thread_storage_usable() && self.0.enabled(metadata, context)
        }

        /// Registers the callsite with the inner filter, then answers `sometimes` unless the inner
        /// filter rejects the callsite outright.
        ///
        /// The decision depends on the thread that reaches the callsite, not on the callsite, so
        /// letting `tracing` cache an `always` would skip the check on the one thread that needs
        /// it. A `never` is safe to cache: no thread passes a filter that rejects the callsite.
        fn callsite_enabled(&self, metadata: &'static Metadata<'static>) -> Interest {
            let interest = self.0.callsite_enabled(metadata);

            if interest.is_never() {
                interest
            } else {
                Interest::sometimes()
            }
        }

        fn event_enabled(&self, event: &Event<'_>, context: &Context<'_, S>) -> bool {
            self.0.event_enabled(event, context)
        }

        fn max_level_hint(&self) -> Option<LevelFilter> {
            self.0.max_level_hint()
        }

        fn on_new_span(
            &self,
            attrs: &span::Attributes<'_>,
            id: &span::Id,
            context: Context<'_, S>,
        ) {
            self.0.on_new_span(attrs, id, context)
        }

        fn on_record(&self, id: &span::Id, values: &span::Record<'_>, context: Context<'_, S>) {
            self.0.on_record(id, values, context)
        }

        /// Forwarded only while the thread's storage is usable: `EnvFilter` keeps the entered
        /// spans in a thread-local stack.
        fn on_enter(&self, id: &span::Id, context: Context<'_, S>) {
            if thread_storage_usable() {
                self.0.on_enter(id, context)
            }
        }

        /// Forwarded only while the thread's storage is usable, like [`Self::on_enter`].
        fn on_exit(&self, id: &span::Id, context: Context<'_, S>) {
            if thread_storage_usable() {
                self.0.on_exit(id, context)
            }
        }

        fn on_close(&self, id: span::Id, context: Context<'_, S>) {
            self.0.on_close(id, context)
        }
    }

    /// Contains a panic in one of a layer's callbacks, so that it drops that layer's output and
    /// nothing else.
    ///
    /// A detour is an `extern "system"` function, and Rust ends the process rather than let a
    /// panic cross that boundary. On MSVC that is a fast-fail which no handler can catch and no
    /// dump can record. Formatting and writing a line is where logging can panic, so each sink's
    /// layer, its filter included, is wrapped in this.
    ///
    /// The registry underneath is not. It allocates the span ids every layer refers to and keeps
    /// the stack of entered spans, and it calls the layers only after its own part of a callback
    /// is done. A panic contained above it therefore leaves every span it knows of intact, and the
    /// other layers still see each span's whole life, from creation to close. A panic contained
    /// around it would leave no real id to answer `new_span` with.
    ///
    /// A callback that panicked answers as if this layer had no interest. For `enabled` and
    /// `event_enabled` that is "yes", as it is for a filtered layer that rejects something:
    /// "no" would disable the span or event for every layer.
    pub(super) struct ContainPanics<L>(pub(super) L);

    /// Runs `call`, and answers `fallback` when it panics.
    fn contained<R>(call: impl FnOnce() -> R, fallback: R) -> R {
        catch_unwind(AssertUnwindSafe(call)).unwrap_or(fallback)
    }

    impl<S, L> Layer<S> for ContainPanics<L>
    where
        S: Subscriber,
        L: Layer<S>,
    {
        fn on_register_dispatch(&self, subscriber: &Dispatch) {
            contained(|| self.0.on_register_dispatch(subscriber), ())
        }

        /// Not contained: it runs once, while the subscriber is built, and never from a detour.
        fn on_layer(&mut self, subscriber: &mut S) {
            self.0.on_layer(subscriber)
        }

        /// A callsite whose registration panicked is asked again on every use rather than turned
        /// off for good.
        fn register_callsite(&self, metadata: &'static Metadata<'static>) -> Interest {
            contained(|| self.0.register_callsite(metadata), Interest::sometimes())
        }

        fn enabled(&self, metadata: &Metadata<'_>, context: Context<'_, S>) -> bool {
            contained(|| self.0.enabled(metadata, context), true)
        }

        fn on_new_span(
            &self,
            attributes: &span::Attributes<'_>,
            id: &span::Id,
            context: Context<'_, S>,
        ) {
            contained(|| self.0.on_new_span(attributes, id, context), ())
        }

        fn max_level_hint(&self) -> Option<LevelFilter> {
            contained(|| self.0.max_level_hint(), None)
        }

        fn on_record(&self, id: &span::Id, values: &span::Record<'_>, context: Context<'_, S>) {
            contained(|| self.0.on_record(id, values, context), ())
        }

        fn on_follows_from(&self, id: &span::Id, follows: &span::Id, context: Context<'_, S>) {
            contained(|| self.0.on_follows_from(id, follows, context), ())
        }

        fn event_enabled(&self, event: &Event<'_>, context: Context<'_, S>) -> bool {
            contained(|| self.0.event_enabled(event, context), true)
        }

        fn on_event(&self, event: &Event<'_>, context: Context<'_, S>) {
            contained(|| self.0.on_event(event, context), ())
        }

        fn on_enter(&self, id: &span::Id, context: Context<'_, S>) {
            contained(|| self.0.on_enter(id, context), ())
        }

        fn on_exit(&self, id: &span::Id, context: Context<'_, S>) {
            contained(|| self.0.on_exit(id, context), ())
        }

        fn on_close(&self, id: span::Id, context: Context<'_, S>) {
            contained(|| self.0.on_close(id, context), ())
        }

        fn on_id_change(&self, old: &span::Id, new: &span::Id, context: Context<'_, S>) {
            contained(|| self.0.on_id_change(old, new, context), ())
        }

        /// Forwarded, so the registry still finds the per-layer filter of the layer inside.
        unsafe fn downcast_raw(&self, id: TypeId) -> Option<*const ()> {
            if id == TypeId::of::<Self>() {
                return Some(self as *const Self as *const ());
            }

            unsafe { self.0.downcast_raw(id) }
        }
    }

    /// Wraps the whole subscriber so that logging never changes what the caller of a detour
    /// observes.
    ///
    /// Every callback `tracing` makes runs with the thread's last error saved, and puts it back
    /// afterwards, whether the callback returns or a sink's panic is contained inside it (see
    /// [`ContainPanics`]). A hooked API reports failure through that error, and its caller reads
    /// it after the hook returns. Writing a log line calls Win32, which sets it, so a line logged
    /// after the original call would otherwise be what the caller finds. Callsite registration,
    /// the filter questions, span steps and events all go through here, so every log line in the
    /// layer is covered wherever it is written. Only the field values are computed before the
    /// subscriber is called, so a field that calls Win32 itself needs a [`LastErrorGuard`] at the
    /// call site.
    ///
    /// `current_span` keeps the trait's default answer, "unknown": its type lives in
    /// `tracing-core`, and nothing in the layer asks for `Span::current`. The registry still
    /// tracks the current span for its own layers.
    pub(super) struct KeepLastError<S>(pub(super) S);

    impl<S> KeepLastError<S> {
        /// Runs `call` on the inner subscriber with the last error kept.
        fn kept<R>(&self, call: impl FnOnce(&S) -> R) -> R {
            let _last_error = LastErrorGuard::save();
            call(&self.0)
        }
    }

    impl<S: Subscriber> Subscriber for KeepLastError<S> {
        fn on_register_dispatch(&self, subscriber: &Dispatch) {
            self.kept(|inner| inner.on_register_dispatch(subscriber))
        }

        fn register_callsite(&self, metadata: &'static Metadata<'static>) -> Interest {
            self.kept(|inner| inner.register_callsite(metadata))
        }

        fn enabled(&self, metadata: &Metadata<'_>) -> bool {
            self.kept(|inner| inner.enabled(metadata))
        }

        fn max_level_hint(&self) -> Option<LevelFilter> {
            self.kept(|inner| inner.max_level_hint())
        }

        fn new_span(&self, span: &span::Attributes<'_>) -> span::Id {
            self.kept(|inner| inner.new_span(span))
        }

        fn record(&self, span: &span::Id, values: &span::Record<'_>) {
            self.kept(|inner| inner.record(span, values))
        }

        fn record_follows_from(&self, span: &span::Id, follows: &span::Id) {
            self.kept(|inner| inner.record_follows_from(span, follows))
        }

        fn event_enabled(&self, event: &Event<'_>) -> bool {
            self.kept(|inner| inner.event_enabled(event))
        }

        fn event(&self, event: &Event<'_>) {
            self.kept(|inner| inner.event(event))
        }

        fn enter(&self, span: &span::Id) {
            self.kept(|inner| inner.enter(span))
        }

        fn exit(&self, span: &span::Id) {
            self.kept(|inner| inner.exit(span))
        }

        fn clone_span(&self, id: &span::Id) -> span::Id {
            self.kept(|inner| inner.clone_span(id))
        }

        fn try_close(&self, id: span::Id) -> bool {
            self.kept(|inner| inner.try_close(id))
        }

        #[allow(deprecated)]
        fn drop_span(&self, id: span::Id) {
            self.try_close(id);
        }

        unsafe fn downcast_raw(&self, id: TypeId) -> Option<*const ()> {
            if id == TypeId::of::<Self>() {
                return Some(self as *const Self as *const ());
            }

            unsafe { self.0.downcast_raw(id) }
        }
    }
}

#[cfg(windows)]
use windows_support::{
    ContainPanics, KeepLastError, SkipWhenStorageGone, StderrHandle, event_format,
};

/// Writes one line to standard error, safely from anywhere this module runs.
///
/// On Windows this runs on the `DllMain` path, so it writes straight to the standard error handle
/// instead of using `eprintln!`. See the warning at the top of this module.
///
/// On platforms other than Windows there is no loader lock and no `DllMain`, so `eprintln!`
/// stays the right call.
///
/// # Arguments
///
/// * `message` - the line to write, without a trailing newline.
#[cfg_attr(
    not(windows),
    allow(
        clippy::disallowed_macros,
        reason = "no DllMain or loader lock outside Windows"
    )
)]
pub fn report_to_stderr(message: std::fmt::Arguments<'_>) {
    #[cfg(windows)]
    {
        let mut stderr = StderrHandle;
        let _ = writeln!(stderr, "{message}");
    }

    #[cfg(not(windows))]
    eprintln!("{message}");
}

/// Initialize logger. Set the logs to go according to the layer's config either to a trace
/// file, to mirrord-console or to stderr.
///
/// Callers that can hold the Windows loader lock must not use this. It reaches
/// [`init_console_logger`], which opens a socket. Use [`init_tracing_sinks`] instead, and call
/// [`init_console_logger`] once the loader lock is released.
pub fn init_tracing() {
    init_tracing_sinks();
    init_console_logger();
}

/// Attaches the mirrord-console logger, when `MIRRORD_CONSOLE_ADDR` is set.
///
/// Kept apart from [`init_tracing_sinks`] because it opens a TCP connection. The first winsock
/// use in a process loads `mswsock` and any layered service provider, and a module load from
/// inside `DllMain` deadlocks on the loader lock. So this must run only after `DllMain` has
/// returned.
///
/// The console installs a `log` logger and relies on `tracing` forwarding events to `log` while
/// no subscriber is set, which is why [`init_tracing_sinks`] installs none in console mode. When
/// the console cannot be reached, the failure goes to stderr and the file and stderr sinks take
/// over, so a mistyped address or a console that is not running still leaves logs behind.
///
/// A console that fails later, after it connected, gets no such fallback: the logger notices on
/// whatever thread logs next, often inside a hook, where opening a file and installing a
/// subscriber is not safe. It reports once through [`report_to_stderr`] and drops the logs that
/// follow.
pub fn init_console_logger() {
    let Ok(console_addr) = std::env::var(MIRRORD_CONSOLE_ADDR) else {
        return;
    };

    if let Err(error) = mirrord_console::init_logger(&console_addr, report_to_stderr) {
        report_to_stderr(format_args!(
            "mirrord-layer failed to connect to mirrord-console at {console_addr} (error: \
             {error}), logging to the file and stderr sinks instead"
        ));
        install_sinks();
    }
}

/// Initializes the file and stderr sinks, and nothing that can load a module.
///
/// This is the half of [`init_tracing`] that is safe to run under the Windows loader lock: it
/// reads environment variables, creates a directory, opens a file, and installs the subscriber.
///
/// With `MIRRORD_CONSOLE_ADDR` set it installs nothing, and the console owns logging from
/// [`init_console_logger`] on. A subscriber here would stop `tracing` from forwarding to the
/// console's `log` logger, and installing it also claims the global `log` logger slot, so the
/// console could not attach at all. Events from before the console attaches are dropped.
pub fn init_tracing_sinks() {
    if std::env::var(MIRRORD_CONSOLE_ADDR).is_ok() {
        return;
    }

    install_sinks();
}

/// Opens the log file and installs the subscriber, once per process.
///
/// A failure to install is reported rather than a panic: another subscriber may already hold the
/// global default, and that is no reason to take down the application.
fn install_sinks() {
    if SINKS_INSTALLED.swap(true, Ordering::AcqRel) {
        return;
    }

    let log_file = std::env::var(MIRRORD_LAYER_LOG_PATH)
        .ok()
        .and_then(|log_dir| {
            open_log_file_from_env(&log_dir)
                .map_err(|err| {
                    report_to_stderr(format_args!(
                        "Failed to open log file from MIRRORD_LAYER_LOG_PATH (error: {err})"
                    ));
                    err
                })
                .ok()
        });

    if let Err(error) = build_subscriber(log_file).try_init() {
        report_to_stderr(format_args!(
            "mirrord-layer failed to install its log sinks (error: {error})"
        ));
    }
}

/// Builds the subscriber that [`install_sinks`] installs.
///
/// Each sink carries its own filter. A single filter on the registry would force one choice on
/// both: give the file a default and every `mirrord exec` prints layer logs to the user's terminal,
/// or keep stderr quiet and the file stays empty.
///
/// Neither sink reports its own write failures. `tracing_subscriber` would do that with
/// `eprintln!`, which on Windows is exactly the call this module must not make, and which panics
/// when stderr itself is the sink that failed.
///
/// On Windows the whole subscriber is wrapped in `KeepLastError`, so that logging inside a hook
/// leaves the caller's last error alone, and each sink in `ContainPanics`, so that a sink that
/// panics loses its own line rather than the process.
///
/// It is separate from [`install_sinks`] so that the tests can install it for one thread only,
/// instead of racing on the global default subscriber.
///
/// # Arguments
///
/// * `log_file` - open log file, when file logging is active.
///
/// # Returns
///
/// The subscriber, with a file layer when `log_file` is given, and always a stderr layer.
fn build_subscriber(log_file: Option<File>) -> impl tracing::Subscriber + Send + Sync {
    #[cfg(windows)]
    let file_layer = log_file.map(|file| {
        ContainPanics(
            tracing_subscriber::fmt::layer()
                .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
                .with_ansi(false) // File logs should avoid ANSI escape codes
                .with_writer(file)
                .event_format(event_format())
                .log_internal_errors(false)
                .with_filter(SkipWhenStorageGone(env_filter(DEFAULT_FILE_DIRECTIVE))),
        )
    });

    #[cfg(not(windows))]
    #[allow(
        clippy::disallowed_methods,
        reason = "thread ids are safe to ask for outside Windows"
    )]
    let file_layer = log_file.map(|file| {
        tracing_subscriber::fmt::layer()
            .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
            .with_thread_ids(true)
            .with_ansi(false) // File logs should avoid ANSI escape codes
            .with_writer(file)
            .with_file(true)
            .with_line_number(true)
            .with_target(true)
            .compact()
            .log_internal_errors(false)
            .with_filter(env_filter(DEFAULT_FILE_DIRECTIVE))
    });

    // Always add stderr layer
    #[cfg(windows)]
    let stderr_layer = ContainPanics(
        tracing_subscriber::fmt::layer()
            .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
            .with_writer(StderrHandle)
            .event_format(event_format())
            .log_internal_errors(false)
            .with_filter(SkipWhenStorageGone(env_filter(None))),
    );

    #[cfg(not(windows))]
    #[allow(
        clippy::disallowed_methods,
        reason = "thread ids and std::io::stderr are safe outside Windows"
    )]
    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
        .with_thread_ids(true)
        .compact()
        .with_writer(std::io::stderr)
        .log_internal_errors(false)
        .with_filter(env_filter(None));

    // Note (Daniel): to disable ansi code properly in file, stderr must be last
    // according to this Stackoverflow comment:
    // https://stackoverflow.com/questions/79118770/strange-symbols-ansi-in-a-log-file-when-using-tracing-subscriber#comment139523806_79119452
    let subscriber = tracing_subscriber::registry()
        .with(file_layer)
        .with(stderr_layer);

    #[cfg(windows)]
    let subscriber = KeepLastError(subscriber);

    subscriber
}

/// Builds a filter from `MIRRORD_LOG`.
///
/// # Arguments
///
/// * `default_directive` - directive to use when `MIRRORD_LOG` is not set or holds no valid
///   directive. A `MIRRORD_LOG` value that parses replaces it, so anyone who sets the variable
///   keeps full control of both sinks.
///
/// # Returns
///
/// The filter. Without a default directive and without `MIRRORD_LOG`, it enables nothing at all.
fn env_filter(default_directive: Option<&str>) -> EnvFilter {
    let builder = EnvFilter::builder().with_env_var("MIRRORD_LOG");

    match default_directive.and_then(|directive| directive.parse::<Directive>().ok()) {
        Some(directive) => builder.with_default_directive(directive).from_env_lossy(),
        None => builder.from_env_lossy(),
    }
}

fn open_log_file_from_env(log_dir: &str) -> io::Result<File> {
    let path = build_log_file_path(log_dir)?;

    // Open for writing, truncating existing content.
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)?;

    // Remember the path so the crash diagnostics can register and bundle this exact file.
    let _ = LOG_FILE_PATH.set(PathBuf::from(path));
    Ok(file)
}

/// Build the log file path inside the provided directory, ensuring it exists.
///
/// Also names the file on stderr, so whoever reads the terminal knows where the layer logs went.
fn build_log_file_path(log_dir: &str) -> io::Result<String> {
    let timestamp = Local::now().format("%Y%m%d_%H%M%S");
    let pid = std::process::id();
    let process_name = sanitized_process_name();

    let dir_path = Path::new(log_dir);
    if dir_path.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "log directory path is empty",
        ));
    }
    std::fs::create_dir_all(dir_path)?;

    let file_name = format!("{LOG_FILE_PREFIX}{timestamp}_{process_name}_pid{pid}");
    let full_path = dir_path.join(file_name);

    report_to_stderr(format_args!(
        "mirrord-layer initializing file logger for process '{process_name}' (pid={pid}), \
         logging to file: {full_path:?}"
    ));

    Ok(full_path.to_string_lossy().into_owned())
}

fn sanitized_process_name() -> String {
    let raw_name = std::env::current_exe()
        .ok()
        .and_then(|path| path.file_stem()?.to_str().map(String::from))
        .unwrap_or_else(|| "unknown".to_owned());

    let mut sanitized: String = raw_name
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect();

    if sanitized.is_empty() {
        sanitized = "unknown".to_owned();
    }

    sanitized
}

#[cfg(test)]
mod tests {
    use std::{fs, net::TcpListener, sync::Mutex};

    use tempfile::tempdir;
    use tracing::subscriber::with_default;
    use tracing_subscriber::{filter::LevelFilter, layer::Filter, registry::Registry};
    #[cfg(windows)]
    use {
        tracing::span::{Attributes, Id, Record},
        winapi::um::errhandlingapi::{GetLastError, SetLastError},
    };

    use super::*;

    /// The Windows crash monitor opens a registered log only under a name that passes its rule,
    /// and it repeats the prefix, because it cannot depend on this crate. A name this logger builds
    /// that failed the rule would drop the log from every crash bundle without a word.
    #[cfg(windows)]
    #[test]
    fn the_crash_monitor_accepts_the_log_file_name() {
        assert_eq!(LOG_FILE_PREFIX, utils_win::diagnostics::monitor::LOG_PREFIX);

        let directory = tempdir().expect("temp dir");
        let path = build_log_file_path(&directory.path().to_string_lossy()).expect("log path");
        let name = Path::new(&path)
            .file_name()
            .and_then(|name| name.to_str())
            .expect("a file name");
        assert!(
            utils_win::diagnostics::monitor::is_log_name(name),
            "{name} is refused by the crash monitor"
        );
    }

    /// The subscriber reads process-wide environment variables, so the tests that change them must
    /// run one at a time.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Sets (or with `None` removes) the environment variable `name`.
    ///
    /// # Returns
    ///
    /// The previous value, to hand back to this function once the test is done.
    fn swap_env(name: &str, value: Option<&str>) -> Option<String> {
        let previous = std::env::var(name).ok();
        unsafe {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
        previous
    }

    /// Paths of the files in `dir`, sorted.
    fn files_in(dir: &Path) -> Vec<PathBuf> {
        let mut paths = fs::read_dir(dir)
            .expect("read temp log dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .collect::<Vec<_>>();
        paths.sort();
        paths
    }

    /// Writes events through a file subscriber built with the given `MIRRORD_LOG` value.
    ///
    /// # Arguments
    ///
    /// * `mirrord_log` - value for `MIRRORD_LOG`, or `None` to remove the variable.
    /// * `emit` - closure that logs the events. Each test must give its own closure, because
    ///   `tracing` caches the interest of a callsite.
    ///
    /// # Returns
    ///
    /// The contents of the log file that the subscriber created.
    fn log_file_contents(mirrord_log: Option<&str>, emit: impl FnOnce()) -> String {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let temp_dir = tempdir().expect("temp dir");

        let prev_log_level = swap_env("MIRRORD_LOG", mirrord_log);

        let log_file =
            open_log_file_from_env(&temp_dir.path().to_string_lossy()).expect("log file");
        with_default(build_subscriber(Some(log_file)), emit);

        swap_env("MIRRORD_LOG", prev_log_level.as_deref());

        let log_path = files_in(temp_dir.path())
            .into_iter()
            .next()
            .expect("log file not created");

        fs::read_to_string(&log_path).expect("read log file")
    }

    #[test]
    fn log_file_is_written_with_mirrord_log_set() {
        let contents = log_file_contents(Some("debug"), || tracing::info!("logging smoke test"));
        assert!(
            contents.contains("logging smoke test"),
            "expected the event in the log file, got {contents:?}"
        );
    }

    /// `mirrord exec` on Windows does not set `MIRRORD_LOG`, but the CLI does set a log path so
    /// that the crash bundle has layer logs, and an empty file in that bundle helps nobody.
    /// Outside Windows the file follows `MIRRORD_LOG` alone, like stderr.
    #[test]
    fn log_file_default_without_mirrord_log_set() {
        let contents = log_file_contents(None, || tracing::info!("logging smoke test"));
        let written = contents.contains("logging smoke test");
        assert_eq!(
            written,
            cfg!(windows),
            "unexpected log file contents without MIRRORD_LOG: {contents:?}"
        );
    }

    /// A span directive enables events inside the named span, and only there.
    ///
    /// The file filter learns which spans a directive matches when a callsite registers and when
    /// a span is created and entered, so every one of those steps has to reach it.
    #[test]
    fn span_directive_enables_events_inside_the_span() {
        let contents = log_file_contents(Some("[span_directive_test]=trace"), || {
            tracing::trace!("outside the span");

            let span = tracing::info_span!("span_directive_test");
            let _entered = span.enter();
            tracing::trace!("inside the span");
        });

        assert!(
            contents.contains("inside the span"),
            "expected the event inside the span, got {contents:?}"
        );
        assert!(
            !contents.contains("outside the span"),
            "expected no event outside the span, got {contents:?}"
        );
    }

    /// A console that cannot be reached must leave the file and stderr sinks in its place, and a
    /// second attempt must neither open a second file nor panic over the installed subscriber.
    ///
    /// This installs the global subscriber, so it is the only test that may call the `init_`
    /// functions. The other tests scope theirs to one thread with [`with_default`].
    #[test]
    fn unreachable_console_falls_back_to_the_sinks() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner());
        let temp_dir = tempdir().expect("temp dir");

        let listener = TcpListener::bind("127.0.0.1:0").expect("bind console listener");
        let console_addr = listener.local_addr().expect("console address").to_string();
        drop(listener);

        let prev_console_addr = swap_env(MIRRORD_CONSOLE_ADDR, Some(&console_addr));
        let prev_log_path = swap_env(
            MIRRORD_LAYER_LOG_PATH,
            Some(&temp_dir.path().to_string_lossy()),
        );

        init_tracing();
        init_console_logger();

        swap_env(MIRRORD_CONSOLE_ADDR, prev_console_addr.as_deref());
        swap_env(MIRRORD_LAYER_LOG_PATH, prev_log_path.as_deref());

        assert!(SINKS_INSTALLED.load(Ordering::Acquire));
        assert_eq!(files_in(temp_dir.path()).len(), 1);
    }

    /// Without `MIRRORD_LOG`, stderr stays quiet on every platform, and only the Windows file sink
    /// has a default. Otherwise every `mirrord exec` prints layer logs to the user's terminal.
    #[test]
    fn stderr_stays_quiet_without_mirrord_log_set() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner());

        let prev_log_level = swap_env("MIRRORD_LOG", None);

        let stderr_hint = Filter::<Registry>::max_level_hint(&env_filter(None));
        let file_hint = Filter::<Registry>::max_level_hint(&env_filter(DEFAULT_FILE_DIRECTIVE));

        swap_env("MIRRORD_LOG", prev_log_level.as_deref());

        let expected_file_hint = if cfg!(windows) {
            LevelFilter::INFO
        } else {
            LevelFilter::OFF
        };

        assert_eq!(stderr_hint, Some(LevelFilter::OFF));
        assert_eq!(file_hint, Some(expected_file_hint));
    }

    /// What a hooked API leaves for its caller, and what writing a log line leaves behind.
    #[cfg(windows)]
    const CALL_ERROR: u32 = 10048;
    #[cfg(windows)]
    const LOGGING_ERROR: u32 = 0xDEAD;

    /// A subscriber that sets the last error in every callback, the way a log write does.
    #[cfg(windows)]
    struct ClobberingSubscriber;

    #[cfg(windows)]
    impl ClobberingSubscriber {
        fn clobber() {
            unsafe { SetLastError(LOGGING_ERROR) };
        }
    }

    #[cfg(windows)]
    impl Subscriber for ClobberingSubscriber {
        fn enabled(&self, _: &Metadata<'_>) -> bool {
            Self::clobber();
            true
        }

        fn new_span(&self, _: &Attributes<'_>) -> Id {
            Self::clobber();
            Id::from_u64(1)
        }

        fn record(&self, _: &Id, _: &Record<'_>) {
            Self::clobber();
        }

        fn record_follows_from(&self, _: &Id, _: &Id) {
            Self::clobber();
        }

        fn event(&self, _: &Event<'_>) {
            Self::clobber();
        }

        fn enter(&self, _: &Id) {
            Self::clobber();
        }

        fn exit(&self, _: &Id) {
            Self::clobber();
        }

        fn try_close(&self, _: Id) -> bool {
            Self::clobber();
            false
        }
    }

    /// Fails the call the way a hooked API does, logs, and asserts that the caller still reads
    /// the call's error.
    #[cfg(windows)]
    fn assert_logging_keeps_the_last_error(log: impl FnOnce()) {
        unsafe { SetLastError(CALL_ERROR) };
        log();
        assert_eq!(
            unsafe { GetLastError() },
            CALL_ERROR,
            "the caller must read the call's error, not the log write's"
        );
    }

    /// A log line written between a failing call and the hook's return, and a span around it,
    /// leave the caller the error the call set.
    #[cfg(windows)]
    #[test]
    fn logging_keeps_the_last_error() {
        with_default(KeepLastError(ClobberingSubscriber), || {
            assert_logging_keeps_the_last_error(|| tracing::info!("after the original call"));
            assert_logging_keeps_the_last_error(|| {
                let span = tracing::info_span!("hook", value = tracing::field::Empty);
                let _entered = span.enter();
                span.record("value", 1);
                tracing::debug!("inside the hook's span");
            });
        });
    }

    /// A sink that sets the last error and then panics in every callback, the way a formatting
    /// layer that fails half-way through a line would.
    #[cfg(windows)]
    struct PanickingLayer;

    #[cfg(windows)]
    impl PanickingLayer {
        fn fail(callback: &str) {
            ClobberingSubscriber::clobber();
            panic!("the sink failed in {callback}");
        }
    }

    #[cfg(windows)]
    impl<S: Subscriber> Layer<S> for PanickingLayer {
        fn on_new_span(&self, _: &Attributes<'_>, _: &Id, _: Context<'_, S>) {
            Self::fail("on_new_span");
        }

        fn on_event(&self, _: &Event<'_>, _: Context<'_, S>) {
            Self::fail("on_event");
        }

        fn on_enter(&self, _: &Id, _: Context<'_, S>) {
            Self::fail("on_enter");
        }

        fn on_exit(&self, _: &Id, _: Context<'_, S>) {
            Self::fail("on_exit");
        }

        fn on_close(&self, _: Id, _: Context<'_, S>) {
            Self::fail("on_close");
        }
    }

    /// A healthy sink next to [`PanickingLayer`], writing down every span step and event it sees,
    /// as the registry describes them.
    #[cfg(windows)]
    #[derive(Clone, Default)]
    struct Witness(std::sync::Arc<Mutex<Vec<String>>>);

    #[cfg(windows)]
    impl Witness {
        fn saw(&self, what: String) {
            self.0.lock().expect("witness").push(what);
        }

        fn seen(&self) -> Vec<String> {
            self.0.lock().expect("witness").clone()
        }
    }

    #[cfg(windows)]
    impl<S> Layer<S> for Witness
    where
        S: Subscriber + for<'lookup> LookupSpan<'lookup>,
    {
        fn on_new_span(&self, attributes: &Attributes<'_>, _: &Id, _: Context<'_, S>) {
            self.saw(format!("new {}", attributes.metadata().name()));
        }

        fn on_event(&self, _: &Event<'_>, context: Context<'_, S>) {
            let current = context.lookup_current();
            let parent = current.as_ref().and_then(|span| span.parent());
            self.saw(format!(
                "event in {:?} under {:?}",
                current.map(|span| span.name()),
                parent.map(|span| span.name())
            ));
        }

        fn on_enter(&self, id: &Id, context: Context<'_, S>) {
            let span = context.span(id).expect("a span the registry knows");
            self.saw(format!("enter {}", span.name()));
        }

        fn on_exit(&self, id: &Id, context: Context<'_, S>) {
            let span = context.span(id).expect("a span the registry knows");
            self.saw(format!("exit {}", span.name()));
        }

        fn on_close(&self, id: Id, context: Context<'_, S>) {
            let span = context.span(&id).expect("a span the registry knows");
            self.saw(format!("close {}", span.name()));
        }
    }

    /// A sink that panics loses its own output and nothing else: the panic never crosses a
    /// detour's boundary, the caller still reads its call's error, and the registry still gives
    /// every other sink each span's real id, the stack of entered spans and the close.
    #[cfg(windows)]
    #[test]
    fn a_panicking_sink_leaves_the_spans_to_the_others() {
        let witness = Witness::default();
        let subscriber = KeepLastError(
            tracing_subscriber::registry()
                .with(ContainPanics(PanickingLayer))
                .with(witness.clone()),
        );

        with_default(subscriber, || {
            assert_logging_keeps_the_last_error(|| {
                let outer = tracing::info_span!("outer");
                let _outer = outer.enter();
                let inner = tracing::info_span!("inner");
                let entered = inner.enter();
                tracing::info!("inside both");
                drop(entered);
                drop(inner);
                tracing::info!("inside the outer one");
            });
            tracing::info!("outside");
        });

        assert_eq!(
            witness.seen(),
            [
                "new outer",
                "enter outer",
                "new inner",
                "enter inner",
                r#"event in Some("inner") under Some("outer")"#,
                "exit inner",
                "close inner",
                r#"event in Some("outer") under None"#,
                "exit outer",
                "close outer",
                "event in None under None",
            ]
        );
    }
}
