use std::{
    collections::HashSet, env, ffi::CStr, lazy::SyncLazy, mem::size_of, os::unix::io::RawFd,
    path::PathBuf, ptr, sync::Mutex,
};

use frida_gum::{interceptor::Interceptor, Module, NativePointer};
use libc::{c_char, c_int, c_void, size_t, ssize_t, FILE};
use mirrord_protocol::{OpenFileResponse, ReadFileResponse};
use regex::RegexSet;
use tokio::sync::oneshot;
use tracing::{debug, error};

use crate::{
    common::{HookMessage, LayerError, OpenFileHook, ReadFileHook},
    HOOK_SENDER,
};

// TODO(alex) [low] 2022-05-03: `panic::catch_unwind`, but do we need it? If we panic in a C context
// it's bad news without it, but are we in a C context when using LD_PRELOAD?
// https://doc.rust-lang.org/nomicon/ffi.html#ffi-and-panics
// https://doc.rust-lang.org/std/panic/fn.catch_unwind.html

// TODO(alex) [low] 2022-05-03: Safe wrappers around these functions.

// https://users.rust-lang.org/t/problem-overriding-libc-functions-with-ld-preload/41516/4

/// NOTE(alex): Regex that ignores system files + files in the current working directory.
static IGNORE_FILES: SyncLazy<RegexSet> = SyncLazy::new(|| {
    // NOTE(alex): To handle the problem of injecting `open` and friends into project runners (like
    // in a call to `node app.js`, or `cargo run app`), we're ignoring files from the current
    // working directory (as suggested by aviram).
    let current_dir = env::current_dir().unwrap();

    let set = RegexSet::new(&[
        r".*\.so",
        r".*\.d",
        r"^/proc/.*",
        r"^/sys/.*",
        r"^/lib/.*",
        r"^/etc/.*",
        r"^/usr/.*",
        r"^/dev/.*",
        // TODO(alex) [low] 2022-05-13: Investigate why it's trying to load this file all of a
        // sudden. We have to ignore this here as `node` will look for it outside the cwd (it
        // searches up the directory, until it finds).
        r".*/package.json",
        &current_dir.to_string_lossy(),
    ])
    .unwrap();

    set
});

static OPEN_FILES: SyncLazy<Mutex<HashSet<RawFd>>> =
    SyncLazy::new(|| Mutex::new(HashSet::with_capacity(4)));

/// Blocking wrapper around `libc::open` call.
///
/// It's bypassed when trying to load system files, and files from the current working directory
/// (which is different anyways when running in `-agent` context).
///
/// When called for a valid file, it'll block, send a `ClientMessage::OpenFileRequest` to be handled
/// by `mirrord-agent` (`handle_peer_message` function), and wait until it receives a
/// `DaemonMessage::FileOpenResponse`.
pub fn open(path: PathBuf) -> Result<RawFd, LayerError> {
    debug!("open -> trying to open valid file {path:?}");
    let sender = unsafe { HOOK_SENDER.as_ref().unwrap() };
    let (file_channel_tx, file_channel_rx) = oneshot::channel::<OpenFileResponse>();

    let requesting_file = OpenFileHook {
        path,
        file_channel_tx,
    };

    // TODO(alex) [high] 2022-05-18: Possible deadlock when `readFile` is used, as it will call
    // `open` and `read`?
    //
    // TODO(alex) [high] 2022-05-19: We're trying to `open` the same file multiple times, why?
    // We even get different fds.
    sender.blocking_send(HookMessage::OpenFileHook(requesting_file))?;

    debug!("Blocking on file open operation!");
    let open_file = file_channel_rx.blocking_recv()?;
    debug!("After `block_on` call, we have an open file {open_file:#?}!");

    OPEN_FILES.lock().unwrap().insert(open_file.fd);

    Ok(open_file.fd)
}

/// Blocking wrapper around `libc::read` call.
///
/// Bypassed when trying to load system files, and files from the current working directory, see
/// `open`.
pub fn read(fd: RawFd, read_amount: usize) -> Result<Vec<u8>, LayerError> {
    debug!("read -> trying to read valid file {fd:?}");
    let sender = unsafe { HOOK_SENDER.as_ref().unwrap() };
    let (file_channel_tx, file_channel_rx) = oneshot::channel::<ReadFileResponse>();

    let read_file = ReadFileHook {
        fd,
        count: read_amount,
        file_channel_tx,
    };

    // TODO(alex) [high] 2022-05-19: We're trying to `read` the same file multiple times, why? It
    // keeps getting called, so after the first `read` -agent crashes:
    /*
    [2022-05-19T03:50:58Z DEBUG mirrord_agent] FileManager::read -> Trying to read file File {
            fd: 14,
            path: "/var/log/dpkg.log",
            read: true,
            write: false,
        }, with count 65536
    [2022-05-19T03:50:58Z DEBUG mirrord_agent] FileManager::read -> File has len 26604
    [2022-05-19T03:50:58Z ERROR mirrord_agent] Peer encountered error failed to fill whole buffer
         */
    sender.blocking_send(HookMessage::ReadFileHook(read_file))?;

    debug!("Blocking on file read operation!");
    let read_file = file_channel_rx.blocking_recv()?;

    Ok(read_file.bytes)
}

/// NOTE(alex): libc also has an `open64` function. Both functions point to the same address, so
/// trying to intercept them all will result in an `InterceptorAlreadyReplaced` error.
pub(super) unsafe extern "C" fn open_detour(raw_path: *const c_char, oflag: c_int) -> RawFd {
    let path: PathBuf = CStr::from_ptr(raw_path)
        .to_str()
        .expect("Failed converting path from c_char!")
        .into();

    if IGNORE_FILES.is_match(path.to_str().unwrap()) {
        let fd = libc::open(raw_path, oflag);
        fd
    } else {
        match open(path).map_err(|fail| {
            error!("Failed opening file with {fail:#?}");
            libc::EFAULT
        }) {
            Ok(fd) => fd,
            Err(fail) => fail,
        }
    }
}

// TODO(alex) [mid] 2022-05-18: This function should check the `fd` if it exists in our remote file
// manager, then we're dealing with some hooked file, otherwise it's probably a system `read` call.
// It must be done in such a fashion to avoid having some sort of `IGNORE_FILES` check here.
unsafe extern "C" fn read_detour(fd: c_int, buf: *mut c_void, count: size_t) -> ssize_t {
    debug!("read_detour -> fd {fd:#?} count {count:#?}");

    if let Some(managed_fd) = OPEN_FILES.lock().unwrap().get(&fd) {
        debug!("read_detour -> reading a debuggable file.");

        match read(*managed_fd, count).map_err(|fail| {
            error!("Failed reading file with {fail:#?}");
            0
        }) {
            Ok(read_bytes) => {
                let read_ptr = read_bytes.as_ptr();
                ptr::copy(read_ptr, buf as *mut u8, read_bytes.len());

                read_bytes.len().try_into().unwrap()
            }
            Err(fail) => fail,
        }
    } else {
        let read_count = libc::read(fd, buf, count);
        read_count
    }
}

pub(super) fn enable_file_hooks(interceptor: &mut Interceptor) {
    interceptor
        .replace(
            Module::find_export_by_name(None, "open").unwrap(),
            // TODO(alex) [low] 2022-05-03: Is there another way of converting a function into a
            // pointer?
            NativePointer(open_detour as *mut c_void),
            NativePointer(std::ptr::null_mut()),
        )
        .unwrap();

    interceptor
        .replace(
            Module::find_export_by_name(None, "read").unwrap(),
            NativePointer(read_detour as *mut c_void),
            NativePointer(std::ptr::null_mut()),
        )
        .unwrap();
}
