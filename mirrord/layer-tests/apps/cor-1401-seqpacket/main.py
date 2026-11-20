import socket
import sys

MESSAGE = b"Julian Leopold Ochorowicz"


def main():
    path = sys.argv[1]
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    try:
        sock.connect(path)

        sent = sock.send(MESSAGE)
        assert sent == len(MESSAGE), f"partial send: {sent}"

        response = sock.recv(len(MESSAGE))
        assert response == MESSAGE, f"unexpected response: {response!r}"
    finally:
        sock.close()


if __name__ == "__main__":
    main()
