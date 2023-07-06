#include <unistd.h>
#include <fcntl.h>


/// This program forks, and the child process calls `socket`.
/// It is used to verify that hooks are handled in a child socket after a fork.
int main() {
    pid_t pid = fork();
    if (!pid) {
        const char path[] = "/path/to/some/file";
        open(path, 0);
    }
    waitpid(pid, NULL, 0);
}
