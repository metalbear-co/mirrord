Fixed file permissions being corrupted when a file was created through `openat64` or
`openat$NOCANCEL` on a path that mirrord handles locally. Both hooks dropped the variadic `mode`
argument before bypassing to libc, so the file was created with whatever value happened to occupy
that argument slot.
