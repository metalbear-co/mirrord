use std::io::SeekFrom;

use bincode::{Decode, Encode};

// TODO: We're not handling `custom_flags` here, if we ever need to do so, add them here (it's an OS
// specific thing).
#[derive(Encode, Decode, Debug, PartialEq, Clone, Copy, Eq, Default)]
pub struct OpenOptionsInternal {
    pub read: bool,
    pub write: bool,
    pub append: bool,
    pub truncate: bool,
    pub create: bool,
    pub create_new: bool,
}

impl From<OpenOptionsInternal> for std::fs::OpenOptions {
    fn from(internal: OpenOptionsInternal) -> Self {
        let OpenOptionsInternal {
            read,
            write,
            append,
            truncate,
            create,
            create_new,
        } = internal;

        std::fs::OpenOptions::new()
            .read(read)
            .write(write)
            .append(append)
            .truncate(truncate)
            .create(create)
            .create_new(create_new)
            .to_owned()
    }
}

/// Alternative to `std::io::SeekFrom`, used to implement `bincode::Encode` and `bincode::Decode`.
#[derive(Encode, Decode, Debug, PartialEq, Clone, Copy, Eq)]
pub enum SeekFromInternal {
    Start(u64),
    End(i64),
    Current(i64),
}

impl const From<SeekFromInternal> for SeekFrom {
    fn from(seek_from: SeekFromInternal) -> Self {
        match seek_from {
            SeekFromInternal::Start(start) => SeekFrom::Start(start),
            SeekFromInternal::End(end) => SeekFrom::End(end),
            SeekFromInternal::Current(current) => SeekFrom::Current(current),
        }
    }
}

impl const From<SeekFrom> for SeekFromInternal {
    fn from(seek_from: SeekFrom) -> Self {
        match seek_from {
            SeekFrom::Start(start) => SeekFromInternal::Start(start),
            SeekFrom::End(end) => SeekFromInternal::End(end),
            SeekFrom::Current(current) => SeekFromInternal::Current(current),
        }
    }
}
