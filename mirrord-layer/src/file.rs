use std::{collections::HashMap, env, lazy::SyncLazy, os::unix::io::RawFd, sync::Mutex};

use regex::RegexSet;

pub(crate) mod hooks;
pub(crate) mod ops;

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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct RemoteFile {
    fd: RawFd,
}

static OPEN_FILES: SyncLazy<Mutex<HashMap<LocalFd, RemoteFd>>> =
    SyncLazy::new(|| Mutex::new(HashMap::with_capacity(4)));

#[derive(Debug, Clone)]
pub(crate) struct ReadFile {
    bytes: Vec<u8>,
    read_amount: usize,
}
