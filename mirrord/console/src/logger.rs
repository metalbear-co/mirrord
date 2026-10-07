use std::{
    cell::Cell,
    fmt,
    io::BufWriter,
    net::{Shutdown, TcpStream},
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use log::LevelFilter;
use mirrord_intproxy_protocol::codec::{self, SyncEncoder};
#[cfg(windows)]
use winapi::um::errhandlingapi::{GetLastError, SetLastError};

use crate::{
    error::Result,
    protocol::{Hello, Record},
};

/// How long one write to the console may block before the console counts as gone.
///
/// Records are sent on the thread that logs them, which inside the layer is usually one of the
/// application's own threads, in the middle of a hook. A console that stops reading must not
/// stall the application, so a write that blocks this long fails, and the logger stops sending.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

type Encoder = SyncEncoder<Record, BufWriter<TcpStream>>;

/// Writes one line where the user can see it, without anything that is unsafe inside a hook.
///
/// The logger calls it once, when it stops sending. The layer passes its own stderr writer, which
/// takes no lock and touches no Rust thread state.
pub type Report = fn(fmt::Arguments<'_>);

thread_local! {
    /// Set while this thread is inside the logger.
    ///
    /// A blocking socket write can run APCs on the writing thread, and an APC that calls a hooked
    /// API logs again, from inside the logger, on the same thread. That call would wait forever
    /// for the encoder lock its own thread holds, so it drops its record instead.
    ///
    /// `const` and without `Drop`, so reading it registers no destructor and stays valid while
    /// the thread is torn down.
    static IN_LOGGER: Cell<bool> = const { Cell::new(false) };
}

/// Keeps the thread's last error across a call into the logger.
///
/// Inside the layer the logger runs in the middle of a hook, often after the hooked API has set
/// the error its caller reads next. Sending a record writes to a socket, which sets it again.
#[cfg(windows)]
struct LastErrorGuard(u32);

#[cfg(windows)]
impl LastErrorGuard {
    fn save() -> Self {
        Self(unsafe { GetLastError() })
    }
}

#[cfg(windows)]
impl Drop for LastErrorGuard {
    fn drop(&mut self) {
        unsafe { SetLastError(self.0) };
    }
}

/// Marks this thread as inside the logger until dropped.
struct ReentryGuard;

impl ReentryGuard {
    /// # Returns
    ///
    /// `None` when this thread is already inside the logger, or can no longer reach its
    /// thread-local storage.
    fn enter() -> Option<Self> {
        let entered = IN_LOGGER.try_with(|inside| !inside.replace(true)).ok()?;
        entered.then_some(Self)
    }
}

impl Drop for ReentryGuard {
    fn drop(&mut self) {
        let _ = IN_LOGGER.try_with(|inside| inside.set(false));
    }
}

/// Console logger that sends log messages to the console app using
/// [`codec`]. It does not use any additional threads, but simply
/// sends the log records through a [`SyncEncoder`].
///
/// A failed write disconnects it for good: the stream is in an unknown state after a partial or
/// timed-out write, and retrying would cost every later record another [`WRITE_TIMEOUT`]. The
/// first failure shuts the socket down, so the console sees the session end, and writes one line
/// through the [`Report`] it was given.
///
/// Records logged after that are dropped. Nothing takes over from inside the logger: switching to
/// file or stderr sinks there would open files and install a subscriber in the middle of
/// whatever hook happened to log.
pub struct ConsoleLogger {
    encoder: Mutex<Encoder>,
    /// Second handle to the encoder's socket, so a failure can shut it down without the lock.
    stream: TcpStream,
    disconnected: AtomicBool,
    report: Report,
}

impl ConsoleLogger {
    /// Connects to the console at `address` and sends it the [`Hello`] message.
    ///
    /// # Arguments
    ///
    /// * `address` - where the console listens.
    /// * `report` - called once, if the logger later stops sending.
    ///
    /// # Errors
    ///
    /// Fails when the console cannot be reached, or when it does not take the hello within
    /// [`WRITE_TIMEOUT`].
    pub fn connect(address: &str, report: Report) -> Result<Self> {
        let mut stream = TcpStream::connect(address)?;
        stream.set_nodelay(true)?;
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;

        send_hello(&mut stream)?;

        Ok(Self {
            stream: stream.try_clone()?,
            encoder: Mutex::new(SyncEncoder::new(BufWriter::new(stream))),
            disconnected: AtomicBool::new(false),
            report,
        })
    }

    /// Runs `send` on the encoder, unless an earlier write already failed or this thread is
    /// already inside the logger (see [`IN_LOGGER`]).
    ///
    /// Any failure, including a poisoned lock, disconnects the logger.
    fn with_encoder(&self, send: impl FnOnce(&mut Encoder) -> codec::Result<()>) {
        let Some(_reentry) = ReentryGuard::enter() else {
            return;
        };

        if self.disconnected.load(Ordering::Acquire) {
            return;
        }

        let error = {
            let Ok(mut encoder) = self.encoder.lock() else {
                self.disconnect(format_args!("the logger's lock is poisoned"));
                return;
            };

            // Another thread may have failed while this one waited for the lock.
            if self.disconnected.load(Ordering::Acquire) {
                return;
            }

            match send(&mut encoder) {
                Ok(()) => return,
                Err(error) => error,
            }
        };

        self.disconnect(format_args!("{error}"));
    }

    /// Stops the logger for good.
    ///
    /// Only the first caller shuts the socket down and reports. It runs outside the encoder lock,
    /// and still inside the caller's [`ReentryGuard`], so a hook reached from the report cannot
    /// log back into this logger.
    fn disconnect(&self, reason: fmt::Arguments<'_>) {
        if self.disconnected.swap(true, Ordering::AcqRel) {
            return;
        }

        let _ = self.stream.shutdown(Shutdown::Both);
        (self.report)(format_args!(
            "mirrord-console logging stopped ({reason}), later logs from this process are dropped"
        ));
    }
}

impl log::Log for ConsoleLogger {
    /// Returns true if the log is generated by mirrord code.
    /// We can have this more fine-grained and also inclusive but
    /// be aware that you might get into a recursive scenario if you let
    /// other module logs slide in.
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.target().contains("mirrord")
    }

    /// Serialize the logs into our protocol then send it over the wire.
    fn log(&self, record: &log::Record) {
        #[cfg(windows)]
        let _last_error = LastErrorGuard::save();

        if self.enabled(record.metadata()) {
            let msg = Record::from(record);

            self.with_encoder(|encoder| {
                encoder.send(&msg)?;
                // Crucial for getting as many of the most interesting logs as possible right
                // before the process exits.
                encoder.flush()
            });
        }
    }

    fn flush(&self) {
        #[cfg(windows)]
        let _last_error = LastErrorGuard::save();

        self.with_encoder(Encoder::flush);
    }
}

/// Send hello message, containing information about the connected process.
fn send_hello(stream: &mut TcpStream) -> Result<()> {
    let hello = Hello::from_env();

    let mut encoder: SyncEncoder<Hello, &mut TcpStream> = SyncEncoder::new(stream);
    encoder.send(&hello)?;

    Ok(())
}

/// Initializes the [`ConsoleLogger`] and sets the global logger to use it.
///
/// # Arguments
///
/// * `address` - where the console listens.
/// * `report` - called once, if the logger later stops sending. See [`ConsoleLogger`].
pub fn init_logger(address: &str, report: Report) -> Result<()> {
    let logger = ConsoleLogger::connect(address, report)?;

    log::set_boxed_logger(Box::new(logger)).map(|()| log::set_max_level(LevelFilter::Trace))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        io::{ErrorKind, Read},
        net::{SocketAddr, TcpListener},
        sync::{atomic::AtomicUsize, mpsc},
        thread,
        time::Instant,
    };

    use log::Log;

    use super::*;

    /// [`Report`] for tests that do not look at the report.
    fn ignore_report(_: fmt::Arguments<'_>) {}

    /// Logs one record with `message` through `logger`.
    fn log_message(logger: &ConsoleLogger, message: &str) {
        logger.log(
            &log::Record::builder()
                .target("mirrord_console::tests")
                .level(log::Level::Info)
                .args(format_args!("{message}"))
                .build(),
        );
    }

    /// A loopback listener for the logger to connect to.
    fn console() -> (TcpListener, SocketAddr) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind console listener");
        let address = listener.local_addr().expect("console address");
        (listener, address)
    }

    /// A console that is not running is an error for the caller to handle, not a panic.
    #[test]
    fn refused_console_is_an_error() {
        let (listener, address) = console();
        drop(listener);

        assert!(ConsoleLogger::connect(&address.to_string(), ignore_report).is_err());
    }

    /// Records sent after the console went away are dropped, the logger stops trying, and it
    /// says so exactly once.
    #[test]
    fn disconnected_console_drops_records() {
        static REPORTS: AtomicUsize = AtomicUsize::new(0);

        fn count_report(_: fmt::Arguments<'_>) {
            REPORTS.fetch_add(1, Ordering::AcqRel);
        }

        let (listener, address) = console();
        let logger = ConsoleLogger::connect(&address.to_string(), count_report).expect("connect");

        let (peer, _) = listener.accept().expect("accept");
        drop(peer);
        drop(listener);

        // The first writes can still land in the socket buffer before the reset arrives.
        let start = Instant::now();
        while !logger.disconnected.load(Ordering::Acquire) && start.elapsed() < WRITE_TIMEOUT * 5 {
            log_message(&logger, "after disconnect");
            thread::sleep(Duration::from_millis(10));
        }

        assert!(logger.disconnected.load(Ordering::Acquire));
        log_message(&logger, "dropped without a panic");
        logger.flush();

        assert_eq!(REPORTS.load(Ordering::Acquire), 1);
    }

    /// A console that accepts but never reads must not block the logging thread for more than
    /// [`WRITE_TIMEOUT`] at a time, and must see the connection close once the logger gives up.
    #[test]
    fn stalled_console_does_not_block_logging() {
        let (listener, address) = console();
        let logger = ConsoleLogger::connect(&address.to_string(), ignore_report).expect("connect");
        let (mut peer, _) = listener.accept().expect("accept");

        let (done_tx, done_rx) = mpsc::channel();
        thread::spawn(move || {
            let message = "x".repeat(1 << 20);
            while !logger.disconnected.load(Ordering::Acquire) {
                log_message(&logger, &message);
            }

            // Once disconnected, logging returns without touching the socket.
            let start = Instant::now();
            log_message(&logger, &message);
            // The logger goes back with the result, so its socket stays open unless the
            // logger itself shut it down.
            let _ = done_tx.send((start.elapsed(), logger));
        });

        let (after_disconnect, _logger) = done_rx
            .recv_timeout(WRITE_TIMEOUT * 15)
            .expect("logging blocked on a stalled console");
        assert!(after_disconnect < WRITE_TIMEOUT);

        peer.set_read_timeout(Some(WRITE_TIMEOUT * 5))
            .expect("console read timeout");
        let mut buffer = vec![0; 1 << 16];
        let closed = loop {
            match peer.read(&mut buffer) {
                Ok(0) => break true,
                Ok(_) => continue,
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    break false;
                }
                Err(_) => break true,
            }
        };
        assert!(closed, "the console never saw the connection close");
    }

    /// A record logged while the same thread is already inside the logger, as an APC run during
    /// a blocking write would, is dropped instead of waiting for the lock that thread holds.
    #[test]
    fn reentrant_logging_does_not_deadlock() {
        let (listener, address) = console();
        let logger = ConsoleLogger::connect(&address.to_string(), ignore_report).expect("connect");
        let (_peer, _) = listener.accept().expect("accept");

        let (done_tx, done_rx) = mpsc::channel();
        thread::spawn(move || {
            logger.with_encoder(|encoder| {
                log_message(&logger, "logged from inside the logger");
                encoder.flush()
            });
            let _ = done_tx.send(logger.disconnected.load(Ordering::Acquire));
        });

        let disconnected = done_rx
            .recv_timeout(WRITE_TIMEOUT * 5)
            .expect("a nested log call deadlocked");
        assert!(!disconnected);
    }

    /// A record sent from inside a hook leaves the caller the error the hooked API set, also
    /// when sending fails and sets an error of its own.
    #[cfg(windows)]
    #[test]
    fn logging_keeps_the_last_error() {
        const CALL_ERROR: u32 = 10048;

        let assert_kept = |log: &dyn Fn()| {
            unsafe { SetLastError(CALL_ERROR) };
            log();
            assert_eq!(unsafe { GetLastError() }, CALL_ERROR);
        };

        let (listener, address) = console();
        let logger = ConsoleLogger::connect(&address.to_string(), ignore_report).expect("connect");

        let (peer, _) = listener.accept().expect("accept");
        assert_kept(&|| log_message(&logger, "while connected"));

        drop(peer);
        drop(listener);

        // The first writes can still land in the socket buffer before the reset arrives.
        let start = Instant::now();
        while !logger.disconnected.load(Ordering::Acquire) && start.elapsed() < WRITE_TIMEOUT * 5 {
            assert_kept(&|| log_message(&logger, "after disconnect"));
            thread::sleep(Duration::from_millis(10));
        }

        assert!(logger.disconnected.load(Ordering::Acquire));
        assert_kept(&|| logger.flush());
    }
}
