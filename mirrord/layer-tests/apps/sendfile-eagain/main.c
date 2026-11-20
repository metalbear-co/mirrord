#include <assert.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/uio.h>
#include <unistd.h>

#ifdef __linux__
#include <sys/sendfile.h>
#endif

/// `sendfile` on a non-blocking socket with a full buffer should give `EAGAIN`
/// and report that nothing was sent.
///
/// On macOS that report comes via the `off_t *len` argument, which callers read
/// even when `sendfile` returns -1; a stale `len` causes callers to skip bytes
/// that weren't written.
static const char *SOURCE_PATH = "/tmp/sendfile_eagain_source";

int main()
{
  char chunk[4096];
  memset(chunk, 'x', sizeof(chunk));

  int source = open(SOURCE_PATH, O_CREAT | O_RDWR | O_TRUNC, 0600);
  assert(source >= 0);
  assert(write(source, chunk, sizeof(chunk)) == sizeof(chunk));
  assert(unlink(SOURCE_PATH) == 0);

  int sockets[2];
  assert(socketpair(AF_UNIX, SOCK_STREAM, 0, sockets) == 0);
  assert(fcntl(sockets[0], F_SETFL, O_NONBLOCK) == 0);

  while (write(sockets[0], chunk, sizeof(chunk)) > 0)
  {
  }
  assert(errno == EAGAIN);

#ifdef __APPLE__
  off_t len = sizeof(chunk);
  int result = sendfile(source, sockets[0], 0, &len, NULL, 0);
  if (result != -1 || errno != EAGAIN || len != 0)
  {
    fprintf(stderr, "sendfile returned %d, errno %d, len %lld\n", result, errno, (long long)len);
  }
  assert(result == -1);
  assert(errno == EAGAIN);
  assert(len == 0);
#else
  off_t offset = 0;
  ssize_t result = sendfile(sockets[0], source, &offset, sizeof(chunk));
  if (result != -1 || errno != EAGAIN || offset != 0)
  {
    fprintf(stderr, "sendfile returned %zd, errno %d, offset %lld\n", result, errno, (long long)offset);
  }
  assert(result == -1);
  assert(errno == EAGAIN);
  assert(offset == 0);
#endif

  return 0;
}
