#include <fcntl.h>

int main() {
    int fd = open("/etc/resolv.conf", O_RDONLY);
    return fd >= 0 ? 0 : 1;
}
