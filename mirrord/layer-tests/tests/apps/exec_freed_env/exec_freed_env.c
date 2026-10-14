// Calls `execv` on a binary that the layer patches for SIP, with an environment that the layer
// changes while it does so.
//
// The binary is in a `.app` bundle, so patching it sets `DYLD_FALLBACK_FRAMEWORK_PATH`. That
// `setenv` moves `environ` to a bigger array and frees the one `execv` passed on as `envp`. The
// environment is rebuilt small and with an odd number of entries first, so that the array is
// exactly a multiple of the allocator's 16 byte quantum and has to move to grow.
//
// Started with the path of the binary, and the binary started with `child`, which checks that
// the environment made it through.

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#define MARKER "MIRRORD_EXEC_FREED_ENV"
#define MARKER_VALUE "intact"

extern char **environ;

static int child(void)
{
    const char *value = getenv(MARKER);
    if (value == NULL || strcmp(value, MARKER_VALUE) != 0)
    {
        fprintf(stderr, "error: %s is %s\n", MARKER, value ? value : "missing");
        return 1;
    }

    printf("%s=%s\n", MARKER, value);
    return 0;
}

static int keeps(const char *entry)
{
    const char *prefixes[] = {"MIRRORD_", "DYLD_INSERT_LIBRARIES=", "PATH=", "HOME=", "TMPDIR=", NULL};
    for (const char **prefix = prefixes; *prefix != NULL; prefix++)
    {
        if (strncmp(entry, *prefix, strlen(*prefix)) == 0)
        {
            return 1;
        }
    }

    return 0;
}

int main(int argc, char *argv[])
{
    if (argc == 2 && strcmp(argv[1], "child") == 0)
    {
        return child();
    }
    if (argc != 2)
    {
        fprintf(stderr, "error: usage: %s <binary>\n", argv[0]);
        return 1;
    }

    size_t count = 0;
    while (environ[count] != NULL)
    {
        count++;
    }
    char **kept = calloc(count + 1, sizeof(char *));
    size_t kept_count = 0;
    for (size_t i = 0; i < count; i++)
    {
        if (keeps(environ[i]))
        {
            kept[kept_count++] = strdup(environ[i]);
        }
    }

    // From here on `environ` is an array that `setenv` allocated, and grows with `realloc`.
    static char *empty[] = {NULL};
    environ = empty;
    for (size_t i = 0; i < kept_count; i++)
    {
        char *separator = strchr(kept[i], '=');
        *separator = '\0';
        setenv(kept[i], separator + 1, 1);
    }
    setenv(MARKER, MARKER_VALUE, 1);
    unsetenv("DYLD_FALLBACK_FRAMEWORK_PATH");

    count = 0;
    while (environ[count] != NULL)
    {
        count++;
    }
    if (count % 2 == 0)
    {
        setenv("MIRRORD_EXEC_FREED_ENV_PADDING", "1", 1);
    }

    char *child_argv[] = {argv[1], "child", NULL};
    execv(argv[1], child_argv);
    perror("error: execv");
    return 1;
}
