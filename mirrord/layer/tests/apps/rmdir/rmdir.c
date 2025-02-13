#include <assert.h>
#include <stdio.h>
#include <unistd.h>
#include <sys/stat.h>

/// Test `rmdir`.
///
/// Creates a folder and then removes it.
///
int main()
{
  // First create and delete a dir that should be created and deleted locally and therefore
  // bypassed in the layer.
  // This should not result in any client message.
  char *test_dir = "/tmp/rmdir_test_dir";
  int mkdir_result = mkdir(test_dir, 0777);
  assert(mkdir_result == 0);

  int rmdir_result = rmdir(test_dir);
  assert(rmdir_result == 0);

  // This should result in client messages.
  char *test_dir = "/rmdir_test_dir";
  int mkdir_result = mkdir(test_dir, 0777);
  assert(mkdir_result == 0);

  int rmdir_result = rmdir(test_dir);
  assert(rmdir_result == 0);

  return 0;
}
