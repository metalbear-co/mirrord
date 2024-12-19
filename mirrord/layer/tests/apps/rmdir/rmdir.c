#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>
#include <sys/fcntl.h>

/// Test `rmdir`.
///
/// Creates a folder and then removes it.
///
int main()
{
  char *test_dir = "/test_dir";
  int mkdir_result = mkdir(mkdir_result, 0777);
  assert(mkdir_result == 0);

  int rmdir_result = rmdir(test_dir);
  assert(rmdir_result == 0);

  return 0;
}
