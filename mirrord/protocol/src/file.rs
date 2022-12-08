
use std::{
    path::PathBuf, fs::{Metadata},
    os::unix::prelude::{MetadataExt},
};

use bincode::{Decode, Encode};



#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct LstatRequest {
    pub path: PathBuf
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct LstatResponse {
    pub metadata: MetadataInternal
}

/// Internal version of Metadata across operating system (macOS, Linux)
/// Only mutual attributes
#[derive(Encode, Decode, Debug, PartialEq, Clone, Copy, Eq, Default)]

pub struct MetadataInternal {
    /// dev_id, st_dev
    device_id: u64,
    /// inode, st_ino
    inode: u64,
    /// file type, st_mode
    mode: u32,
    /// number of hard links, st_nlink
    hard_links: u64,
    /// user id, st_uid
    user_id: u32,
    /// group id, st_gid
    group_id: u32,
    /// rdevice id, st_rdev (special file)
    rdevice_id: u64,
    /// file size, st_size
    size: u64,
    /// time is in nano seconds, can be converted to seconds by dividing by 1e9
    /// access time, st_atime_ns
    access_time: i64,
    /// modification time, st_mtime_ns
    modification_time: i64,
    /// creation time, st_ctime_ns
    creation_time: i64,
    /// block size, st_blksize
    block_size: u64,
    /// number of blocks, st_blocks
    blocks: u64,
}


impl From<Metadata> for MetadataInternal {
    fn from(metadata: Metadata) -> Self {
        Self {
            device_id: metadata.dev(),
            inode: metadata.ino(),
            mode: metadata.mode(),
            hard_links: metadata.nlink(),
            user_id: metadata.uid(),
            group_id: metadata.gid(),
            rdevice_id: metadata.rdev(),
            size: metadata.size(),
            access_time: metadata.atime_nsec(),
            modification_time: metadata.mtime_nsec(),
            creation_time: metadata.ctime_nsec(),
            block_size: metadata.blksize(),
            blocks: metadata.blocks(),
        }
    }
}
