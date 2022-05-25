use std::{ffi::CString, io::SeekFrom, os::unix::io::RawFd, path::PathBuf};

use libc::{c_int, O_ACCMODE, O_CREAT, O_RDONLY, O_RDWR, O_WRONLY, S_IRUSR, S_IWUSR, S_IXUSR};
use mirrord_protocol::{
    OpenFileResponse, OpenOptionsInternal, ReadFileResponse, SeekFileResponse, WriteFileResponse,
};
use tokio::sync::oneshot;
use tracing::debug;

use super::ReadFile;
use crate::{
    common::{HookMessage, LayerError, OpenFileHook, ReadFileHook, SeekFileHook, WriteFileHook},
    file::OPEN_FILES,
    HOOK_SENDER,
};

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
