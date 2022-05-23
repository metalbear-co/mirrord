use std::{
    collections::{HashMap, HashSet},
    env,
    ffi::{CStr, CString},
    fs::OpenOptions,
    io::SeekFrom,
    lazy::SyncLazy,
    os::unix::{io::RawFd, prelude::OpenOptionsExt},
    path::PathBuf,
    ptr,
    sync::Mutex,
};

use frida_gum::{interceptor::Interceptor, Module, NativePointer};
use libc::{
    c_char, c_int, c_long, c_void, off64_t, off_t, size_t, ssize_t, O_ACCMODE, O_RDONLY, O_RDWR,
    O_WRONLY,
};
use mirrord_protocol::{
    OpenFileResponse, OpenOptionsInternal, ReadFileResponse, SeekFileResponse, WriteFileResponse,
};
use regex::RegexSet;
use tokio::sync::oneshot;
use tracing::{debug, error, info};

use crate::{
    common::{HookMessage, LayerError, OpenFileHook, ReadFileHook, SeekFileHook, WriteFileHook},
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

type LocalFd = RawFd;
type RemoteFd = RawFd;

static OPEN_FILES: SyncLazy<Mutex<HashMap<LocalFd, RemoteFd>>> =
    SyncLazy::new(|| Mutex::new(HashMap::with_capacity(4)));

#[derive(Debug, Clone)]
pub(crate) struct ReadFile {
    bytes: Vec<u8>,
    read_amount: usize,
}

// TODO(alex) [high] 2022-05-22: Hook file `write`.

// TODO(alex) [mid] 2022-05-22: Start dealing with errors in a better way. Ideally every response
// type should return a proper result.

/// Blocking wrapper around `libc::open` call.
///
/// It's bypassed when trying to load system files, and files from the current working directory
/// (which is different anyways when running in `-agent` context).
///
/// When called for a valid file, it'll block, send a `ClientMessage::OpenFileRequest` to be handled
/// by `mirrord-agent` (`handle_peer_message` function), and wait until it receives a
/// `DaemonMessage::FileOpenResponse`.
pub(crate) fn open(path: PathBuf, open_options: OpenOptionsInternal) -> Result<RawFd, LayerError> {
    debug!("open -> trying to open valid file {path:?}.");

    let sender = unsafe { HOOK_SENDER.as_ref().unwrap() };
    let (file_channel_tx, file_channel_rx) = oneshot::channel::<OpenFileResponse>();

    let requesting_file = OpenFileHook {
        path,
        open_options,
        file_channel_tx,
    };

    sender.blocking_send(HookMessage::OpenFileHook(requesting_file))?;

    debug!("open -> await response from remote");
    let open_file = file_channel_rx.blocking_recv()?;

    let fake_file_name = CString::new(open_file.fd.to_string()).unwrap();
    let local_file_fd = unsafe { libc::memfd_create(fake_file_name.as_ptr(), 0) };

    debug!(
        "open -> local_fd {local_file_fd:#?} | remote_fd {:#?}",
        open_file.fd
    );

    OPEN_FILES
        .lock()
        .unwrap()
        .insert(local_file_fd, open_file.fd);

    Ok(local_file_fd)
}

/// Blocking wrapper around `libc::read` call.
///
/// Bypassed when trying to load system files, and files from the current working directory, see
/// `open`.
pub(crate) fn read(fd: RawFd, read_amount: usize) -> Result<ReadFile, LayerError> {
    debug!("read -> trying to read valid file {fd:?}");

    let sender = unsafe { HOOK_SENDER.as_ref().unwrap() };
    let (file_channel_tx, file_channel_rx) = oneshot::channel::<ReadFileResponse>();

    let reading_file = ReadFileHook {
        fd,
        buffer_size: read_amount,
        file_channel_tx,
    };

    sender.blocking_send(HookMessage::ReadFileHook(reading_file))?;

    let read_file_response = file_channel_rx.blocking_recv()?;

    let read_file = ReadFile {
        bytes: read_file_response.bytes,
        read_amount: read_file_response.read_amount,
    };

    Ok(read_file)
}

pub(crate) fn lseek(fd: RawFd, seek_from: SeekFrom) -> Result<u64, LayerError> {
    debug!("lseek -> trying to seek valid file {fd:?}.");

    let sender = unsafe { HOOK_SENDER.as_ref().unwrap() };
    let (file_channel_tx, file_channel_rx) = oneshot::channel::<SeekFileResponse>();

    let seeking_file = SeekFileHook {
        fd,
        seek_from,
        file_channel_tx,
    };

    sender.blocking_send(HookMessage::SeekFileHook(seeking_file))?;

    let seek_file_response = file_channel_rx.blocking_recv()?;
    let result_offset = seek_file_response.result_offset;

    Ok(result_offset)
}

pub(crate) fn write(fd: RawFd, write_bytes: Vec<u8>) -> Result<isize, LayerError> {
    debug!("write -> trying to write valid file {fd:?}.");

    let sender = unsafe { HOOK_SENDER.as_ref().unwrap() };
    let (file_channel_tx, file_channel_rx) = oneshot::channel::<WriteFileResponse>();

    let writing_file = WriteFileHook {
        fd,
        write_bytes,
        file_channel_tx,
    };

    sender.blocking_send(HookMessage::WriteFileHook(writing_file))?;

    let write_file_response = file_channel_rx.blocking_recv()?;

    Ok(write_file_response.written_amount.try_into().unwrap())
}

/// NOTE(alex): libc also has an `open64` function. Both functions point to the same address, so
/// trying to intercept them all will result in an `InterceptorAlreadyReplaced` error.
pub(super) unsafe extern "C" fn open_detour(raw_path: *const c_char, oflag: c_int) -> RawFd {
    let path: PathBuf = CStr::from_ptr(raw_path)
        .to_str()
        .expect("Failed converting path from c_char!")
        .into();

    if IGNORE_FILES.is_match(path.to_str().unwrap()) {
        let bypassed_fd = libc::open(raw_path, oflag);
        debug!("open_detour -> bypassed_fd {bypassed_fd:#?}");

        bypassed_fd
    } else {
        let open_options = OpenOptionsInternal {
            read: (oflag & O_ACCMODE == O_RDONLY) || (oflag & O_ACCMODE == O_RDWR),
            write: (oflag & O_ACCMODE == O_WRONLY) || (oflag & O_ACCMODE == O_RDWR),
            flags: oflag,
        };

        match open(path, open_options).map_err(|fail| {
            error!("Failed opening file with {fail:#?}");
            libc::EFAULT
        }) {
            Ok(fd) => fd,
            Err(fail) => fail,
        }
    }
}

unsafe extern "C" fn read_detour(fd: RawFd, buf: *mut c_void, count: size_t) -> ssize_t {
    // NOTE(alex): We're only interested in files that are handled by `mirrord-agent`.
    if let Some(remote_fd) = OPEN_FILES.lock().unwrap().get(&fd).cloned() {
        // TODO(alex) [high] 2022-05-23: After changing things to `OpenOptions`, running sample
        // just hangs here, why?
        debug!("read_detour -> managed_fd {remote_fd:#?} | fd {fd:#?} | count {count:#?}");

        match read(remote_fd, count).map_err(|fail| {
            error!("Failed reading file with {fail:#?}");
            -1
        }) {
            Ok(read_file) => {
                let ReadFile { bytes, read_amount } = read_file;

                let read_ptr = bytes.as_ptr();
                let out_buffer = buf.cast();
                ptr::copy(read_ptr, out_buffer, read_amount);

                // WARN(alex): Must be careful when it comes to `EOF`, incorrect handling appears
                // as the `read` call being repeated.
                if read_amount == 0 {
                    libc::EOF.try_into().unwrap()
                } else {
                    read_amount.try_into().unwrap()
                }
            }
            Err(fail) => fail,
        }
    } else {
        debug!("read_detour -> fd {fd:#?}");
        let read_count = libc::read(fd, buf, count);
        read_count
    }
}

unsafe extern "C" fn lseek_detour(fd: RawFd, offset: off_t, whence: c_int) -> off_t {
    if let Some(managed_fd) = OPEN_FILES.lock().unwrap().get(&fd) {
        debug!(
            "lseek_detour -> managed_fd {managed_fd:#?} | offset {offset:#?} | whence {whence:#?}"
        );

        let seek_from = match whence {
            libc::SEEK_SET => SeekFrom::Start(offset as u64),
            libc::SEEK_CUR => SeekFrom::Current(offset),
            libc::SEEK_END => SeekFrom::End(offset),
            libc::SEEK_DATA => todo!(),
            libc::SEEK_HOLE => todo!(),
            other => {
                error!("lseek_detour -> invalid value for whence {whence:#?}");
                return libc::EINVAL.into();
            }
        };

        match lseek(*managed_fd, seek_from).map_err(|fail| {
            error!("Failed lseek operation with {fail:#?}");
            libc::EFAULT
        }) {
            Ok(result_offset) => result_offset as off_t,
            Err(fail) => fail.into(),
        }
    } else {
        let result_offset = libc::lseek(fd, offset, whence);
        result_offset
    }
}

unsafe extern "C" fn write_detour(fd: RawFd, buf: *const c_void, count: size_t) -> ssize_t {
    if let Some(remote_fd) = OPEN_FILES.lock().unwrap().get(&fd) {
        debug!("write_detour -> managed_fd {remote_fd:#?} | count {count:#?}");

        let write_bytes: Vec<u8> = Vec::from_raw_parts(buf as *mut _, count, count);

        match write(*remote_fd, write_bytes).map_err(|fail| {
            error!("Failed reading file with {fail:#?}");
            -1 as isize
        }) {
            Ok(written_amount) => {
                if written_amount == -1 {
                    libc::EOF.try_into().unwrap()
                } else {
                    written_amount.try_into().unwrap()
                }
            }
            Err(fail) => fail,
        }
    } else {
        let written_count = libc::write(fd, buf, count);
        written_count.try_into().unwrap()
    }
}

pub(super) fn enable_file_hooks(interceptor: &mut Interceptor) {
    interceptor
        .replace(
            Module::find_export_by_name(None, "open").unwrap(),
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

    interceptor
        .replace(
            Module::find_export_by_name(None, "lseek").unwrap(),
            NativePointer(lseek_detour as *mut c_void),
            NativePointer(std::ptr::null_mut()),
        )
        .unwrap();

    interceptor
        .replace(
            Module::find_export_by_name(None, "write").unwrap(),
            NativePointer(write_detour as *mut c_void),
            NativePointer(std::ptr::null_mut()),
        )
        .unwrap();
}
