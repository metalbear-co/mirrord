"""Does an action on the main thread while another thread waits in the middle of a close.

Used by `close_while_blocked.rs`. The test stops reading the intproxy connection of the layer, so
the large remote write below fills the socket buffer and blocks while it holds that connection.
The close in the second thread then waits in the middle of its request to the intproxy.

The main thread must not close a socket or a remote file during the action, because these closes
wait for each other. Closes of other fds do not wait.

Usage: close_while_blocked.py <action> <target>

- action: `fork`, `spawn`, `dup` or `close-local` (closes a pipe).
- target: what the second thread closes. `dir` is a remote directory, `socket` is a listening
  socket, `file` is a remote file, `replaced-file` is a remote file that the layer still has after
  `close_range`, and that an `open` replaces.
"""

import ctypes
import os
import socket
import sys
import threading
import time

# The test makes the paths in this directory remote.
REMOTE_DIR = "/mirrord-test"

libc = ctypes.CDLL(None)
libc.fdopendir.restype = ctypes.c_void_p
libc.closedir.argtypes = [ctypes.c_void_p]

action, target = sys.argv[1], sys.argv[2]

if target == "dir":
    # The directory stream uses a copy of `dir_fd`, so `closedir` only sends `CloseDirRequest`:
    # the remote file stays open through `dir_fd`.
    dir_fd = os.open(f"{REMOTE_DIR}/dir", os.O_RDONLY)
    directory = libc.fdopendir(os.dup(dir_fd))

    def close_target():
        libc.closedir(directory)

elif target == "socket":
    listener = socket.socket()
    listener.bind(("127.0.0.1", 41234))
    listener.listen()

    def close_target():
        listener.close()

elif target == "file":
    closed_file = os.open(f"{REMOTE_DIR}/closed", os.O_RDONLY)

    def close_target():
        os.close(closed_file)

elif target == "replaced-file":
    stale = os.open(f"{REMOTE_DIR}/stale", os.O_RDONLY)

    def close_target():
        # Gets the number of `stale`, so the layer replaces its stale entry, and drops the old
        # remote file, which sends its close request.
        if os.open(f"{REMOTE_DIR}/new", os.O_RDONLY) == stale:
            print("replaced", flush=True)


# A local fd, which the layer does not manage.
local_read, local_write = os.pipe()

blocker = os.open(f"{REMOTE_DIR}/blocker", os.O_WRONLY)

if target == "replaced-file":
    # The layer does not hook `close_range`, so `OPEN_FILES` keeps an entry for `stale`.
    libc.close_range(stale, stale, 0)


def close_in_thread():
    print("closing", flush=True)
    close_target()
    print("close done", flush=True)


# Larger than the socket buffers of the connection, so the write blocks until the test reads.
write_thread = threading.Thread(target=lambda: os.write(blocker, b"x" * (32 << 20)))
close_thread = threading.Thread(target=close_in_thread)

if target == "replaced-file":
    # The `open` sends its request before the write holds the connection. The test answers it
    # after "respond", so that its close request waits behind the write.
    close_thread.start()
    time.sleep(1)
    write_thread.start()
    time.sleep(1)
    print("respond", flush=True)
    time.sleep(1)
else:
    write_thread.start()
    time.sleep(1)
    close_thread.start()
    time.sleep(1)

print(f"{action} start", flush=True)
if action == "fork":
    pid = os.fork()
    if pid == 0:
        os._exit(0)
    os.waitpid(pid, 0)
elif action == "spawn":
    # An empty environment, so that the child does not load the layer.
    pid = os.posix_spawn(sys.executable, [sys.executable, "-c", ""], {})
    os.waitpid(pid, 0)
elif action == "dup":
    # Python calls `fcntl(F_DUPFD_CLOEXEC)`, which the layer handles like `dup`.
    os.dup(1)
elif action == "close-local":
    os.close(local_read)
print(f"{action} done", flush=True)
