Fixed `unlinkat` relative to an opened remote directory to keep using that directory after it is renamed or its original path is replaced, preventing deletion in a replacement directory.
