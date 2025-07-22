# Listens then exits.
import socket


def main():
    sock = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    sock.bind(("0.0.0.0", 21232))
    sock.listen(1)
    sock.close()

if __name__ == "__main__":
    main()