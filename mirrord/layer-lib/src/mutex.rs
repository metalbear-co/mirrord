use std::{
    backtrace::Backtrace,
    sync::{LockResult, MutexGuard, TryLockError},
    time::{Duration, Instant},
};

const NODEADLOCK_ENV: &str = "MIRRORD_NODEADLOCK";
const TRY_LOCK_SLEEP: Duration = Duration::from_millis(10);
const TRY_LOCK_TIMEOUT: Duration = Duration::from_secs(1);

fn nodeadlock_enabled() -> bool {
    std::env::var(NODEADLOCK_ENV)
        .ok()
        .and_then(|value| value.trim().to_ascii_lowercase().parse::<bool>().ok())
        .unwrap_or(false)
}

#[derive(Debug)]
pub struct Mutex<T> {
    inner: std::sync::Mutex<T>,
    nodeadlock: bool,
}

impl<T: Default> Default for Mutex<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> Mutex<T> {
    pub fn new(value: T) -> Self {
        Self {
            inner: std::sync::Mutex::new(value),
            nodeadlock: nodeadlock_enabled(),
        }
    }

    pub fn lock(&self) -> LockResult<MutexGuard<'_, T>> {
        if !self.nodeadlock {
            return self.inner.lock();
        }

        let start = Instant::now();
        loop {
            match self.inner.try_lock() {
                Ok(guard) => return Ok(guard),
                Err(TryLockError::Poisoned(error)) => return Err(error),
                Err(TryLockError::WouldBlock) => {
                    if start.elapsed() >= TRY_LOCK_TIMEOUT {
                        let backtrace = Backtrace::force_capture();
                        panic!(
                            "MIRRORD_NODEADLOCK: failed to acquire mutex within {:?}.\nBacktrace:\n{backtrace:?}",
                            TRY_LOCK_TIMEOUT
                        );
                    }
                    std::thread::sleep(TRY_LOCK_SLEEP);
                }
            }
        }
    }
}
