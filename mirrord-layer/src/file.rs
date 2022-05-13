use std::{
    borrow::Borrow,
    collections::HashMap,
    ffi::CStr,
    lazy::SyncLazy,
    mem::MaybeUninit,
    net::SocketAddr,
    os::unix::{io::RawFd, prelude::AsRawFd},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, Once},
};

use frida_gum::{interceptor::Interceptor, Module, NativePointer};
use futures::channel::oneshot;
use libc::{c_char, c_int, c_short, c_void, size_t, ssize_t, FILE};
use mirrord_protocol::FileOpen;
use multi_map::MultiMap;
use queues::Queue;
use rand::prelude::*;
use regex::{Regex, RegexSet};
use socketpair::{socketpair_stream, SocketpairStream};
use tokio::sync::Notify;
use tracing::{debug, error, info, warn};

use crate::{
    common::{HookMessage, OpenFile},
    HOOK_SENDER, RUNTIME,
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
    let mut cwd_buffer = [0; libc::PATH_MAX as usize];
    let cwd = unsafe { libc::getcwd(cwd_buffer.as_mut_ptr(), cwd_buffer.len()) };

    let cwd_str = unsafe { CStr::from_ptr(cwd) }
        .to_str()
        .expect("Failed converting cwd from c_char!");

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
        cwd_str,
    ])
    .unwrap();

    set
});

pub static mut FILE_HOOK_SENDER: SyncLazy<Mutex<Vec<oneshot::Sender<FileOpen>>>> =
    SyncLazy::new(|| Mutex::new(Vec::with_capacity(4)));
static FILE_HOOK_INITIALIZER: Once = Once::new();

pub enum FileState {
    AwaitingRemote,
}

pub struct File {
    pub fake_fd: RawFd,
    pub fd: RawFd,
    pub state: FileState,
}

impl Borrow<RawFd> for File {
    fn borrow(&self) -> &RawFd {
        &self.fd
    }
}

static mut FILES: SyncLazy<Mutex<Vec<File>>> = SyncLazy::new(|| Mutex::new(Vec::with_capacity(32)));

/// Blocking wrapper around `libc::open` call.
///
/// It's bypassed when trying to load system files, and files from the current working directory
/// (which is different anyways when running in `-agent` context).
///
/// When called for a valid file, it'll block, send a `ClientMessage::OpenFileRequest` to be handled
/// by `mirrord-agent` (`handle_peer_message` function), and wait until it receives a
/// `DaemonMessage::FileOpenResponse`.
pub fn open(raw_path: *const c_char, oflag: c_int) -> RawFd {
    let path: PathBuf = unsafe { CStr::from_ptr(raw_path) }
        .to_str()
        .expect("Failed converting path from c_char!")
        .into();

    if IGNORE_FILES.is_match(path.to_str().unwrap()) {
        let fd = unsafe { libc::open(raw_path, oflag) };
        fd
    } else {
        debug!("open -> trying to open valid file {path:?}");
        let sender = unsafe { HOOK_SENDER.as_ref().unwrap() };
        let (file_channel_tx, file_channel_rx) = oneshot::channel::<FileOpen>();

        unsafe {
            FILE_HOOK_SENDER.lock().unwrap().push(file_channel_tx);
        };

        let requesting_file = OpenFile { path };

        match sender.blocking_send(HookMessage::OpenFile(requesting_file)) {
            Ok(_) => {}
            Err(fail) => {
                error!("open: failed to send open message: {fail:?}!");
                return libc::EFAULT;
            }
        };

        debug!("Blocking on file open operation!");

        let file_open = RUNTIME.block_on(async {
            let file_open = file_channel_rx.await.unwrap();
            file_open
        });

        debug!("After `block_on` call, we have an open file {file_open:#?}!");

        // TODO(alex) [high] 2022-05-12: Instead of returning this `fake_fd` thing, we should block
        // while waiting for an `-agent` reply.
        // To do this:
        // - create a (send_tx, recv_tx) for file ops;
        // - do `recv_tx.recv()` here to block until the "file is open" message is received;
        // - send an open file request to `-agent` in `poll_agent`;
        // - when the reply comes back, `send_tx.send(file)` in `poll_agent`;

        file_open.fd
    }
}

/// NOTE(alex): libc also has an `open64` function. Both functions point to the same address, so
/// trying to intercept them all will result in an `InterceptorAlreadyReplaced` error.
pub(super) unsafe extern "C" fn open_detour(path: *const c_char, oflag: c_int) -> RawFd {
    open(path, oflag)
}

/// NOTE(alex): libc also has a `fopen64` function. Both functions point to the same address, so
/// trying to intercept them all will result in an `InterceptorAlreadyReplaced` error.
unsafe extern "C" fn fopen_detour(filename: *const c_char, mode: *const c_char) -> *mut FILE {
    let filename_str = CStr::from_ptr(filename)
        .to_str()
        .expect("Failed converting filename from c_char!");
    debug!("fopen_detour -> filename {filename_str}");

    let file_ptr = if IGNORE_FILES.is_match(filename_str) {
        libc::fopen(filename, mode)
    } else {
        libc::fopen(filename, mode)
    };

    debug!("fopen_detour -> file_ptr {file_ptr:?}");
    file_ptr
}

unsafe extern "C" fn read_detour(fd: c_int, buf: *mut c_void, count: size_t) -> ssize_t {
    debug!("read_detour -> fd {fd:#?}");

    let read_count = libc::read(fd, buf, count);

    read_count
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

    // interceptor
    //     .replace(
    //         Module::find_export_by_name(None, "fopen").unwrap(),
    //         NativePointer(fopen_detour as *mut c_void),
    //         NativePointer(std::ptr::null_mut()),
    //     )
    //     .unwrap();

    // interceptor
    //     .replace(
    //         Module::find_export_by_name(None, "read").unwrap(),
    //         NativePointer(read_detour as *mut c_void),
    //         NativePointer(std::ptr::null_mut()),
    //     )
    //     .unwrap();
}
