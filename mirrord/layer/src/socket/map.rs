use std::{
    collections::HashMap,
    os::unix::io::RawFd,
    sync::{Arc, Mutex, MutexGuard},
};

use base64::prelude::*;
use bincode::error::EncodeError;

use super::{SHARED_SOCKETS_ENV_VAR, UserSocket};

/// A wrapper around `Mutex<HashMap<...>>` for cleanly managing
/// `UserSocket` instances, ensuring it's never treated as a POD type
/// so reference counting can be used.
#[derive(Debug)]
pub struct SocketMap {
    inner: Mutex<HashMap<RawFd, Arc<Mutex<UserSocketWrapper>>>>,
}

#[derive(Debug)]
struct UserSocketWrapper(UserSocket);
impl Drop for UserSocketWrapper {
    fn drop(&mut self) {
        let _ = self.0.close();
    }
}

impl UserSocketWrapper {
    fn make(inner: UserSocket) -> Arc<Mutex<Self>> {
        Arc::new(Mutex::new(Self(inner)))
    }

    fn clone_inner(&self) -> UserSocket {
        self.0.clone()
    }
}

pub struct Lock<'a>(#[allow(unused)] MutexGuard<'a, HashMap<RawFd, Arc<Mutex<UserSocketWrapper>>>>);

impl SocketMap {
    pub(super) fn read_from_env() -> Self {
        let map = std::env::var(SHARED_SOCKETS_ENV_VAR)
            .ok()
            .and_then(|encoded| {
                BASE64_URL_SAFE
                    .decode(encoded.into_bytes())
                    .inspect_err(|error| {
                        tracing::warn!(
                            ?error,
                            "failed decoding base64 value from {SHARED_SOCKETS_ENV_VAR}"
                        )
                    })
                    .ok()
            })
            .and_then(|decoded| {
                bincode::decode_from_slice::<Vec<(i32, UserSocket)>, _>(
                    &decoded,
                    bincode::config::standard(),
                )
                .inspect_err(|error| {
                    tracing::warn!(?error, "failed parsing shared sockets env value")
                })
                .ok()
            })
            .map(|(fds_and_sockets, _)| {
                Mutex::new(HashMap::from_iter(fds_and_sockets.into_iter().filter_map(
                    |(fd, socket)| {
                        // Do not inherit sockets that are `FD_CLOEXEC`.
                        // NOTE: The original `fcntl` is called instead of `FN_FCNTL` because the
                        // latter may be null at this point, likely due to
                        // child-spawning functions that mess with memory
                        // such as fork/exec. See: https://github.com/metalbear-co/mirrord-intellij/issues/374
                        if unsafe { libc::fcntl(fd, libc::F_GETFD, 0) != -1 } {
                            Some((fd, UserSocketWrapper::make(socket)))
                        } else {
                            None
                        }
                    },
                )))
            })
            .unwrap_or_default();

        Self { inner: map }
    }

    pub fn insert(&self, fd: RawFd, socket: UserSocket) {
        self.inner
            .lock()
            .unwrap()
            .insert(fd, UserSocketWrapper::make(socket));
    }

    pub fn serialize(&self) -> Result<String, EncodeError> {
        let lock = self.inner.lock().unwrap();

        let pairs = lock
            .iter()
            .map(|(key, value)| (*key, value.lock().unwrap().clone_inner()))
            .collect::<Vec<_>>();

        bincode::encode_to_vec(pairs, bincode::config::standard())
            .map(|bytes| BASE64_URL_SAFE.encode(bytes))
    }

    pub fn get(&self, fd: &RawFd) -> Option<UserSocket> {
        self.inner
            .lock()
            .unwrap()
            .get(fd)
            .map(|t| t.lock().unwrap().clone_inner())
    }

    pub fn remove(&self, fd: &RawFd) -> bool {
        self.inner.lock().unwrap().remove(fd).is_some()
    }

    // /// Calls `fun` on the entry corresponding to `fd`, returns the
    // /// result. Allows for efficiently retrieving
    // /// fields from the contained `UserSocket`s without making bulky
    // /// copies of the entire struct.
    // pub fn get<F, T>(&self, fd: &RawFd, fun: F) -> Option<T>
    // where
    //     F: FnOnce(&UserSocket) -> T,
    //     T: 'static,
    // {
    //     self.inner
    //         .lock()
    //         .unwrap()
    //         .get(&fd)
    //         .map(|t| fun(&t.lock().unwrap().0))
    // }

    pub fn any<F>(&self, mut fun: F) -> bool
    where
        F: FnMut((&RawFd, &UserSocket)) -> bool,
    {
        let lock = self.inner.lock().unwrap();
        for (fd, socket) in lock.iter() {
            if fun((fd, &socket.lock().unwrap().0)) {
                return true;
            }
        }

        false
    }

    pub fn find_map<F, T>(&self, mut fun: F) -> Option<T>
    where
        F: FnMut((&RawFd, &UserSocket)) -> Option<T>,
    {
        let lock = self.inner.lock().unwrap();
        for (fd, socket) in lock.iter() {
            let val = fun((fd, &socket.lock().unwrap().0));
            if val.is_some() {
                return val;
            }
        }

        None
    }

    /// Modify the entry corresponding to `fd`.
    ///
    /// Returns `true` if the entry existed and the modification was
    /// done, false otherwise.
    pub fn modify<F>(&self, fd: &RawFd, fun: F) -> bool
    where
        F: FnOnce(&mut UserSocket),
    {
        let lock = self.inner.lock().unwrap();

        let Some(entry) = lock.get(fd) else {
            return false;
        };

        fun(&mut entry.lock().unwrap().0);
        true
    }

    /// Clone the entry in `from` to `to`.
    ///
    /// Returns whether `from` was occupied, i.e. the operation succeded.
    pub fn clone_entry(&self, from: &RawFd, to: RawFd) -> bool {
        let mut lock = self.inner.lock().unwrap();
        let Some(e) = lock.get(from) else {
            return false;
        };
        let cloned = Arc::clone(e);
        lock.insert(to, cloned);

        true
    }

    /// Lock this map.
    ///
    /// Note that this cannot be used for any operations on the map,
    /// and should be used for solely controlling when the internal
    /// mutex is locked, like in [`super::super::fork_detour`].
    pub fn lock(&self) -> Lock<'_> {
        Lock(self.inner.lock().unwrap())
    }
}
