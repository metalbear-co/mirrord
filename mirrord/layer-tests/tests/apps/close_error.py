"""Opens a remote file and closes it twice, printing what each `close` returns.

Used by `close_error.rs`, which fails the first close remotely. The first close must report that
error, and the layer must forget the fd anyway, so the second close fails locally with `EBADF`.
"""

import errno
import os

# The test makes the paths in this directory remote.
REMOTE_FILE = "/mirrord-test/close-error"

fd = os.open(REMOTE_FILE, os.O_RDONLY)

for attempt in ("first", "second"):
    try:
        os.close(fd)
        print(f"{attempt} close succeeded")
    except OSError as error:
        print(f"{attempt} close failed: {errno.errorcode[error.errno]}")
