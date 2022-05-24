use std::{
    collections::HashMap,
    env,
    ffi::{CStr, CString},
    io::SeekFrom,
    lazy::SyncLazy,
    os::unix::io::RawFd,
    path::PathBuf,
    ptr, slice,
    sync::Mutex,
};

use frida_gum::{interceptor::Interceptor, Module, NativePointer};
use libc::{
    c_char, c_int, c_void, off_t, size_t, ssize_t, O_ACCMODE, O_CREAT, O_RDONLY, O_RDWR, O_WRONLY,
    S_IRUSR, S_IWUSR, S_IXUSR,
};
use mirrord_protocol::{
    OpenFileResponse, OpenOptionsInternal, ReadFileResponse, SeekFileResponse, WriteFileResponse,
};
use regex::RegexSet;
use tokio::sync::oneshot;
use tracing::{debug, error};

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
    is_eof: bool,
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
pub(crate) fn open(path: PathBuf, open_flags: c_int) -> Result<RawFd, LayerError> {
    debug!("open -> trying to open valid file {path:?}.");

    let sender = unsafe { HOOK_SENDER.as_ref().unwrap() };
    let (file_channel_tx, file_channel_rx) = oneshot::channel::<OpenFileResponse>();

    let open_options = OpenOptionsInternal {
        read: (open_flags & O_ACCMODE == O_RDONLY) || (open_flags & O_ACCMODE == O_RDWR),
        write: (open_flags & O_ACCMODE == O_WRONLY) || (open_flags & O_ACCMODE == O_RDWR),
        flags: open_flags,
    };

    let requesting_file = OpenFileHook {
        path,
        open_options,
        file_channel_tx,
    };

    sender.blocking_send(HookMessage::OpenFileHook(requesting_file))?;

    debug!("open -> await response from remote");
    let open_file = file_channel_rx.blocking_recv()?;

    let fake_file_name = CString::new(open_file.fd.to_string()).unwrap();

    // TODO(alex) [mid] 2022-05-24: Be very careful here, if a call to create the local fd fails,
    // the remote one stays up (basically a leak). So I need a way to `close` the remote on failure
    // here.
    //
    // NOTE(alex): The pair `shm_open`, `shm_unlink` are used to create a temporary file
    // (in `/dev/shm/`), and then remove it, as we only care about the `fd`. This is done to
    // preserve `open_flags`, as `memfd_create` will always return a `File` with read and write
    // permissions.
    let local_file_fd = unsafe {
        // NOTE(alex): `mode` is access rights: user, root.
        let local_file_fd = libc::shm_open(
            fake_file_name.as_ptr(),
            O_RDONLY | O_CREAT,
            S_IRUSR | S_IWUSR | S_IXUSR,
        );

        libc::shm_unlink(fake_file_name.as_ptr());

        local_file_fd
    };

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
///
/// If you call `read` and it returns `0` bytes read, it might be because the file cursor is at the
/// end, so a call to `seek` is required.
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
        is_eof: read_file_response.is_eof,
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
pub(super) unsafe extern "C" fn open_detour(raw_path: *const c_char, open_flags: c_int) -> RawFd {
    let path: PathBuf = CStr::from_ptr(raw_path)
        .to_str()
        .expect("Failed converting path from c_char!")
        .into();

    if IGNORE_FILES.is_match(path.to_str().unwrap()) {
        let bypassed_fd = libc::open(raw_path, open_flags);
        debug!("open_detour -> bypassed_fd {bypassed_fd:#?}");

        bypassed_fd
    } else {
        match open(path, open_flags).map_err(|fail| {
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
    let remote_fd = OPEN_FILES.lock().unwrap().get(&fd).cloned();
    if let Some(remote_fd) = remote_fd {
        debug!("read_detour -> managed_fd {remote_fd:#?} | fd {fd:#?} | count {count:#?}");

        match read(remote_fd, count).map_err(|fail| {
            error!("Failed reading file with {fail:#?}");
            -1
        }) {
            Ok(read_file) => {
                let ReadFile {
                    bytes,
                    read_amount,
                    is_eof,
                } = read_file;

                let read_ptr = bytes.as_ptr();
                let out_buffer = buf.cast();
                ptr::copy(read_ptr, out_buffer, read_amount);

                // WARN(alex): Must be careful when it comes to `EOF`, incorrect handling appears
                // as the `read` call being repeated.

                // TODO(alex) [high] 2022-05-24: Gotta properly handle `0` return now, as it can be
                // returned from -agent if we indeed read 0 bytes.
                /*
                does not handle blank line case (at least not in windows with edition 2018). you will need to check file size as well.
                 */
                if is_eof {
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
    let remote_fd = OPEN_FILES.lock().unwrap().get(&fd).cloned();
    if let Some(remote_fd) = remote_fd {
        debug!(
            "lseek_detour -> managed_fd {remote_fd:#?} | offset {offset:#?} | whence {whence:#?}"
        );

        let seek_from = match whence {
            libc::SEEK_SET => SeekFrom::Start(offset as u64),
            libc::SEEK_CUR => SeekFrom::Current(offset),
            libc::SEEK_END => SeekFrom::End(offset),
            libc::SEEK_DATA => todo!(),
            libc::SEEK_HOLE => todo!(),
            _other => {
                error!("lseek_detour -> invalid value for whence {whence:#?}");
                return libc::EINVAL.into();
            }
        };

        match lseek(remote_fd, seek_from).map_err(|fail| {
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
    let remote_fd = OPEN_FILES.lock().unwrap().get(&fd).cloned();
    if let Some(remote_fd) = remote_fd {
        debug!("write_detour -> managed_fd {remote_fd:#?} | count {count:#?}");

        if buf.is_null() {
            return libc::EFAULT.try_into().unwrap();
        }

        // WARN(alex): Be veeery careful here, you cannot construct the `Vec` directly, as the
        // buffer allocation is handled on the C side.
        let outside_buffer = slice::from_raw_parts(buf as *const u8, count);
        let write_bytes = outside_buffer.to_vec();

        match write(remote_fd, write_bytes).map_err(|fail| {
            error!("Failed writing file with {fail:#?}");
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
