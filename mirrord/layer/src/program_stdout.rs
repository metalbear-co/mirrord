//! The program's stdout, for the only lines the layer writes there.

use std::io;

/// Writes to the program's stdout, fd 1: only the lines `mirrord up` reads from it, `Ready!` and
/// the debug hold's `debug-hold …`. Nothing else the layer says goes to stdout.
///
/// The layer runs inside the program, from its constructor, before `main`. Each write is one
/// `write(2)` straight to fd 1, so a line can't sit in a buffer (Rust's or the program's libc
/// stdio) or interleave with what those buffers flush later, and it reaches `up` at once, even
/// when the program then blocks in the debug hold. These lines are far shorter than `PIPE_BUF`,
/// so each one reaches a pipe whole.
///
/// Not `std::io::stdout`: in this `cdylib` it is the layer's own copy of Rust's, a line buffer
/// that the program's exit never flushes. `layer-lib/clippy.toml` also bans it from code shared
/// with the Windows layer, where its reentrant lock can abort the process (see
/// `layer-lib::logging`).
///
/// Write lines with `write_all`, which retries short and interrupted (`EINTR`) writes.
pub(crate) struct ProgramStdout;

impl io::Write for ProgramStdout {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // SAFETY: `buf` is valid for `buf.len()` bytes, and `write` reports a closed descriptor as
        // an error.
        let written = unsafe { libc::write(libc::STDOUT_FILENO, buf.as_ptr().cast(), buf.len()) };
        usize::try_from(written).map_err(|_| io::Error::last_os_error())
    }

    /// Nothing to flush: every write has already reached fd 1.
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
