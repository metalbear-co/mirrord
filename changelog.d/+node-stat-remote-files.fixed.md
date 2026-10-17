On Linux, `lstat` through glibc's `__lxstat` no longer follows symlinks. Node.js sees remote file metadata with the new `experimental.hook_libc_syscall` and `experimental.old_glibc_support` options.
