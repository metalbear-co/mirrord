use std::{
    collections::HashMap,
    ffi::{CStr, CString},
    lazy::SyncLazy,
    mem::MaybeUninit,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex, Once, RwLock,
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

struct LibcFileOps {
    open: fn(*const c_char, c_int) -> c_int,
    fopen: fn(*const c_char, *const c_char) -> *mut FILE,
    fdopen: fn(c_int, *const c_char) -> *mut FILE,
    read: fn(c_int, *mut c_void, size_t) -> ssize_t,
}

static IGNORE_FILES: SyncLazy<Regex> = SyncLazy::new(|| {
    Regex::new(r#"(.*\.so|.*\.d|/proc/.*|/sys/.*|/lib/.*|/etc/.*|/usr/.*|/dev/.*)"#)
        .expect("Failed building regex to ignore file extensions!")
});

static LIBC_FILE_FUNCTIONS: SyncLazy<LibcFileOps> = SyncLazy::new(|| {
    let libc_open_ptr = Module::find_export_by_name(None, "open").unwrap();
    let libc_open: fn(*const c_char, c_int) -> libc::c_int =
        unsafe { std::mem::transmute(libc_open_ptr) };

    let libc_fopen_ptr = Module::find_export_by_name(None, "fopen").unwrap();
    let libc_fopen: fn(*const c_char, *const c_char) -> *mut FILE =
        unsafe { std::mem::transmute(libc_fopen_ptr) };

    let libc_fdopen_ptr = Module::find_export_by_name(None, "fdopen").unwrap();
    let libc_fdopen: fn(c_int, *const c_char) -> *mut FILE =
        unsafe { std::mem::transmute(libc_fdopen_ptr) };

    let libc_read_ptr = Module::find_export_by_name(None, "read").unwrap();
    let libc_read: fn(c_int, *mut c_void, size_t) -> ssize_t =
        unsafe { std::mem::transmute(libc_read_ptr) };

    LibcFileOps {
        open: libc_open,
        fopen: libc_fopen,
        fdopen: libc_fdopen,
        read: libc_read,
    }
});

// TODO(alex) [high] 2022-05-10: It tries to load "/home/alexc/dev/mirrord/sample-node/app.js", and
// this is not a desired behavior. Don't want it to load the binary the user wants to debug, also
// don't want to load "project" files, like `Cargo.toml`, or `package.json`. Is there a way to
// ignore these files during load, without ignoring them as a regex (it would be bad to just blank
// ignore all possible files of these types)?
/// NOTE(alex): libc also has an `open64` function. Both functions point to the same address, so
/// trying to intercept them all will result in an `InterceptorAlreadyReplaced` error.
pub(super) unsafe extern "C" fn open_detour(path: *const c_char, flags: i32) -> i32 {
    debug!("loaded open_detour");

    let path_str = CStr::from_ptr(path)
        .to_str()
        .expect("Failed converting path from c_char!");
    debug!("trying to load path {:?}", path_str);

    let file_fd = if IGNORE_FILES.is_match(path_str) {
        (LIBC_FILE_FUNCTIONS.open)(path, flags)
    } else {
        (LIBC_FILE_FUNCTIONS.open)(path, flags)
    };

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

    let file_fd = if IGNORE_FILES.is_match(filename_str) {
        (LIBC_FILE_FUNCTIONS.fopen)(filename, mode)
    } else {
        (LIBC_FILE_FUNCTIONS.fopen)(filename, mode)
    };

    file_fd
}

unsafe extern "C" fn fdopen_detour(fd: c_int, mode: *const c_char) -> *mut FILE {
    debug!("loaded fdopen_detour");

    let file_fd = (LIBC_FILE_FUNCTIONS.fdopen)(fd, mode);

    file_fd
}

unsafe extern "C" fn read_detour(fd: c_int, buf: *mut c_void, count: size_t) -> ssize_t {
    debug!("loaded read_detour");

    let read_count = (LIBC_FILE_FUNCTIONS.read)(fd, buf, count);

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

    interceptor
        .replace(
            Module::find_export_by_name(None, "read").unwrap(),
            NativePointer(read_detour as *mut c_void),
            NativePointer(std::ptr::null_mut()),
        )
        .unwrap();
}
