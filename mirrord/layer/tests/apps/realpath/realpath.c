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
    char *resolved_path = realpath(path, NULL);
    assert(resolved_path == NULL);
    assert(errno == ENOENT);
}


// second case: path is absolute, should be resolved locally
// we trust the runner to abort if it is not the case
void absolute_locally() {
    const char path[] = "/dev/good/cake";
    char *resolved_path = realpath(path, NULL);
    assert(resolved_path == NULL);
    assert(errno == ENOENT);
}

// third case: path is absolute, should be resolved remotely
void absolute_remotely() {
    // /etc/hosts is usually resolved remotely
    const char path[] = "/etc/../etc/hosts";
    char output_path[PATH_MAX] = {0};
    char *resolved_path = realpath(path, output_path);
    assert(resolved_path == output_path);
    // assert that resolved path is handled correctly
    printf("resolved_path\n %s", resolved_path);
    assert(strcmp(resolved_path, "/etc/hosts") == 0);
}

// fourth case: path is absolute, we put null in output buffer, should be resolved remotely and allocate for us, and we free it.
void absolute_remotely_malloc() {
    // /etc/hosts is usually resolved remotely
    const char path[] = "/etc/../etc/hosts";
    char *resolved_path = realpath(path, NULL);
    assert(resolved_path != NULL);
    // assert that resolved path is handled correctly
    assert(strcmp(resolved_path, "/etc/hosts") == 0);
    free(resolved_path);
}

// fifth case: path is absolute, doesn't exist - should return null and set appropriate errno
void absolute_remotely_doesnt_exist() {
    const char path[] = "/etc/../etc/hosts";
    char *resolved_path = realpath(path, NULL);
    assert(resolved_path == NULL);
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
