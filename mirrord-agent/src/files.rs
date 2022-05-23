use std::{
    self,
    collections::HashMap,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::PathBuf,
};

use anyhow::Result;
use mirrord_protocol::{ReadFileResponse, SeekFileResponse, WriteFileResponse};
use tracing::debug;

#[derive(Debug, Default)]
pub(crate) struct FileManager {
    pub open_files: HashMap<i32, File>,
}

impl FileManager {
    // TODO(alex) [mid] 2022-05-19: Need to do a better job handling errors here (and in `read`)?
    // Ideally we would send the error back in the `Response`, to be handled by the mirrod-layer
    // side, and converted into some `libc::X` value.
    pub(crate) fn open(&mut self, path: PathBuf) -> Result<i32> {
        debug!("FileManager::open -> Trying to open file {path:#?}.");

        // TODO(alex) [low] 2022-05-18: To avoid opening the same file multiple times, we could
        // search `FileManager::open_files` for a path that matches the one passed as a parameter.
        let file = std::fs::File::open(path)?;
        let fd = std::os::unix::prelude::AsRawFd::as_raw_fd(&file);

        self.open_files.insert(fd, file);

        Ok(fd)
    }

    pub(crate) fn read(&self, fd: i32, buffer_size: usize) -> Result<ReadFileResponse> {
        let mut file = self.open_files.get(&fd).unwrap();

        debug!("FileManager::read -> Trying to read file {file:#?}, with count {buffer_size:#?}");

        let mut buffer = vec![0; buffer_size];
        let read_amount = file.read(&mut buffer)?;

        let response = ReadFileResponse {
            bytes: buffer,
            read_amount,
        };

        Ok(response)
    }

    pub(crate) fn seek(&self, fd: i32, seek_from: SeekFrom) -> Result<SeekFileResponse> {
        let mut file = self.open_files.get(&fd).unwrap();

        debug!("FileManager::seek -> Trying to seek file {file:#?}, with seek {seek_from:#?}");

        let result_offset = file.seek(seek_from)?;

        let response = SeekFileResponse { result_offset };
        Ok(response)
    }

    pub(crate) fn write(&self, fd: i32, write_bytes: Vec<u8>) -> Result<WriteFileResponse> {
        todo!()
    }
}
