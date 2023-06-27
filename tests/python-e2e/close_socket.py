"""
This is an application that listens to a socket and accepts 1 connection, and closes the socket once that connection is
closed.
"""
import socket
import fileinput

from sys import stderr

LISTEN_ADDR = ("127.0.0.1", 80)


def main():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listen:
        listen.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listen.bind(LISTEN_ADDR)
        listen.listen(1)

    # Here we are outside of the context above, so the socket is closed.
    # We continue running after closing the socket.

    print("Closed socket.", file=stderr)

    # The (Rust) test code informs this application via stdin when it can exit
    fileinput.input().readline()


if __name__ == '__main__':
    main()
