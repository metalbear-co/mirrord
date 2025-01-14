#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <fcntl.h>
#include <sys/stat.h>
#include <sys/types.h>

#if defined(__APPLE__) && defined(__MACH__)
#include <sys/param.h>
#include <sys/mount.h>
#else
#include <sys/vfs.h>
#endif

/// Test `statfs / fstatfs`.
///
/// Gets information about a mounted filesystem
///
int main()
{
  char *tmp_test_path = "/statfs_fstatfs_test_path";
  mkdir(tmp_test_path, 0777);

  // statfs
  struct statfs statfs_buf;
  if (statfs(tmp_test_path, &statfs_buf) == -1)
  {
    perror("statfs failed");
    return EXIT_FAILURE;
  }

  // fstatfs
  int fd = open(tmp_test_path, O_RDONLY);

  if (fd == -1)
  {
    perror("Error opening tmp_test_path");
    return 1;
  }

  struct statfs fstatfs_buf;
  if (fstatfs(fd, &fstatfs_buf) == -1)
  {
    perror("fstatfs failed");
    close(fd);
    return EXIT_FAILURE;
  }

  close(fd);
  return 0;
}
