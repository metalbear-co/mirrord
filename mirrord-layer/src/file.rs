use std::{collections::HashMap, env, lazy::SyncLazy, os::unix::io::RawFd, sync::Mutex};

use libc::{c_int, O_ACCMODE, O_APPEND, O_CREAT, O_RDONLY, O_RDWR, O_TRUNC, O_WRONLY};
use mirrord_protocol::OpenOptionsInternal;
use regex::RegexSet;
use tracing::warn;

pub(crate) mod hooks;
pub(crate) mod ops;

/// Regex that ignores system files + files in the current working directory.
static IGNORE_FILES: SyncLazy<RegexSet> = SyncLazy::new(|| {
    // To handle the problem of injecting `open` and friends into project runners (like in a call to
    // `node app.js`, or `cargo run app`), we're ignoring files from the current working directory.
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
        r"^/home/iojs/.*",
        // TODO: `node` searches for this file in multiple directories, bypassing some of our
        // ignore regexes, maybe other "project runners" will do the same.
        r".*/package.json",
        &current_dir.to_string_lossy(),
    ])
    .unwrap();

    set
});

type LocalFd = RawFd;
type RemoteFd = RawFd;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RemoteFile {
    fd: RawFd,
}

pub(crate) static OPEN_FILES: SyncLazy<Mutex<HashMap<LocalFd, RemoteFd>>> =
    SyncLazy::new(|| Mutex::new(HashMap::with_capacity(4)));

pub(crate) trait OpenOptionsInternalExt {
    fn from_flags(flags: c_int) -> Self;
    fn from_mode(mode: String) -> Self;
}

impl OpenOptionsInternalExt for OpenOptionsInternal {
    fn from_flags(flags: c_int) -> Self {
        let open_options = OpenOptionsInternal {
            read: (flags & O_ACCMODE == O_RDONLY) || (flags & O_ACCMODE == O_RDWR),
            write: (flags & O_ACCMODE == O_WRONLY) || (flags & O_ACCMODE == O_RDWR),
            append: (flags & O_APPEND != 0),
            truncate: (flags & O_TRUNC != 0),
            create: (flags & O_CREAT != 0),
            create_new: false,
        };

        open_options
    }

    /// WARN: Using the wrong mode is undefined behavior, according to the C standard, we're not
    /// deviating from it.
    fn from_mode(mode: String) -> Self {
        mode.chars()
            .fold(OpenOptionsInternal::default(), |mut open_options, value| {
                match value {
                    'r' => open_options.read = true,
                    'w' => {
                        open_options.write = true;
                        open_options.create = true;
                        open_options.truncate = true;
                        // open_options.flags |= CREATION_FLAGS as i32;
                    }
                    'a' => {
                        open_options.append = true;
                        open_options.create = true;
                        // open_options.flags |= CREATION_FLAGS as i32;
                    }
                    '+' => {
                        open_options.read = true;
                        open_options.write = true;
                    }
                    'x' => {
                        open_options.create_new = true;
                    }
                    // Only has meaning for `fmemopen`.
                    'b' => {}
                    invalid => {
                        warn!("Invalid mode for fopen {:#?}", invalid);
                    }
                }

                open_options
            })
    }
}
