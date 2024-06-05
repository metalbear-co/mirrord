#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

int main() {
  char *out_buffer;
  ssize_t out_buffer_size = 16;
  out_buffer = malloc(out_buffer_size);

  char *path = "/gatos/link.txt";
  ssize_t amount_read = readlink(path, out_buffer, out_buffer_size);
  assert(amount_read <= out_buffer_size);
  assert(amount_read >= 0);

  out_buffer[amount_read] = '\0';

  printf("'%s' -> '%s'", path, out_buffer);
  return 0;
}
