#include <unistd.h>
#include <fcntl.h>
#include <sys/wait.h>

// The layer replaces `vfork` with a real `fork`, so this child is a genuine process with its own
// memory rather than one sharing the parent's. The `open` exercises the layer's hooks from inside
// that child: reaching the internal proxy requires the child to hold a connection of its own.
//
// A child of a real `vfork` may only `_exit` or `exec`, which is why the layer substitutes `fork`
// at all, and why the child here still ends with `_exit`.
int main() {
    pid_t pid = vfork();
    if (!pid) {
        const char path[] = "/path/to/some/file";
        open(path, 0);
        _exit(0);
    }
    waitpid(pid, NULL, 0);
}
