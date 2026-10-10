pub mod filter;
pub mod mapper;
pub mod pattern_path;
// Serving a prefetched copy is done by the file hooks, which are unix only.
#[cfg(unix)]
pub mod prefetched;
#[cfg(unix)]
pub mod unix;
#[cfg(windows)]
pub mod windows;
