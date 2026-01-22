#ifdef __APPLE__

#include <sys/socket.h>
#include <netinet/in.h>
#include <arpa/inet.h>
#include <net/if.h>

#include <unistd.h>
#include <stdio.h>
#include <string.h>
#include <errno.h>

int connect_and_send(const sa_endpoints_t *eps, const char *label) {
    int fd = socket(AF_INET, SOCK_STREAM, 0);
    if (fd < 0) {
        perror("socket");
        return -1;
    }

    int ret = connectx(
        fd, // socket
        eps, // endpoints
        0, // associd
        0, // flags
        NULL, // iov
        0, // iovcnt
        NULL, // len
        NULL // connid
    );

    if (ret < 0) {
        perror("connectx");
        close(fd);
        return -1;
    }

    const char msg[] = "hello\n";
    ssize_t n = send(fd, msg, sizeof(msg) - 1, 0);
    if (n < 0) {
        perror("send");
        close(fd);
        return -1;
    }

    printf("[%s] sent %zd bytes\n", label, n);

    close(fd);

    return 0;
}

int main(void) {
    // Destination: 127.0.0.1:80
    struct sockaddr_in dst;
    memset(&dst, 0, sizeof(dst));
    dst.sin_family = AF_INET;
    dst.sin_port = htons(80);
    dst.sin_addr.s_addr = htonl(INADDR_LOOPBACK);
	
	// Source: 0.0.0.0:23333
    struct sockaddr_in src;
    memset(&src, 0, sizeof(src));
    src.sin_port = 23333;
	inet_pton(AF_INET, "0.0.0.0", &src.sin_addr);
	src.sin_family = AF_INET;
	src.sin_len    = sizeof(src);

    sa_endpoints_t eps3;
    memset(&eps3, 0, sizeof(eps3));
    eps3.sae_dstaddr = (const struct sockaddr *)&dst;
    eps3.sae_dstaddrlen = sizeof(dst);
    eps3.sae_srcaddr = (const struct sockaddr *)&src;
    eps3.sae_srcaddrlen = sizeof(src);

    connect_and_send(&eps3, "src+dst");

    return 0;
}

// Not macOS
#else

int main(void) {
    return 0;
}

#endif
