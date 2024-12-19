#include <assert.h>
#include <stdio.h>
#include <sys/stat.h>
#include <sys/fcntl.h>

/// Test `mkdir`.
///
/// Creates a folder in the current directory.
///
int main()
{
  char *mkdir_test_path = "/mkdir_test_path";
  int mkdir_result = mkdir(mkdir_test_path, 0777);
  assert(mkdir_result == 0);

  char *mkdirat_test_path = "/mkdirat_test_path";
  int mkdirat_result = mkdirat(AT_FDCWD, mkdirat_test_path, 0777);
  assert(mkdirat_result == 0);

  return 0;
}
