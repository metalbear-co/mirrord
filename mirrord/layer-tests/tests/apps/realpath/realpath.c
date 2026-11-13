#include <stdlib.h>
#include <assert.h>
#include <err.h>
#include <errno.h>
#include <limits.h>
#include <string.h>
#include <stdio.h>

// first case: path is relative, should be resolved locally
// we trust the runner to abort if it is not the case
void relative_locally() {
    const char path[] = "./fun/me";
    errno = 0;
    char *resolved_path = realpath(path, NULL);
    assert(resolved_path == NULL);
    assert(errno == ENOENT);
}


// second case: path is absolute, should be resolved locally
// we trust the runner to abort if it is not the case
void absolute_locally() {
    const char path[] = "/dev/good/cake";
    errno = 0;
    char *resolved_path = realpath(path, NULL);
    assert(resolved_path == NULL);
    assert(errno == ENOENT);
}

// third case: path is absolute, should be resolved remotely
void absolute_remotely() {
    // /app matches none of the default local patterns, so the default `read` mode resolves it
    // remotely. The layer matches the patterns against the path as the app gives it, before it
    // removes the `..`, so /etc/../etc/hosts would stay local under `^/etc`.
    const char path[] = "/app/config/../data.txt";
    char output_path[PATH_MAX] = {0};
    char *resolved_path = realpath(path, output_path);
    assert(resolved_path == output_path);
    // assert that resolved path is handled correctly
    printf("resolved_path\n %s", resolved_path);
    assert(strcmp(resolved_path, "/app/data.txt") == 0);
}

// fourth case: path is absolute, we put null in output buffer, should be resolved remotely and allocate for us, and we free it.
void absolute_remotely_malloc() {
    const char path[] = "/app/config/../data.txt";
    char *resolved_path = realpath(path, NULL);
    assert(resolved_path != NULL);
    // assert that resolved path is handled correctly
    assert(strcmp(resolved_path, "/app/data.txt") == 0);
    free(resolved_path);
}

// fifth case: path is absolute, doesn't exist - should return null and set appropriate errno
void absolute_remotely_doesnt_exist() {
    const char path[] = "/app/missing.txt";
    // The cases above leave ENOENT in errno, which would hide a `realpath` that fails without
    // setting it.
    errno = 0;
    char *resolved_path = realpath(path, NULL);
    assert(resolved_path == NULL);
    printf("%d ernno", errno);
    assert(errno == ENOENT);
}

/// Test few cases of using realpath
int main() {
    relative_locally();
    absolute_locally();
    absolute_remotely();
    absolute_remotely_malloc();
    absolute_remotely_doesnt_exist();
}
