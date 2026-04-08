#include <assert.h>
#include <stdio.h>
#include <sys/fcntl.h>
#include <sys/stat.h>
#include <unistd.h>


/// Test `mkdir`, `mkdirat` and `rmdir`.
///
/// For each of `mkdir` and `mkdirat`:
/// - creates a folder in a path that mirrord should ignore (bypass and do locally).
/// - deletes that folder (should happen locally as well).
/// - creates a folder in a path that mirrord should handle remotely.
/// - deletes that folder (should happen remotely as well).
///
int main()
{
  // First create and delete a dir that should be created and deleted locally and therefore
  // bypassed in the layer.
  // This should not result in any client message.
  char *ignored_dir = "/tmp/rmdir_test_dir";

  // CREATE dir (should happen locally)
  int local_mkdir_result = mkdir(ignored_dir, 0777);
  assert(local_mkdir_result == 0);

  // DELETE dir (should happen locally)
  int local_rmdir_result = rmdir(ignored_dir);
  assert(local_rmdir_result == 0);

  char *ignored_mkdirat_test_path = "/tmp/mkdirat_test_path";

  // CREATE dir (should happen locally)
  int local_mkdirat_result = mkdirat(AT_FDCWD, ignored_mkdirat_test_path, 0777);
  assert(local_mkdirat_result == 0);

  // DELETE dir (should happen locally)
  int local_rmdir_mkdirat_result = rmdir(ignored_mkdirat_test_path);
  assert(local_rmdir_mkdirat_result == 0);

  // This should result in client messages.
  char *test_dir = "/mkdir_rmdir_test_dir";

  // CREATE dir (should happen remotely)
  int mkdir_result = mkdir(test_dir, 0777);
  assert(mkdir_result == 0);

  // DELETE dir (should happen remotely)
  int rmdir_result = rmdir(test_dir);
  assert(rmdir_result == 0);

  char *mkdirat_test_path = "/mkdirat_rmdir_test_dir";

  // CREATE dir (should happen remotely)
  int mkdirat_result = mkdirat(AT_FDCWD, mkdirat_test_path, 0777);
  assert(mkdirat_result == 0);

  // DELETE dir (should happen remotely)
  int rmdir_mkdirat_result = rmdir(mkdirat_test_path);
  assert(rmdir_mkdirat_result == 0);

  return 0;
}
