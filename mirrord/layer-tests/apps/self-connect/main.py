"""
Test the use case of listening and connecting to the same port,
making sure that we don't go through the layer for this.
"""
import socket

TEST_DATA = b"test"
LISTEN_ADDR = ("127.0.0.1", 1337)

def main():
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listen:
        listen.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listen.bind(LISTEN_ADDR)
        listen.listen(1)
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as connect:
            connect.connect(LISTEN_ADDR)
            connect.send(TEST_DATA)
            server_socket, _ = listen.accept()
            with server_socket:
                data = server_socket.recv(4)
                assert data == TEST_DATA


if __name__ == '__main__':
    main()