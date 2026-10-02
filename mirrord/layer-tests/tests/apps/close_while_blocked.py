"""Does an action on the main thread while another thread waits in the middle of a close.

Used by `close_while_blocked.rs`. The test stops reading the intproxy connection of the layer, so
the large remote write below fills the socket buffer and blocks while it holds that connection.
The close in the second thread then waits in the middle of its request to the intproxy.

The main thread must not close an fd during the action, because closes wait for each other.

Usage: close_while_blocked.py <action> <target>

- action: `fork` or `spawn`.
- target: what the second thread closes. `dir` is a remote directory, `socket` is a listening
  socket.
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
    listener.bind(("0.0.0.0", 41234))
    listener.listen()

    def close_target():
        listener.close()


blocker = os.open(f"{REMOTE_DIR}/blocker", os.O_WRONLY)


def close_in_thread():
    close_target()
    print("close done", flush=True)


# Larger than the socket buffers of the connection, so the write blocks until the test reads.
threading.Thread(target=lambda: os.write(blocker, b"x" * (32 << 20))).start()
time.sleep(1)
threading.Thread(target=close_in_thread).start()
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
print(f"{action} done", flush=True)
