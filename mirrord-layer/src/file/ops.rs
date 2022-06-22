use std::{ffi::CString, io::SeekFrom, os::unix::io::RawFd, path::PathBuf};

use libc::{c_int, c_uint, FILE, O_CREAT, O_RDONLY, S_IRUSR, S_IWUSR, S_IXUSR};
use mirrord_protocol::{
    file::OpenOptionsInternal, CloseFileResponse, OpenFileResponse, ReadFileResponse,
    SeekFileResponse, WriteFileResponse,
};
use tokio::sync::oneshot;
use tracing::{debug, error};

use crate::{
    blocking_send_hook_message,
    error::LayerError,
    file::{OpenOptionsInternalExt, OPEN_FILES},
    message::{
        CloseFileHook, HookMessage, OpenFileHook, OpenRelativeFileHook, ReadFileHook, SeekFileHook,
        WriteFileHook,
    },
};

/// Blocking wrapper around `libc::open` call.
///
/// **Bypassed** when trying to load system files, and files from the current working directory
/// (which is different anyways when running in `-agent` context).
///
/// When called for a valid file, it blocks and sends an open file request to be handled by
/// `mirrord-agent`, and waits until it receives an open file response.
///
/// `open` is also used by other _open-ish_ functions, and it takes care of **creating** the _local_
/// and _remote_ file association, plus **inserting** it into the storage for `OPEN_FILES`.
pub(super) fn open(path: PathBuf, open_options: OpenOptionsInternal) -> Result<RawFd, LayerError> {
    let (file_channel_tx, file_channel_rx) = oneshot::channel::<OpenFileResponse>();

    let requesting_file = OpenFileHook {
        path,
        open_options,
        file_channel_tx,
    };

    blocking_send_hook_message(HookMessage::OpenFileHook(requesting_file))?;

    let OpenFileResponse { fd: remote_fd } = file_channel_rx.blocking_recv()?;

    // TODO: Need a way to say "open a directory", right now `is_dir` always returns false.
    // This requires having a fake directory name (`/fake`, for example), instead of just converting
    // the fd to a string.
    let fake_local_file_name = CString::new(remote_fd.to_string())?;

    // The pair `shm_open`, `shm_unlink` are used to create a temporary file
    // (in `/dev/shm/`), and then remove it, as we only care about the `fd`. This is done to
    // preserve `open_flags`, as `memfd_create` will always return a `File` with read and write
    // permissions.
    let local_file_fd = unsafe {
        // `mode` is access rights: user, root, ...
        let local_file_fd = libc::shm_open(
            fake_local_file_name.as_ptr(),
            O_RDONLY | O_CREAT,
            (S_IRUSR | S_IWUSR | S_IXUSR) as c_uint,
        );

        libc::shm_unlink(fake_local_file_name.as_ptr());

        local_file_fd
    };

    // Close the remote file if the call to `libc::shm_open` failed and we have an invalid local fd.
    if local_file_fd == -1 {
        let _ = close_remote_file_on_failure(remote_fd)?;
    }

    OPEN_FILES.lock().unwrap().insert(local_file_fd, remote_fd);

    Ok(local_file_fd)
}

fn close_remote_file_on_failure(fd: usize) -> Result<CloseFileResponse, LayerError> {
    // Close the remote file if the call to `libc::shm_open` failed and we have an invalid local fd.
    error!("Call to `libc::shm_open` resulted in an error, closing the file remotely!");

    let (file_channel_tx, file_channel_rx) = oneshot::channel::<CloseFileResponse>();

    blocking_send_hook_message(HookMessage::CloseFileHook(CloseFileHook {
        fd,
        file_channel_tx,
    }))?;

    file_channel_rx.blocking_recv().map_err(From::from)
}

pub(super) fn openat(
    path: PathBuf,
    open_flags: c_int,
    relative_fd: usize,
) -> Result<RawFd, LayerError> {
    debug!(
        "openat -> trying to open valid file {:?} with relative dir {:?}.",
        path, relative_fd
    );
    let (file_channel_tx, file_channel_rx) = oneshot::channel::<OpenFileResponse>();

    let open_options = OpenOptionsInternalExt::from_flags(open_flags);

    let requesting_file = OpenRelativeFileHook {
        relative_fd,
        path,
        open_options,
        file_channel_tx,
    };

    blocking_send_hook_message(HookMessage::OpenRelativeFileHook(requesting_file))?;

    let OpenFileResponse { fd: remote_fd } = file_channel_rx.blocking_recv()?;
    let fake_file_name = CString::new(remote_fd.to_string())?;

    // The pair `shm_open`, `shm_unlink` are used to create a temporary file
    // (in `/dev/shm/`), and then remove it, as we only care about the `fd`. This is done to
    // preserve `open_flags`, as `memfd_create` will always return a `File` with read and write
    // permissions.
    let local_file_fd = unsafe {
        // `mode` is access rights: user, root.
        let local_file_fd = libc::shm_open(
            fake_file_name.as_ptr(),
            O_RDONLY | O_CREAT,
            (S_IRUSR | S_IWUSR | S_IXUSR) as c_uint,
        );

        libc::shm_unlink(fake_file_name.as_ptr());

        local_file_fd
    };

    // Close the remote file if the call to `libc::shm_open` failed and we have an invalid local fd.
    if local_file_fd == -1 {
        let _ = close_remote_file_on_failure(remote_fd)?;
    }

    debug!(
        "openat -> local_fd {local_file_fd:#?} | remote_fd {:#?}",
        remote_fd
    );

    OPEN_FILES.lock().unwrap().insert(local_file_fd, remote_fd);

    Ok(local_file_fd)
}

/// Calls `open` and returns a `FILE` pointer based on the **local** `fd`.
pub(super) fn fopen(
    path: PathBuf,
    open_options: OpenOptionsInternal,
) -> Result<*mut FILE, LayerError> {
    debug!(
        "fopen -> trying to fopen valid file {:?} with options {:#?}",
        path, open_options
    );

    let local_file_fd = open(path, open_options)?;
    let open_files = OPEN_FILES.lock().unwrap();

    open_files
        .get_key_value(&local_file_fd)
        .ok_or(LayerError::LocalFDNotFound(local_file_fd))
        // Convert the fd into a `*FILE`, this is be ok as long as `OPEN_FILES` holds the fd.
        .map(|(local_fd, _)| local_fd as *const _ as *mut _)
}

pub(super) fn fdopen(
    local_fd: &RawFd,
    remote_fd: usize,
    _open_options: OpenOptionsInternal,
) -> Result<*mut FILE, LayerError> {
    debug!("fdopen -> trying to fdopen valid file {:#?}", remote_fd);

    // TODO: Check that the constraint: remote file must have the same mode stuff that is passed
    // here.
    Ok(local_fd as *const _ as *mut _)
}

/// Blocking wrapper around `libc::read` call.
///
/// **Bypassed** when trying to load system files, and files from the current working directory, see
/// `open`.
pub(super) fn read(fd: usize, read_amount: usize) -> Result<ReadFileResponse, LayerError> {
    debug!("read -> trying to read valid file {:?}.", fd);

    let (file_channel_tx, file_channel_rx) = oneshot::channel::<ReadFileResponse>();

    let reading_file = ReadFileHook {
        fd,
        buffer_size: read_amount,
        file_channel_tx,
    };

    blocking_send_hook_message(HookMessage::ReadFileHook(reading_file))?;

    let read_file_response = file_channel_rx.blocking_recv()?;
    Ok(read_file_response)
}

pub(super) fn lseek(fd: usize, seek_from: SeekFrom) -> Result<u64, LayerError> {
    debug!("lseek -> trying to seek valid file {:?}.", fd);
    let (file_channel_tx, file_channel_rx) = oneshot::channel::<SeekFileResponse>();

    let seeking_file = SeekFileHook {
        fd,
        seek_from,
        file_channel_tx,
    };

    blocking_send_hook_message(HookMessage::SeekFileHook(seeking_file))?;

    let SeekFileResponse { result_offset } = file_channel_rx.blocking_recv()?;
    Ok(result_offset)
}

pub(super) fn write(fd: usize, write_bytes: Vec<u8>) -> Result<isize, LayerError> {
    debug!("write -> trying to write valid file {:?}.", fd);
    let (file_channel_tx, file_channel_rx) = oneshot::channel::<WriteFileResponse>();

    let writing_file = WriteFileHook {
        fd,
        write_bytes,
        file_channel_tx,
    };

    blocking_send_hook_message(HookMessage::WriteFileHook(writing_file))?;

    let WriteFileResponse { written_amount } = file_channel_rx.blocking_recv()?;
    Ok(written_amount.try_into()?)
}

pub(crate) fn close(fd: usize) -> Result<c_int, LayerError> {
    debug!("close -> trying to close valid file {:?}.", fd);
    let (file_channel_tx, file_channel_rx) = oneshot::channel::<CloseFileResponse>();

    let closing_file = CloseFileHook {
        fd,
        file_channel_tx,
    };

    blocking_send_hook_message(HookMessage::CloseFileHook(closing_file))?;

    file_channel_rx.blocking_recv()?;
    Ok(0)
}
