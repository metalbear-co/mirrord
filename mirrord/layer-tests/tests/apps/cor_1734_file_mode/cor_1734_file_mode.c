#define _GNU_SOURCE
#define _LARGEFILE64_SOURCE

#include <assert.h>
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <unistd.h>

/// Every `open`-family hook must forward the variadic `mode` argument when it bypasses the call
/// and runs it locally. A hook that declares the libc function as non-variadic silently drops
/// `mode`, and the file ends up created with whatever garbage occupied the argument slot.
///
/// All paths here live under `/tmp`, which mirrord treats as local, so each call bypasses to libc
/// and the resulting permission bits must match exactly what was requested.

/// Several modes, because whatever garbage a broken hook passes on can happen to agree with any
/// single one of them. A dropped `mode` cannot agree with all of these at once.
static const mode_t MODES[] = {0640, 0604, 0431, 0755};
#define MODE_COUNT (sizeof(MODES) / sizeof(MODES[0]))

static const int CREATE_FLAGS = O_CREAT | O_WRONLY | O_TRUNC;

/// `O_CREAT` only applies `mode` when the file does not exist yet, so a leftover file from an
/// earlier run would mask the very bug this app checks for.
static void fresh_path(char *out, size_t len, const char *name, mode_t mode)
{
  snprintf(out, len, "/tmp/cor_1734_%s_%04o", name, mode);
  assert(unlink(out) == 0 || errno == ENOENT);
}

static void assert_created_with_mode(const char *path, mode_t expected, int fd)
{
  assert(fd >= 0);

  struct stat file_stat;
  assert(fstat(fd, &file_stat) == 0);
  assert(close(fd) == 0);

  mode_t actual = file_stat.st_mode & 07777;
  if (actual != expected)
  {
    fprintf(stderr, "%s created with mode %04o, expected %04o\n", path, actual, expected);
  }
  assert(actual == expected);

  assert(unlink(path) == 0);
}

int main()
{
  // So that the expected mode is not masked away.
  umask(0);

#ifdef __APPLE__
  // The `$NOCANCEL` variants have no declaration to call, but they are hooked just like the
  // plain ones, so resolve them at runtime to make sure they forward `mode` too.
  int (*open_nocancel)(const char *, int, ...) = dlsym(RTLD_DEFAULT, "open$NOCANCEL");
  assert(open_nocancel != NULL);
  int (*openat_nocancel)(int, const char *, int, ...) = dlsym(RTLD_DEFAULT, "openat$NOCANCEL");
  assert(openat_nocancel != NULL);
#endif

  for (size_t i = 0; i < MODE_COUNT; i++)
  {
    mode_t mode = MODES[i];
    char path[128];

    fresh_path(path, sizeof(path), "open", mode);
    assert_created_with_mode(path, mode, open(path, CREATE_FLAGS, mode));

    fresh_path(path, sizeof(path), "openat", mode);
    assert_created_with_mode(path, mode, openat(AT_FDCWD, path, CREATE_FLAGS, mode));

#ifdef __linux__
    fresh_path(path, sizeof(path), "open64", mode);
    assert_created_with_mode(path, mode, open64(path, CREATE_FLAGS, mode));

    fresh_path(path, sizeof(path), "openat64", mode);
    assert_created_with_mode(path, mode, openat64(AT_FDCWD, path, CREATE_FLAGS, mode));
#endif

#ifdef __APPLE__
    fresh_path(path, sizeof(path), "open_nocancel", mode);
    assert_created_with_mode(path, mode, open_nocancel(path, CREATE_FLAGS, mode));

    fresh_path(path, sizeof(path), "openat_nocancel", mode);
    assert_created_with_mode(path, mode, openat_nocancel(AT_FDCWD, path, CREATE_FLAGS, mode));
#endif

    snprintf(path, sizeof(path), "/tmp/cor_1734_mkdir_%04o", mode);
    assert(rmdir(path) == 0 || errno == ENOENT);
    assert(mkdir(path, mode) == 0);
    struct stat dir_stat;
    assert(stat(path, &dir_stat) == 0);
    if ((dir_stat.st_mode & 07777) != mode)
    {
      fprintf(stderr, "%s created with mode %04o, expected %04o\n", path,
              dir_stat.st_mode & 07777, mode);
    }
    assert((dir_stat.st_mode & 07777) == mode);
    assert(rmdir(path) == 0);
  }

  return 0;
}
