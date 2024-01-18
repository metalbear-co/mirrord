#include <stdio.h>
#include <fcntl.h>
#include <sys/uio.h>
#include <unistd.h>
#include <string.h>
#include <signal.h>


/// Opens a test file, then tries to read (part) of it with `readv`.
int main(int argc, char *argv[]) {
  printf("test issue 2178: START");

  char first[4], second[8];
  
  struct iovec iov[2];

  int fd = open("/app/test.txt", O_RDONLY);
  if (fd == -1) {
    printf("test issue 2178: FAILED");
    perror("open");
    return 1;
  }

  memset(second, 0, 8);

  iov[0].iov_base = first;
  iov[0].iov_len = sizeof(first);
  iov[1].iov_base = second;
  iov[1].iov_len = sizeof(second);

  ssize_t result = readv(fd, iov, 2);
  if (result == -1) {
    printf("test issue 2178: FAILED");
    perror("readv");
    return 1;
  }


  if (memcmp(first, "abcd", 4) != 0) {
    printf("test issue 2178: FAILED");
    printf("first: %s", first);
    return 1;
  }

  if (memcmp(second, "efgh\x00\x00\x00\x00", 8) != 0) {
    printf("test issue 2178: FAILED");
    printf("second: %s", second);
    return 1;
  }

  ssize_t null_result = readv(fd, NULL, 2);
  if (null_result == -1) {
    printf("test issue 2178: expected `readv: Bad address`");
  }

  close(fd);

  printf("test issue 2178: SUCCESS");
  return 0;
}
