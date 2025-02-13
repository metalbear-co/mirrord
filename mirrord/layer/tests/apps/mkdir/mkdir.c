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
  // Create a directory at a path that is ignored by default by the layer,
  // so that the test can make sure it gets ignored.
  char *ignored_mkdir_path = "/tmp/mkdir_test_path";
  int local_mkdir_result = mkdir(ignored_mkdir_path, 0777);
  assert(local_mkdir_result == 0);

  char *mkdir_test_path = "/mkdir_test_path";
  int mkdir_result = mkdir(mkdir_test_path, 0777);
  assert(mkdir_result == 0);

  // Create a directory at a path that is ignored by default by the layer,
  // so that the test can make sure it gets ignored.
  char *ignored_mkdirat_path = "/tmp/mkdirat_test_path";
  int local_mkdirat_result = mkdirat(AT_FDCWD, ignored_mkdirat_path, 0777);
  assert(local_mkdirat_result == 0);

  char *mkdirat_test_path = "/mkdirat_test_path";
  int mkdirat_result = mkdirat(AT_FDCWD, mkdirat_test_path, 0777);
  assert(mkdirat_result == 0);

  return 0;
}
