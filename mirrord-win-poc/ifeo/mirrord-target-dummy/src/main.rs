#![feature(io_error_more)]

use std::{ffi::OsStr, os::windows::ffi::OsStrExt, path::Path, pin::Pin, task::Poll};

use tokio::io::{Error, ErrorKind};
use winapi::{
    shared::{ntdef::HANDLE, winerror::ERROR_SUCCESS},
    um::{
        errhandlingapi::GetLastError,
        fileapi::{CreateFileW, OPEN_EXISTING, ReadFile},
        handleapi::INVALID_HANDLE_VALUE,
        winnt::{FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, GENERIC_READ},
    },
};

/// Structure holding all the necessary data
/// to do a poll "tick".
struct ReadFileApi {
    handle: HANDLE,
    bytes_read: usize,
    buf: Vec<u8>,
}

impl Future for ReadFileApi {
    type Output = Result<Vec<u8>, Error>;

    /// Handles every poll through `ReadFile`, maintains state being prepared for
    /// when the `Future` is ready.
    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        // If the file handle is invalid, return an `Error`.
        if self.handle.is_null() || self.handle == INVALID_HANDLE_VALUE {
            return Poll::Ready(Err(ErrorKind::InvalidFilename.into()));
        }

        // Declare how much shall be read during this `poll` call.
        const POLL_CHUNK_SIZE: u32 = 0x1000;
        const POLL_CHUNKS: u32 = 1;
        const POLL_READ: u32 = POLL_CHUNK_SIZE * POLL_CHUNKS;

        // If the buffer capacity is not large enough for a new read,
        // add enough space for the following call, to be truncated later.
        let current_len = self.bytes_read;
        if self.buf.len() < (current_len + POLL_READ as usize) {
            self.buf.extend([0; POLL_READ as _]);
        }

        let mut bytes_read = 0u32;
        let buf_offset_ptr = unsafe { self.buf.as_mut_ptr().add(self.bytes_read) };
        let ret = unsafe {
            ReadFile(
                self.handle,
                buf_offset_ptr as _,
                POLL_READ,
                &mut bytes_read,
                std::ptr::null_mut(),
            )
        };

        // If the return value of `ReadFile` is non-zero, and the last
        // read was zero bytes, then the file has completed reading.
        if ret != 0 && bytes_read == 0 {
            let final_len = self.bytes_read;
            self.buf.truncate(final_len as _);
            return Poll::Ready(Ok(std::mem::take(&mut self.buf)));
        }

        self.bytes_read += bytes_read as usize;

        // Check if there's any `GetLastError` set. However, if it's
        // `ERROR_MORE_DATA`, then it just means we should read more.
        let error = unsafe { GetLastError() };
        if error != ERROR_SUCCESS {
            return Poll::Ready(Err(Error::from_raw_os_error(error as _)));
        }

        cx.waker().wake_by_ref();
        Poll::Pending
    }
}

/// Function that takes in a path, obtains a handle to file,
/// and returns a structure that implements returning a Future
/// of type `Result<Vec<u8>, Error>`.
fn async_read_file(path: impl AsRef<Path>) -> impl Future<Output = Result<Vec<u8>, Error>> {
    let path: Vec<u16> = OsStr::new(path.as_ref())
        .encode_wide()
        .chain(Some(0u16))
        .collect();
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            GENERIC_READ,
            FILE_SHARE_READ,
            std::ptr::null_mut(),
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            std::ptr::null_mut(),
        )
    };

    ReadFileApi {
        handle,
        bytes_read: 0,
        buf: Default::default(),
    }
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let file = Path::new(r#"C:\windows\system32\notepad.exe"#);
    let file = async_read_file(file).await?;
    let file = String::from_utf8_lossy(file.as_slice());
    println!("File succesfully read! {} bytes.", file.len());

    Ok(())
}
