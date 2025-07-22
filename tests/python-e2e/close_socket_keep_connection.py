"""
This is an application that listens to a socket and accepts 1 connection, and closes the socket once that connection is
closed.
"""
import socket
import fileinput

from sys import stderr

TEST_DATA = b"test"
LISTEN_ADDR = ("127.0.0.1", 80)


def main():

    initial_socket = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    initial_socket.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    initial_socket.bind(LISTEN_ADDR)
    initial_socket.listen(1)

    print("app listening.")

    connected_socket, _ = initial_socket.accept()
    print("app accepted connection.")

    initial_socket.close()
    print("Closed socket.", file=stderr)

    data = connected_socket.recv(16, socket.MSG_WAITALL)
    connected_socket.sendall(data.upper())

    connected_socket.close()

    # The (Rust) test code informs this application via stdin when it can exit
    fileinput.input().readline()


if __name__ == '__main__':
    main()
