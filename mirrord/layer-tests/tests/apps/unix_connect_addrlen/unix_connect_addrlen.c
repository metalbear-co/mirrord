// Calls `connect(2)` on a Unix socket with the same `sun_path` but different
// `addrlen` values: the exact length, a padded `sockaddr_un`, and PHP's
// `offsetof(sockaddr_un, sun_path) + strlen(path)` form that omits the NUL.
// The layer is expected to send the same complete pathname to the agent.

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

    socklen_t php_style =
        (socklen_t)(offsetof(struct sockaddr_un, sun_path) + strlen(SOCKET_PATH));
    try_connect(php_style, "php-style");

    return 0;
}
