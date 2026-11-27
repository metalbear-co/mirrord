"""Connects to a socket shaped like the .NET diagnostic port `mirrord up` serves.

Listens on `<dir>/mirrord-debug-<uid>/dotnet-<pid>-<id>.sock`, connects to it as a paused .NET
runtime does, and checks the bytes arrive, so the connection stayed on this machine.
"""

import os
import shutil
import socket
import tempfile

TEST_DATA = b"ADVR_V1\0"


def main():
    root = tempfile.mkdtemp(dir="/tmp")
    try:
        directory = os.path.join(root, f"mirrord-debug-{os.getuid()}")
        os.mkdir(directory, 0o700)
        path = os.path.join(directory, f"dotnet-{os.getpid()}-layertest.sock")
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as listen:
            listen.bind(path)
            listen.listen(1)
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connect:
                connect.connect(path)
                connect.sendall(TEST_DATA)
                server_socket, _ = listen.accept()
                with server_socket:
                    data = server_socket.recv(len(TEST_DATA))
                    assert data == TEST_DATA, f"unexpected data: {data!r}"
    finally:
        shutil.rmtree(root, ignore_errors=True)
    print("connected locally", flush=True)


if __name__ == "__main__":
    main()
