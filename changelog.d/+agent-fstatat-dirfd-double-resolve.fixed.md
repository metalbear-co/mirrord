Fixed `fstatat(dirfd, name, ...)` against a remote directory fd always failing with `ENOENT`, which made `ioutil.ReadDir` and `(*os.File).Readdir` in Go silently return an empty directory listing.
