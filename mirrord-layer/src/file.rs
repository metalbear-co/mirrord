use std::{
    ffi::{CStr, CString},
    mem::MaybeUninit,
    sync::{
        atomic::{AtomicU32, Ordering},
        Once,
    },
};

use frida_gum::{interceptor::Interceptor, Module, NativePointer};
use lazy_static::lazy_static;
use libc::{c_char, c_int, c_void, mode_t, size_t, ssize_t, FILE};
use regex::Regex;
use tracing::debug;

use crate::GUM;

// TODO(alex) [mid] 2022-05-03: `panic::catch_unwind`, but do we need it? If we panic in a C context
// it's bad news without it, but are we in a C context when using LD_PRELOAD?
// https://doc.rust-lang.org/nomicon/ffi.html#ffi-and-panics
// https://doc.rust-lang.org/std/panic/fn.catch_unwind.html

// TODO(alex) [low] 2022-05-03: Safe wrappers around these functions.

// https://users.rust-lang.org/t/problem-overriding-libc-functions-with-ld-preload/41516/4

lazy_static! {
    static ref IGNORE_FILES: Regex = Regex::new(
        r#"(.*\.so|.*\.d|.*\.toml|.*\.gitconfig|/proc/.*|/sys/.*|/lib/.*|/etc/.*|/usr/.*|/dev/.*|rust-toolchain/.*|\.cargo/.*)"#
    )
    .expect("Failed building regex to ignore file extensions!");
}

static mut REAL_HIT_COUNT: AtomicU32 = AtomicU32::new(0);
static mut FAKE_HIT_COUNT: AtomicU32 = AtomicU32::new(0);

/// NOTE(alex): libc also has an `open64` function. Both functions point to the same address, so
/// trying to intercept them all will result in an `InterceptorAlreadyReplaced` error.
pub(super) unsafe extern "C" fn open_detour(path: *const c_char, flags: i32) -> i32 {
    debug!("loaded open_detour");

    debug!("trying to load file {:?}", path);

    let open_ptr = libc::dlsym(libc::RTLD_NEXT, CString::new("open").unwrap().into_raw());
    let real_open: fn(*const c_char, i32) -> libc::c_int = std::mem::transmute(open_ptr);

    let path_str = CStr::from_ptr(path)
        .to_str()
        .expect("Failed converting path from c_char!");
    debug!("trying to load path {:?}", path_str);

    let file_fd = if IGNORE_FILES.is_match(path_str) {
        debug!("returning real open");
        REAL_HIT_COUNT.fetch_add(1, Ordering::SeqCst);

        real_open(path, flags)
    } else {
        debug!("returning fake open");
        FAKE_HIT_COUNT.fetch_add(1, Ordering::SeqCst);

        real_open(path, flags)
    };

    debug!("file_fd {file_fd:?}");
    debug!("files REAL hit count: {REAL_HIT_COUNT:?}");
    debug!("files FAKE hit count: {FAKE_HIT_COUNT:?}");

    file_fd
}

/// NOTE(alex): libc also has an `fopen64` function. Both functions point to the same address, so
/// trying to intercept them all will result in an `InterceptorAlreadyReplaced` error.
unsafe extern "C" fn fopen_detour(filename: *const c_char, mode: *const c_char) -> *mut FILE {
    debug!("loaded fopen_detour");

    let fopen_ptr = libc::dlsym(libc::RTLD_NEXT, CString::new("fopen").unwrap().into_raw());
    let real_fopen: fn(*const c_char, *const c_char) -> *mut FILE = std::mem::transmute(fopen_ptr);

    let filename_str = CStr::from_ptr(filename)
        .to_str()
        .expect("Failed converting filename from c_char!");
    debug!("trying to load file {:?}", filename_str);

    let file_fd = if IGNORE_FILES.is_match(filename_str) {
        debug!("returning real fopen");
        REAL_HIT_COUNT.fetch_add(1, Ordering::SeqCst);

        real_fopen(filename, mode)
    } else {
        debug!("returning fake fopen");
        FAKE_HIT_COUNT.fetch_add(1, Ordering::SeqCst);

        real_fopen(filename, mode)
    };

    debug!("file_fd {file_fd:?}");
    debug!("files REAL hit count: {REAL_HIT_COUNT:?}");
    debug!("files FAKE hit count: {FAKE_HIT_COUNT:?}");

    file_fd
}

unsafe extern "C" fn fdopen_detour(fd: c_int, mode: *const c_char) -> *mut FILE {
    debug!("loaded fdopen_detour");

    let fdopen_ptr = libc::dlsym(libc::RTLD_NEXT, CString::new("fdopen").unwrap().into_raw());
    let real_fdopen: fn(c_int, *const c_char) -> *mut FILE = std::mem::transmute(fdopen_ptr);
    debug!("trying to load fd {:?}", fd);

    let file_fd = real_fdopen(fd, mode);

    debug!("file_fd {file_fd:?}");

    file_fd
}

unsafe extern "C" fn read_detour(fd: c_int, buf: *mut c_void, count: size_t) -> ssize_t {
    debug!("loaded read_detour");

    let read_ptr = libc::dlsym(libc::RTLD_NEXT, CString::new("read").unwrap().into_raw());
    let real_read: fn(c_int, *mut c_void, size_t) -> ssize_t = std::mem::transmute(read_ptr);
    debug!("trying to load fd {:?}", fd);

    let read_count = real_read(fd, buf, count);

    debug!("read_count {read_count:?}");

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
            Module::find_export_by_name(None, "fdopen").unwrap(),
            NativePointer(fdopen_detour as *mut c_void),
            NativePointer(std::ptr::null_mut()),
        )
        .unwrap();
}
