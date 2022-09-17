#include <stdio.h>
#include <unistd.h>
#include <stdlib.h>
#include <string.h>
#include <dlfcn.h>

__attribute__((constructor))
static void arm64e_ctor(void) {
    // printf("hello from arm64e-bridge\n");
    char* mirrord_dylib_path = getenv("MIRRORD_DYLIB_PATH");
    const char* name = getprogname();
    // printf("name: %s\n", name);
    if (mirrord_dylib_path) {
        // printf("arm64e-bridge: loading %s\n", mirrord_dylib_path);
        if (strcmp(name, "debugserver") != 0 && strcmp(name, "dlv") != 0) {
            void* handle = dlopen(mirrord_dylib_path, RTLD_NOW);
            if (handle == NULL) {
                // printf("arm64e-bridge: failed to load");   
            }     
    } else {
        return;
    }
}
    }