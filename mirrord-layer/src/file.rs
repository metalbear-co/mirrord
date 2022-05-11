use std::{
    borrow::Borrow,
    collections::HashMap,
    ffi::CStr,
    lazy::SyncLazy,
    net::SocketAddr,
    os::unix::{io::RawFd, prelude::AsRawFd},
    path::Path,
    sync::Mutex,
};

use frida_gum::{interceptor::Interceptor, Module, NativePointer};
use libc::{c_char, c_int, c_short, c_void, size_t, ssize_t, FILE};
use multi_map::MultiMap;
use queues::Queue;
use regex::Regex;
use socketpair::{socketpair_stream, SocketpairStream};
use tracing::debug;

// TODO(alex) [low] 2022-05-03: `panic::catch_unwind`, but do we need it? If we panic in a C context
// it's bad news without it, but are we in a C context when using LD_PRELOAD?
// https://doc.rust-lang.org/nomicon/ffi.html#ffi-and-panics
// https://doc.rust-lang.org/std/panic/fn.catch_unwind.html

// TODO(alex) [low] 2022-05-03: Safe wrappers around these functions.

// https://users.rust-lang.org/t/problem-overriding-libc-functions-with-ld-preload/41516/4

/// NOTE(alex): Regex that ignores system files + files in the current working directory.
static IGNORE_FILES: SyncLazy<Regex> = SyncLazy::new(|| {
    // NOTE(alex): To handle the problem of injecting `open` and friends into project runners (like
    // in a call to `node app.js`, or `cargo run app`), we're ignoring files from the current
    // working directory (as suggested by aviram).
    let mut cwd_buffer = [0; libc::PATH_MAX as usize];
    let cwd = unsafe { libc::getcwd(cwd_buffer.as_mut_ptr(), cwd_buffer.len()) };

    let cwd_str = unsafe { CStr::from_ptr(cwd) }
        .to_str()
        .expect("Failed converting cwd from c_char!");
    debug!("cwd {cwd_str}");

    let system_ignore = r#"(.*\.so|.*\.d|/proc/.*|/sys/.*|/lib/.*|/etc/.*|/usr/.*|/dev/.*)"#;

    Regex::new(&format!("{system_ignore}|{cwd_str}/.*"))
        .expect("Failed building regex to ignore files!")
});

pub struct Open {
    file: std::fs::File,
}

pub enum FileState {
    AwaitingRemote,
    Open(Open),
}

pub struct File {
    pub fd: RawFd,
    pub read_file: SocketpairStream,
    pub write_file: SocketpairStream,
}

impl Borrow<RawFd> for File {
    fn borrow(&self) -> &RawFd {
        &self.fd
    }
}

static mut FILES: SyncLazy<Mutex<Vec<File>>> = SyncLazy::new(|| Mutex::new(Vec::with_capacity(32)));

// TODO(alex) [high] 2022-05-10: Create this action on the other side, so that we can properly
// implement file faking. Look at how `socket` is doing the message passing.
pub fn open_file(path: *const c_char, oflag: c_int) -> RawFd {
    debug!("open_file -> path: {path:?}, flags: {oflag:?}");
    let fd = unsafe { libc::open(path, oflag) };

    let path_str = unsafe { CStr::from_ptr(path) }
        .to_str()
        .expect("Failed converting path from c_char!");

    if IGNORE_FILES.is_match(path_str) {
        return fd;
    }

    fd
}

/// NOTE(alex): libc also has an `open64` function. Both functions point to the same address, so
/// trying to intercept them all will result in an `InterceptorAlreadyReplaced` error.
pub(super) unsafe extern "C" fn open_detour(path: *const c_char, oflag: c_int) -> RawFd {
    let path_str = CStr::from_ptr(path)
        .to_str()
        .expect("Failed converting path from c_char!");
    debug!("open_detour -> path {path_str}");
    open_file(path, oflag)
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

    interceptor
        .replace(
            Module::find_export_by_name(None, "fopen").unwrap(),
            NativePointer(fopen_detour as *mut c_void),
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
