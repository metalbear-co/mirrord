Fixed corrupted permissions on files created through `openat64` or `openat$NOCANCEL` on a path that
mirrord handles locally. Both hooks dropped the variadic `mode` argument before bypassing to libc,
so libc read whatever value happened to occupy that argument slot.
