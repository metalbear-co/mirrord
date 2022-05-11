use std::{
    collections::HashMap, ffi::CStr, lazy::SyncLazy, net::SocketAddr, os::unix::prelude::AsRawFd,
    path::Path, sync::Mutex,
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

type LibcOpen = fn(*const c_char, c_int) -> c_int;
type LibcFopen = fn(*const c_char, *const c_char) -> *mut FILE;
type LibcFdopen = fn(c_int, *const c_char) -> *mut FILE;
type LibcRead = fn(c_int, *mut c_void, size_t) -> ssize_t;

struct LibcFileOps {
    open: LibcOpen,
    fopen: LibcFopen,
    fdopen: LibcFdopen,
    read: LibcRead,
}

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

static LIBC_FILE_FUNCTIONS: SyncLazy<LibcFileOps> = SyncLazy::new(|| {
    let libc_open_ptr = Module::find_export_by_name(None, "open").unwrap();
    let libc_open: LibcOpen = unsafe { std::mem::transmute(libc_open_ptr) };

    let libc_fopen_ptr = Module::find_export_by_name(None, "fopen").unwrap();
    let libc_fopen: LibcFopen = unsafe { std::mem::transmute(libc_fopen_ptr) };

    let libc_fdopen_ptr = Module::find_export_by_name(None, "fdopen").unwrap();
    let libc_fdopen: LibcFdopen = unsafe { std::mem::transmute(libc_fdopen_ptr) };

    let libc_read_ptr = Module::find_export_by_name(None, "read").unwrap();
    let libc_read: LibcRead = unsafe { std::mem::transmute(libc_read_ptr) };

    LibcFileOps {
        open: libc_open,
        fopen: libc_fopen,
        fdopen: libc_fdopen,
        read: libc_read,
    }
});

static mut FILES: SyncLazy<Files> = SyncLazy::new(|| Files::default());

type FileFd = c_int;
type Port = c_short;
type ConnectionId = c_short;
type TCPBuffer = Vec<u8>;

pub struct File {
    pub read_fd: FileFd,
    pub read_file: SocketpairStream,
    pub write_file: SocketpairStream,
}

pub struct ConnectionFile {
    pub read_fd: FileFd,
    pub read_file: SocketpairStream,
    pub write_file: SocketpairStream,
    pub address: SocketAddr,
    pub state: ConnectionState,
}

#[derive(PartialEq)]
pub enum ConnectionState {
    Bound,
    Listening,
}

pub struct DataFile {
    pub connection_id: ConnectionId,
    #[allow(dead_code)]
    pub read_socket: SocketpairStream, /* Though this is unread, it's necessary to keep the
                                        * socket open */
    pub write_socket: SocketpairStream,
    pub address: SocketAddr,
}

pub struct Files {
    new_files: Mutex<HashMap<FileFd, File>>,
    connections: Mutex<MultiMap<FileFd, Port, ConnectionFile>>,
    data: Mutex<MultiMap<FileFd, ConnectionId, DataFile>>,

    /// Used to enqueue incoming connection ids from the agent, to be read in the 'accept' call.
    connection_queues: Mutex<HashMap<FileFd, Queue<ConnectionId>>>,

    /// Used to store data that arrived before its connection was opened. When the connection is
    /// later opened, pending_data is read and emptied.
    pending_data: Mutex<HashMap<ConnectionId, TCPBuffer>>,
}

impl Default for Files {
    fn default() -> Self {
        Self {
            new_files: Mutex::new(HashMap::new()),
            connections: Mutex::new(MultiMap::new()),
            data: Mutex::new(MultiMap::new()),
            connection_queues: Mutex::new(HashMap::new()),
            pending_data: Mutex::new(HashMap::new()),
        }
    }
}

impl Files {
    // TODO(alex) [high] 2022-05-10: Create this action on the other side, so that we can properly
    // implement file faking. Look at how `socket` is doing the message passing.
    pub fn open_file<P: AsRef<Path>>(&self, path: P) -> FileFd {
        let path = path.as_ref();
        debug!("open_file -> path {path:?}");

        let (write_socket, read_socket) = socketpair_stream().unwrap();
        let read_fd = read_socket.as_raw_fd();
        let socket = File {
            read_fd,
            read_file: read_socket,
            write_file: write_socket,
        };

        self.new_files.lock().unwrap().insert(read_fd, socket);

        read_fd
    }
}

/// NOTE(alex): libc also has an `open64` function. Both functions point to the same address, so
/// trying to intercept them all will result in an `InterceptorAlreadyReplaced` error.
pub(super) unsafe extern "C" fn open_detour(path: *const c_char, flags: c_int) -> FileFd {
    let path_str = CStr::from_ptr(path)
        .to_str()
        .expect("Failed converting path from c_char!");
    debug!("open_detour -> path {path_str}");

    if IGNORE_FILES.is_match(path_str) {
        debug!("open_detour -> ignored path {path_str}");
        (LIBC_FILE_FUNCTIONS.open)(path, flags)
    } else {
        debug!("open_detour -> opening fake path {path_str}");
        FILES.open_file(path_str)
    }
}

/// NOTE(alex): libc also has a `fopen64` function. Both functions point to the same address, so
/// trying to intercept them all will result in an `InterceptorAlreadyReplaced` error.
unsafe extern "C" fn fopen_detour(filename: *const c_char, mode: *const c_char) -> *mut FILE {
    let filename_str = CStr::from_ptr(filename)
        .to_str()
        .expect("Failed converting filename from c_char!");
    debug!("fopen_detour -> filename {filename_str}");

    let file_fd = if IGNORE_FILES.is_match(filename_str) {
        (LIBC_FILE_FUNCTIONS.fopen)(filename, mode)
    } else {
        (LIBC_FILE_FUNCTIONS.fopen)(filename, mode)
    };

    file_fd
}

unsafe extern "C" fn fdopen_detour(fd: c_int, mode: *const c_char) -> *mut FILE {
    debug!("fdopen_detour -> fd {fd:#?}");
    let file_fd = (LIBC_FILE_FUNCTIONS.fdopen)(fd, mode);

    file_fd
}

unsafe extern "C" fn read_detour(fd: c_int, buf: *mut c_void, count: size_t) -> ssize_t {
    debug!("read_detour -> fd {fd:#?}");

    // let mut stat: libc::stat = std::mem::zeroed();
    // let stat_result = libc::fstat(fd, &mut stat);
    // debug!("read_detour -> stat_result {stat_result:?}, stat {stat:#?}");

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
