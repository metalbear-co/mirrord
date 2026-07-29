#define _GNU_SOURCE
#define _LARGEFILE64_SOURCE

#include <assert.h>
#include <dlfcn.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <sys/stat.h>
#include <unistd.h>

/// Every `open`-family hook must forward the variadic `mode` argument when it bypasses the call
/// and runs it locally. A hook that declares the libc function as non-variadic silently drops
/// `mode`, and the file ends up created with whatever garbage occupied the argument slot.
///
/// All paths here live under `/tmp`, which mirrord treats as local, so each call bypasses to libc
/// and the resulting permission bits must match exactly what was requested.
#define EXPECTED_MODE 0640

/// `O_CREAT` only applies `mode` when the file does not exist yet, so a leftover file from an
/// earlier run would mask the very bug this app checks for.
static void remove_stale(const char *path)
{
  assert(unlink(path) == 0 || errno == ENOENT);
}

static void assert_created_with_mode(const char *path, int fd)
{
  assert(fd >= 0);

  struct stat file_stat;
  assert(fstat(fd, &file_stat) == 0);
  assert(close(fd) == 0);

  mode_t actual = file_stat.st_mode & 07777;
  if (actual != EXPECTED_MODE)
  {
    fprintf(stderr, "%s created with mode %04o, expected %04o\n", path, actual, EXPECTED_MODE);
  }
  assert(actual == EXPECTED_MODE);

  assert(unlink(path) == 0);
}

int main()
{
  // So that the expected mode is not masked away.
  umask(0);

  const char *open_path = "/tmp/cor_1734_open";
  remove_stale(open_path);
  assert_created_with_mode(
      open_path, open(open_path, O_CREAT | O_WRONLY | O_TRUNC, EXPECTED_MODE));

  const char *openat_path = "/tmp/cor_1734_openat";
  remove_stale(openat_path);
  assert_created_with_mode(
      openat_path,
      openat(AT_FDCWD, openat_path, O_CREAT | O_WRONLY | O_TRUNC, EXPECTED_MODE));

#ifdef __linux__
  const char *open64_path = "/tmp/cor_1734_open64";
  remove_stale(open64_path);
  assert_created_with_mode(
      open64_path, open64(open64_path, O_CREAT | O_WRONLY | O_TRUNC, EXPECTED_MODE));

  const char *openat64_path = "/tmp/cor_1734_openat64";
  remove_stale(openat64_path);
  assert_created_with_mode(
      openat64_path,
      openat64(AT_FDCWD, openat64_path, O_CREAT | O_WRONLY | O_TRUNC, EXPECTED_MODE));
#endif

#ifdef __APPLE__
  // The `$NOCANCEL` variants have no declaration to call, but they are hooked just like the
  // plain ones, so resolve them at runtime to make sure they forward `mode` too.
  int (*open_nocancel)(const char *, int, ...) = dlsym(RTLD_DEFAULT, "open$NOCANCEL");
  assert(open_nocancel != NULL);
  const char *open_nocancel_path = "/tmp/cor_1734_open_nocancel";
  remove_stale(open_nocancel_path);
  assert_created_with_mode(
      open_nocancel_path,
      open_nocancel(open_nocancel_path, O_CREAT | O_WRONLY | O_TRUNC, EXPECTED_MODE));

  int (*openat_nocancel)(int, const char *, int, ...) = dlsym(RTLD_DEFAULT, "openat$NOCANCEL");
  assert(openat_nocancel != NULL);
  const char *openat_nocancel_path = "/tmp/cor_1734_openat_nocancel";
  remove_stale(openat_nocancel_path);
  assert_created_with_mode(
      openat_nocancel_path,
      openat_nocancel(AT_FDCWD, openat_nocancel_path, O_CREAT | O_WRONLY | O_TRUNC, EXPECTED_MODE));
#endif

  const char *mkdir_path = "/tmp/cor_1734_mkdir";
  assert(rmdir(mkdir_path) == 0 || errno == ENOENT);
  assert(mkdir(mkdir_path, EXPECTED_MODE) == 0);
  struct stat dir_stat;
  assert(stat(mkdir_path, &dir_stat) == 0);
  assert((dir_stat.st_mode & 07777) == EXPECTED_MODE);
  assert(rmdir(mkdir_path) == 0);

  return 0;
}
