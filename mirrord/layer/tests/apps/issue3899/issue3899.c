#include <arpa/inet.h>
#include <errno.h>
#include <spawn.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <unistd.h>

extern char **environ;

int main(int argc, char **argv) {
    if (argc > 1 && strcmp(argv[1], "child") == 0) {
		printf("Running child process\n");
		sleep(1);
		return 0;
    }

    int sock = socket(AF_INET, SOCK_STREAM, 0);
    if (sock < 0) {
        perror("socket");
        exit(1);
    }

    struct sockaddr_in addr = {
        .sin_family = AF_INET,
        .sin_port = htons(8080),
    };
    inet_pton(AF_INET, "127.0.0.1", &addr.sin_addr);

    if (connect(sock, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        perror("connect");
        exit(1);
    }

    char *child_argv[] = {
        argv[0],
        "child",
        NULL
    };

	write(sock, "before spawn\n", 13);
    pid_t pid;
    if (posix_spawn(&pid, argv[0], NULL, NULL, child_argv, environ) != 0) {
        perror("posix_spawn");
        exit(1);
    }
	printf("Child pid: %d\n", pid);

	printf("Waitin for child processes to finish...\n");
	sleep(3);

	write(sock, "after spawn\n", 12);
	return 0;
}

