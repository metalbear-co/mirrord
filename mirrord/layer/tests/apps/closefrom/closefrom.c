#ifndef __APPLE__
#include <stdlib.h>
#include <unistd.h>
#include <fcntl.h>
#include <string.h>
#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <sys/wait.h>

int create_socket(int port) {
    int sock_fd;
    struct sockaddr_in addr;

    sock_fd = socket(AF_INET, SOCK_STREAM, 0);
    if (sock_fd < 0) {
        return -1;
    }

    memset(&addr, 0, sizeof(addr));
    addr.sin_family = AF_INET;
    addr.sin_addr.s_addr = INADDR_ANY;
    addr.sin_port = htons(port);

    if (bind(sock_fd, (struct sockaddr *)&addr, sizeof(addr)) < 0) {
        close(sock_fd);
        return -1;
    }

    if (listen(sock_fd, 5) < 0) {
        close(sock_fd);
        return -1;
    }

    return sock_fd;
}

int main() {
    int sock1, sock2;
    pid_t pid;

	int port = 40000;

    /* Create 2 sockets in parent */
    sock1 = create_socket(port++);
    sock2 = create_socket(port++);

    if (sock1 < 0 || sock2 < 0) {
        return 1;
    }

    /* Fork */
    pid = fork();

    if (pid < 0) {
        return 1;
    }

    if (pid == 0) {
        /* Child process */
        int sock3, sock4;

        /* Create 2 more sockets in child */
        sock3 = create_socket(port++);
        sock4 = create_socket(port++);

        if (sock3 < 0 || sock4 < 0) {
            exit(1);
        }

        closefrom(3);
        exit(0);
    } else {
        /* Parent process */
        int status;

        /* Wait for child */
        waitpid(pid, &status, 0);
        closefrom(3);

        return WIFEXITED(status) ? WEXITSTATUS(status) : 1;
    }
}
#else
// mac doesn't support this
int main(){
	return 0;
}
#endif
