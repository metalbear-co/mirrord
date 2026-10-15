---
title: File Operations
description: Control which files your local process reads and writes remotely
draft: false
toc: true
tags: ["oss", "team", "enterprise"]
---

By default, mirrord redirects most file access from your local process to the remote pod. When your code opens `/etc/config/app.yaml`, it reads the remote pod's copy. This means your local process sees the same configuration files, certificates, and data that the deployed application does.

Some paths, like language runtimes, package managers, and temp files, are always read locally. See the [full default list](https://github.com/metalbear-co/mirrord/blob/main/mirrord/layer-lib/src/file/unix/read_local_by_default.rs) for details.

## Choosing a mode

mirrord supports three file system modes:

| Mode | Behavior | Use when |
|------|----------|----------|
| `read` (default) | Reads from remote, writes locally | You want remote config but don't want to risk writing to remote files |
| `local` | All file access stays local | You don't need any remote files |
| `localwithoverrides` | All file access local, except paths you specify | You need just a few specific remote files |

```json
{
  "feature": {
    "fs": {
      "mode": "read"
    }
  }
}
```

## Reading specific files remotely

Use `localwithoverrides` when you only need a handful of remote files, for example a config file or TLS certificate:

```json
{
  "feature": {
    "fs": {
      "mode": "localwithoverrides",
      "read_only": ["/etc/config/.*", "/var/secrets/.*"]
    }
  }
}
```

Paths support regex patterns.

## Writing files remotely

If your application writes to files that need to land on the remote filesystem (e.g. uploading to a shared volume), use `read_write`:

```json
{
  "feature": {
    "fs": {
      "read_write": ["/mnt/shared/uploads/.*"]
    }
  }
}
```

Be careful with remote writes. They modify the actual remote pod's filesystem.

## Keeping specific files local

If the default `read` mode pulls in remote files that conflict with your local setup (e.g. `/tmp` files, lock files), you can force them to stay local:

```json
{
  "feature": {
    "fs": {
      "local": ["/tmp/.*", ".*\\.lock"]
    }
  }
}
```

## Common scenarios

**"My app reads config from a mounted ConfigMap"** The default `read` mode handles this. Your local process will read the ConfigMap contents from the remote pod.

**"My app writes logs to a file and I don't want them on the remote pod"** Default `read` mode already writes locally. No change needed.

**"I only need one remote config file, everything else should be local"** Use `localwithoverrides` with the specific path.

For the full list of file operation settings, see the [configuration reference](https://metalbear.com/mirrord/docs/config#feature.fs).
For a technical explanation of how file operations work under the hood, see the [File Operations reference](../reference/fileops.md).
