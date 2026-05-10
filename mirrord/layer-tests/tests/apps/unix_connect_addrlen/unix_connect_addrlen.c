// Calls `connect(2)` on a unix socket twice with the same `sun_path` but
// different `addrlen` values: first the exact length, then `sizeof(struct
// sockaddr_un)` so `sun_path` carries trailing null bytes. The layer is
// expected to send the same trimmed pathname to the agent in both cases.

#include <stddef.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/un.h>
#include <unistd.h>

static const char SOCKET_PATH[] = "/tmp/mirrord_test_uds_addrlen.sock";

static void try_connect(socklen_t addrlen, const char *label) {
    int fd = socket(AF_UNIX, SOCK_STREAM, 0);
    if (fd < 0) {
        perror("socket");
        return;
    }

    struct sockaddr_un addr;
    memset(&addr, 0, sizeof(addr));
    addr.sun_family = AF_UNIX;
    strncpy(addr.sun_path, SOCKET_PATH, sizeof(addr.sun_path) - 1);

    int ret = connect(fd, (const struct sockaddr *)&addr, addrlen);
    printf("[%s] connect ret=%d\n", label, ret);

    close(fd);
}

int main(void) {
    socklen_t exact = (socklen_t)(offsetof(struct sockaddr_un, sun_path)
                                  + strlen(SOCKET_PATH) + 1);
    try_connect(exact, "exact");

    try_connect((socklen_t)sizeof(struct sockaddr_un), "padded");

    return 0;
}
