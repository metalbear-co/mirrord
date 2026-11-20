"""
Validate Python's socketpair on Windows under mirrord.
"""
import socket


def main():
    a, b = socket.socketpair()
    try:
        payload = b"ping"
        a.sendall(payload)
        data = b.recv(len(payload))
        assert data == payload
    finally:
        a.close()
        b.close()


if __name__ == "__main__":
    main()
