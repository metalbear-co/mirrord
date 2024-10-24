#include <stdio.h>
#include <stdlib.h>
#include <dlfcn.h>

// Function to be executed when the library is loaded
__attribute__((constructor))
void on_library_load() {
    const char *lib_env = getenv("MIRRORD_MACOS_ARM64_LIBRARY");

    printf("%s", lib_env);
    if (lib_env && *lib_env) {
        dlopen(lib_env, RTLD_LAZY);
    }
}
