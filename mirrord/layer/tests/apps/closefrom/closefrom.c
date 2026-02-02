#include <arpa/inet.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

int is_fd_open(int fd) {
	return fcntl(fd, F_GETFD) != -1;
}

int main() {
	int fd1, fd2, fd3, fd4, fd5, sock_fd;
	struct sockaddr_in addr;
	int opt = 1;

	/* Open several file descriptors */
	fd1 = open("/a/file", O_RDONLY);
	fd2 = open("/some/other/file", O_RDONLY);
	fd3 = open("/yet/another_file", O_RDONLY);
	fd4 = open("/take/a/wild/guess", O_RDONLY);
	fd5 = open("/oh/wow", O_RDONLY);

	if (fd1 < 0 || fd2 < 0 || fd3 < 0 || fd4 < 0 || fd5 < 0) {
		return 1;
	}

	/* Create a TCP server socket */
	sock_fd = socket(AF_INET, SOCK_STREAM, 0);
	if (sock_fd < 0) {
		return 1;
	}

	/* Set socket options */
	setsockopt(sock_fd, SOL_SOCKET, SO_REUSEADDR, &opt, sizeof(opt));

	/* Bind to a random port */
	memset(&addr, 0, sizeof(addr));
	addr.sin_family = AF_INET;
	addr.sin_addr.s_addr = INADDR_ANY;
	addr.sin_port = htons(0);

	if (bind(sock_fd, (struct sockaddr *) &addr, sizeof(addr)) < 0) {
		return 1;
	}

	/* Start listening */
	if (listen(sock_fd, 5) < 0) {
		return 1;
	}

	/* Verify all are open before closefrom */
	int all_open_before = is_fd_open(fd1) && is_fd_open(fd2) &&
						  is_fd_open(fd3) && is_fd_open(fd4) &&
						  is_fd_open(fd5) && is_fd_open(sock_fd);

	/* Call closefrom to close all file descriptors */
	closefrom(0);

	/* Check which file descriptors are still open */
	int success = is_fd_open(fd1) && is_fd_open(fd2) && !is_fd_open(fd3) &&
				  !is_fd_open(fd4) && !is_fd_open(fd5) &&
				  !is_fd_open(sock_fd) && all_open_before;

	/* Clean up remaining file descriptors */
	if (is_fd_open(fd1))
		close(fd1);
	if (is_fd_open(fd2))
		close(fd2);

	return success ? 0 : 1;
}
