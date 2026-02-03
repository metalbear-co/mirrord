#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <fcntl.h>
#include <errno.h>

int parse_address(const char *s, struct sockaddr_in *a) {
	int port = atoi(s);
	if (port <= 0 || port > 65535) return -1;
	memset(a, 0, sizeof(*a));
	a->sin_family = AF_INET;
	a->sin_port = htons(port);
	return inet_pton(AF_INET, "127.0.0.1", &a->sin_addr) == 1 ? 0 : -1;
}

int create_connection(struct sockaddr_in *a) {
	int s = socket(AF_INET, SOCK_STREAM, 0);
	if (s < 0) return -1;
	if (connect(s, (struct sockaddr *)a, sizeof(*a)) < 0) {
		close(s);
		return -1;
	}
	return s;
}

int ping(int fd) {
	char buf[64];
	if (send(fd, "PING", 4, 0) < 0) return -1;
	ssize_t n = recv(fd, buf, sizeof(buf) - 1, 0);
	if (n < 0) return -1;
	buf[n] = '\0';
	return strcmp(buf, "PONG") == 0 ? 0 : -1;
}

int main(int argc, char *argv[]) {
	if (!getenv("AFTER_EXEC")) {
		const char *addr_str = getenv("TEST_ECHO_SERVER_PORT");
		if (!addr_str) return 1;

		struct sockaddr_in addr;
		if (parse_address(addr_str, &addr) < 0) return 1;

		int fd1 = create_connection(&addr);
		int fd2 = create_connection(&addr);
		if (fd1 < 0 || fd2 < 0) return 1;

		int flags = fcntl(fd1, F_GETFD);
		if (flags < 0 || fcntl(fd1, F_SETFD, flags | FD_CLOEXEC) < 0) return 1;

		if (ping(fd1) < 0 || ping(fd2) < 0) return 1;

		pid_t pid = fork();
		if (pid < 0) return 1;

		if (pid == 0) {
			// Give the parent some time to do its cleanup
			sleep(1);
			if (ping(fd1) < 0 || ping(fd2) < 0) exit(1);

			char fd1_str[16], fd2_str[16];
			snprintf(fd1_str, sizeof(fd1_str), "%d", fd1);
			snprintf(fd2_str, sizeof(fd2_str), "%d", fd2);
			setenv("AFTER_EXEC", "1", 1);
			setenv("FD1", fd1_str, 1);
			setenv("FD2", fd2_str, 1);

			execv(argv[0], argv);
			exit(1);
		}
		close(fd1);
		close(fd2);
		int status;
		waitpid(pid, &status, 0);
		return WIFEXITED(status) && WEXITSTATUS(status) == 0 ? 0 : 1;
	} else {
		int fd1 = atoi(getenv("FD1"));
		int fd2 = atoi(getenv("FD2"));

		if (fcntl(fd1, F_GETFD) >= 0 || errno != EBADF) return 1;
		if (fcntl(fd2, F_GETFD) < 0) return 1;
		if (ping(fd2) < 0) return 1;

		close(fd2);
		return 0;
	}
}
