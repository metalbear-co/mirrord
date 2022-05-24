use std::{
    self,
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{prelude::*, SeekFrom},
    os::unix::prelude::OpenOptionsExt,
    path::PathBuf,
};

use mirrord_protocol::{
    ErrorKindInternal, OpenFileResponse, OpenOptionsInternal, ReadFileResponse, SeekFileResponse,
    WriteFileResponse,
};
use tracing::debug;

#[derive(Debug, Default)]
pub(crate) struct FileManager {
    pub open_files: HashMap<i32, File>,
}

impl FileManager {
    // TODO(alex) [mid] 2022-05-19: Need to do a better job handling errors here (and in `read`)?
    // Ideally we would send the error back in the `Response`, to be handled by the mirrod-layer
    // side, and converted into some `libc::X` value.
    pub(crate) fn open(
        &mut self,
        path: PathBuf,
        open_options: OpenOptionsInternal,
    ) -> Result<OpenFileResponse, ErrorKindInternal> {
        debug!("FileManager::open -> Trying to open file {path:#?}");

        let OpenOptionsInternal { read, write, flags } = open_options;
        debug!("FileManager::open -> read {read:#?} | write {write:#?} | flags {flags:#?}");
        debug!(
            "FileManager::open -> file has O_CREATE flag {:#?}",
            flags & 0x40 > 0
        );

        let open_file_result = OpenOptions::new()
            .custom_flags(flags)
            .read(read)
            .write(write)
            .open(path)
            .map(|file| {
                let fd = std::os::unix::prelude::AsRawFd::as_raw_fd(&file);
                self.open_files.insert(fd, file);

                OpenFileResponse { fd }
            })
            .map_err(|fail| fail.kind().into());

        open_file_result
    }

    pub(crate) fn read(
        &mut self,
        fd: i32,
        buffer_size: usize,
    ) -> Result<ReadFileResponse, ErrorKindInternal> {
        let file = self.open_files.get_mut(&fd).unwrap();

        debug!("FileManager::read -> Trying to read file {file:#?}, with count {buffer_size:#?}");

        let mut buffer = vec![0; buffer_size];
        let read_file_result = file
            .read(&mut buffer)
            .map(|read_amount| {
                debug!("FileManager::read -> read {read_amount:#?} bytes from {fd:#?}");

                ReadFileResponse {
                    bytes: buffer,
                    is_eof: read_amount == 0,
                    read_amount,
                }
            })
            .map_err(|fail| fail.kind().into());

        read_file_result
    }

    pub(crate) fn seek(
        &mut self,
        fd: i32,
        seek_from: SeekFrom,
    ) -> Result<SeekFileResponse, ErrorKindInternal> {
        let file = self.open_files.get_mut(&fd).unwrap();

        debug!("FileManager::seek -> Trying to seek file {file:#?}, with seek {seek_from:#?}");

        let seek_file_result = file
            .seek(seek_from)
            .map(|result_offset| SeekFileResponse { result_offset })
            .map_err(|fail| fail.kind().into());

        seek_file_result
    }

    pub(crate) fn write(
        &mut self,
        fd: i32,
        write_bytes: Vec<u8>,
    ) -> Result<WriteFileResponse, ErrorKindInternal> {
        let file = self.open_files.get_mut(&fd).unwrap();

        debug!("FileManager::write -> Trying to write file {file:#?}");

        let write_file_result = file
            .write(&write_bytes)
            .map(|written_amount| WriteFileResponse { written_amount })
            .map_err(|fail| fail.kind().into());

        write_file_result
    }
}
