`dup2()` or `dup3()` over the last file descriptor of a listening socket stops mirrord from stealing or mirroring the socket's port, instead of leaving it in place until the process exits.
