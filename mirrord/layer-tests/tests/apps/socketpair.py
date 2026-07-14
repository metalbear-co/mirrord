"""
Validate Python's socketpair on Windows under mirrord.
"""
import socket


def main():
    print("TEST - SOCKETPAIR START")
    a, b = socket.socketpair()
    try:
        payload = b"ping"
        a.sendall(payload)
        data = b.recv(len(payload))
        assert data == payload
    finally:
        a.close()
        b.close()
    print("TEST - SOCKETPAIR SUCCESS")


if __name__ == "__main__":
    main()
