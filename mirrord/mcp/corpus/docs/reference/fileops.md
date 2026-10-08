---
title: File Operations Reference
date: 2022-06-15T08:48:45.000Z
lastmod: 2022-06-15T08:48:45.000Z
draft: false
images: []
menu:
  docs:
    parent: reference
weight: 160
toc: true
description: Reference to mirrord's file operations
tags:
  - oss
  - team
  - enterprise
---

# File Operations

### Overview

mirrord will relay file access (except for some exceptions ([Unix](https://github.com/metalbear-co/mirrord/blob/main/mirrord/layer-lib/src/file/unix/read_local_by_default.rs) | [Windows](https://github.com/metalbear-co/mirrord/blob/main/mirrord/layer-lib/src/file/windows/read_local_by_default.rs))) to the target pod by default. This functionality is controlled by the `feature.fs.mode` configuration option (or the `--fs-mode` CLI flag).

#### Modes

| Mode                 | Behavior                                                                                                                            |
| -------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| `local`              | All file operations happen on the local filesystem. mirrord doesn't touch fs syscalls.                                              |
| `localwithoverrides` | Mostly local, but mirrord still applies its built-in overrides for a small set of paths (the default exception lists linked above). |
| `read` _(default)_   | Reads go to the remote pod, writes go to the local filesystem.                                                                      |
| `write`              | Both reads and writes go to the remote pod.                                                                                         |

User overrides and mirrord's default overrides apply on top of the selected mode — for instance, `read` mode will still resolve a handful of paths locally (DNS files, the user's home directory, etc.).

For example, the following python script calls the built-in `open` function which translate to something like `openat(AT_FDCWD, "/tmp/test", O_RDWR|O_CLOEXEC)` at a lower level:

```py
with open("/tmp/test", "r+") as rw_file:
    read_str = rw_file.read(42)
print(read_str)
```

When we run that python script with mirrord:

```bash
mirrord exec -c --target py-serv-deployment-cfc458fd4-bjzjx python3 test.py
```

mirrord overrides that `openat` call and opens `/tmp/test` on the remote pod.

### How does it work?

Once a request to open a new file is received by `mirrord-agent` from `mirrord-layer`, the agent forwards the request to the container in the remote pod in context of the provided path for the open system call, prefixed with path to the root directory of the container.

`mirrord-agent` uses APIs provided by docker and containerd runtimes to get the PID of the remote container, and refers to the root directory of the remote container through `/proc/container_pid/root`

### Syscalls

mirrord overrirdes calls to the following libc functions/system calls:

#### open

`int open(const char *pathname, int flags);`

Open files on the remote pod. Functionality when opening with different types of paths might differ. In the case when `pathname` is specified to be a relative path, the call to open is sent to libc instead of the remote pod.

Example:

```py
import os
fd = os.open("/tmp/test", os.O_WRONLY | os.O_CREAT)
```

#### openat

`int openat(int dirfd, const char *pathname, int flags);`

`openat` works the same as `open` when `dirfd` is specified as `AT_FDCWD` or if the path is absolute. If a valid `dirfd` is provided, files relative to the directory referred to by the `dirfd` can be opened.

Example:

```py
dir = os.open("/tmp", os.O_RDONLY | os.O_NONBLOCK | os.O_CLOEXEC | os.O_DIRECTORY)

os.open("test", os.O_RDWR | os.O_NONBLOCK | os.O_CLOEXEC, dir_fd=dir)
```

#### read

`ssize_t read(int fd, void *buf, size_t count);`

Read from a file on the remote pod.

Example:

```py
fd = os.open("/tmp/test", os.O_RDWR | os.O_NONBLOCK | os.O_CLOEXEC)
read = os.read(fd, 1024)
```

#### write

`ssize_t write(int fd, const void *buf, size_t count);`

Write to a file on the remote pod.

Example:

```py
with open("/tmp/test", "w") as file:
    file.write(TEXT)
```

#### lseek

`off_t lseek(int fd, off_t offset, int whence);`

Reposition the file offset of an open file on the remote pod. lseek through mirrord-layer supports all valid options for whence as specified in the Linux manpages.

Example:

```py
with open("/tmp/test", "w") as file:
    file.seek(10)
    file.write(TEXT)
```

> **Note:** For read, write, and lseek if the provided `fd` is a valid file descriptor i.e. it refers to a file opened on the remote pod then the call is forwarded to the remote pod, otherwise the call is sent to libc.
