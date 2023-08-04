#include <fcntl.h>

int main() {
    int fd = open("/etc/hostname", O_RDONLY);
    return fd >= 0 ? 0 : 1;
}
