#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

/// Test `readlink`.
///
/// Creates a buffer that will be filled with the contents of the symbolic link
/// `path`.
///
/// The test mimics a `readlink` call if you created the symlink like this:
/// `ln -s /gatos/rajado.txt /gatos/tigrado.txt`.
int main() {
  char *out_buffer;
  ssize_t out_buffer_size = 30;
  out_buffer = malloc(out_buffer_size);

  // The symbolic link path.
  char *path = "/gatos/tigrado.txt";
  ssize_t amount_read = readlink(path, out_buffer, out_buffer_size);
  assert(amount_read <= out_buffer_size);
  assert(amount_read >= 0);

  // `readlink` doesn't terminate the buffer string.
  out_buffer[amount_read] = '\0';

  printf("'%s' -> '%s'", path, out_buffer);

  // "/gatos/rajado.txt" is the original file.
  assert(strcmp("/gatos/rajado.txt", out_buffer) == 0);
  return 0;
}
