use std::{
    self,
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{prelude::*, SeekFrom},
    os::unix::prelude::OpenOptionsExt,
    path::PathBuf,
};

use anyhow::Result;
use mirrord_protocol::{
    OpenOptionsInternal, ReadFileResponse, SeekFileResponse, WriteFileResponse,
};
use tracing::{debug, error};

#[derive(Debug, Default)]
pub(crate) struct FileManager {
    pub open_files: HashMap<i32, File>,
}

impl FileManager {
    // TODO(alex) [mid] 2022-05-19: Need to do a better job handling errors here (and in `read`)?
    // Ideally we would send the error back in the `Response`, to be handled by the mirrod-layer
    // side, and converted into some `libc::X` value.
    //
    // TODO(alex) [high] 2022-05-24: Somehow the proper `OpenOptions` are not being used here, as
    // it doesn't create a file if it doesn't exist.
    /*
    [2022-05-24T04:21:38Z DEBUG mirrord_agent::files] FileManager::open -> Trying to open file "/tmp/meow.txt"
    [2022-05-24T04:21:38Z DEBUG mirrord_agent::files] FileManager::open -> read true | write true | flags 524354
    [2022-05-24T04:21:38Z ERROR mirrord_agent] Peer encountered error No such file or directory (os error 2)
        */
    pub(crate) fn open(&mut self, path: PathBuf, open_options: OpenOptionsInternal) -> Result<i32> {
        debug!("FileManager::open -> Trying to open file {path:#?}");

        let OpenOptionsInternal { read, write, flags } = open_options;
        debug!("FileManager::open -> read {read:#?} | write {write:#?} | flags {flags:#?}");

        let file = OpenOptions::new()
            .custom_flags(flags)
            .read(read)
            .write(write)
            .open(path)?;

        debug!("FileManager::open -> file {file:#?}");

        let fd = std::os::unix::prelude::AsRawFd::as_raw_fd(&file);

        self.open_files.insert(fd, file);

        Ok(fd)
    }

    pub(crate) fn read(&mut self, fd: i32, buffer_size: usize) -> Result<ReadFileResponse> {
        let file = self.open_files.get_mut(&fd).unwrap();

        debug!("FileManager::read -> Trying to read file {file:#?}, with count {buffer_size:#?}");

        let mut buffer = vec![0; buffer_size];
        let read_amount = file.read(&mut buffer)?;

        let response = ReadFileResponse {
            bytes: buffer,
            read_amount,
        };

        Ok(response)
    }

    pub(crate) fn seek(&mut self, fd: i32, seek_from: SeekFrom) -> Result<SeekFileResponse> {
        let file = self.open_files.get_mut(&fd).unwrap();

        debug!("FileManager::seek -> Trying to seek file {file:#?}, with seek {seek_from:#?}");

        let result_offset = file.seek(seek_from)?;

        let response = SeekFileResponse { result_offset };
        Ok(response)
    }

    pub(crate) fn write(&mut self, fd: i32, write_bytes: Vec<u8>) -> Result<WriteFileResponse> {
        let file = self.open_files.get_mut(&fd).unwrap();

        debug!("FileManager::write -> Trying to write file {file:#?}");

        let written_amount = file.write(&write_bytes)?;

        let response = WriteFileResponse { written_amount };
        Ok(response)
    }
}
