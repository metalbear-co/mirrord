#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/stat.h>

/// Test `mkdir`.
///
/// Creates a folder in the current directory.
///
int main()
{
  char *path = "/test_mkdir_folder";
  int result = mkdir(path, 0777);

  assert(result == 0);
  return 0;
}
