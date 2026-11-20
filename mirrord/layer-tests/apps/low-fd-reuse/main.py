"""Takes over fds 3 to 9 without asking for free numbers, then opens a remote file.

Used by `low_fd_reuse.rs`. Shell scripts do the same with `exec 3>file` and `exec 3<&-`. The
layer's connection to the internal proxy must survive this, so the remote `open` still works.
"""

import os

# The test makes the paths in this directory remote.
REMOTE_FILE = "/mirrord-test/file"

devnull = os.open("/dev/null", os.O_RDWR)
for fd in range(3, 10):
    if fd != devnull:
        os.dup2(devnull, fd)
for fd in range(3, 10):
    os.close(fd)
if devnull >= 10:
    os.close(devnull)

remote = os.open(REMOTE_FILE, os.O_RDONLY)
os.close(remote)
print("remote open done")
