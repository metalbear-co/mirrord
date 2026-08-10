---
title: Configuration Examples
date: 2023-05-17T12:59:39.000Z
lastmod: 2026-08-10T00:00:00.000Z
draft: false
images: []
menu:
  docs:
    parent: reference
weight: 110
toc: true
tags:
  - open source
  - team
  - enterprise
description: Getting started with mirrord configuration.
---

# Getting Started

mirrord allows for a high degree of customization when it comes to which features you want to
enable, and how they should function.

All of the configuration fields have a default value, so a minimal configuration would be no
configuration at all.

The configuration supports [templating](#root-templating), so values can be derived at runtime
instead of hardcoded.

To use a configuration file in the CLI, use the `-f <CONFIG_PATH>` flag.
Or if using VSCode Extension or JetBrains plugin, simply create a `.mirrord/mirrord.json` file
or use the UI.

## Templating {#root-templating}

Config files are rendered with the [Tera](https://keats.github.io/tera/) template engine before
they are parsed, so Tera's built-in functions and filters all work. On top of those, mirrord
provides these variables:

- `key` - the [session key](#root-key), either the one you provided or the one mirrord generated
  for this session.
- `git_branch` - the branch checked out in the working directory mirrord was started from. Set
  `MIRRORD_BRANCH_NAME` to override it; the JetBrains plugin does exactly that, with the branch
  of the project you have open.

mirrord generates a session key for you, and you can reference it as `{{ key }}` in your HTTP
filter like so:

```json
{
  "feature": {
    "network": {
      "incoming": {
        "mode": "steal",
        "http_filter": {
          "header_filter": "^baggage: .*mirrord-session={{ key }}.*$"
        }
      }
    }
  }
}
```

It also supports setting your git branch as the key, so that each branch gets its own session:

```json
{
  "key": "{{ git_branch }}",
  "feature": {
    "network": {
      "incoming": {
        "mode": "steal",
        "http_filter": {
          "header_filter": "^baggage: .*mirrord-session={{ key }}.*$"
        }
      }
    }
  }
}
```

### Templating the `key` field {#root-templating-key}

The [`key`](#root-key) field is read out of the config file *before* any templating happens, to
break the cycle where the key is needed to render templates but is itself defined in the file
being rendered. Two consequences:

1. The file has to stay valid JSON/TOML/YAML as written. A double-quoted string inside a `key`
   template ends the surrounding JSON string early, so the `key` field is silently ignored and
   mirrord falls back to a generated key, leaving a session that looks healthy but filters on
   the wrong value. Tera accepts single-quoted string literals, so use those in `key`:
   `default(value='shared')` rather than `default(value="shared")`. Every other field is
   rendered before parsing and accepts either quote style.
2. Only variables that don't depend on the key are available there, which today means
   `git_branch`.

### When a variable is undefined {#root-templating-undefined}

`git_branch` is left out of the context entirely when the branch can't be determined - the
directory isn't a git repository, `git` isn't installed, or `HEAD` is detached, which is the
usual state in CI. Referencing it then fails the render with
``Variable `git_branch` not found in context``, rather than quietly resolving to an empty
string and producing a filter that matches nothing.

Give configs that also have to work in those environments a fallback:

```json
{
  "key": "{{ git_branch | default(value='shared') }}"
}
```

If you want us to provide any other value, please let us know.

## Examples

To help you get started, here are examples of a basic configuration file, and a complete
configuration file containing all fields.

### Basic `config.json` {#root-basic}

```json
{
  "target": "pod/bear-pod",
  "feature": {
    "env": true,
    "fs": "read",
    "network": true
  }
}
```

### Basic `config.json` with templating {#root-basic-templating}

```json
{
  "target": "{{ get_env(name="TARGET", default="pod/fallback") }}",
  "feature": {
    "env": true,
    "fs": "read",
    "network": true
  }
}
```

### Complete `config.json` {#root-complete}

 Don't use this example as a starting point, it's just here to show you all the available
 options.
```json
{
  "accept_invalid_certificates": false,
  "skip_processes": "ide-debugger",
  "target": {
    "path": "pod/bear-pod",
    "namespace": "default"
  },
  "connect_tcp": null,
  "agent": {
    "log_level": "info",
    "json_log": false,
    "labels": { "user": "meow" },
    "annotations": { "cats.io/inject": "enabled" },
    "namespace": "default",
    "image": "ghcr.io/metalbear-co/mirrord:latest",
    "image_pull_policy": "IfNotPresent",
    "image_pull_secrets": [ { "name": "secret" } ],
    "ttl": 30,
    "ephemeral": false,
    "communication_timeout": 30,
    "startup_timeout": 360,
    "flush_connections": true,
    "metrics": "0.0.0.0:9000",
  },
  "feature": {
    "env": {
      "include": "DATABASE_USER;PUBLIC_ENV",
      "exclude": "DATABASE_PASSWORD;SECRET_ENV",
      "override": {
        "DATABASE_CONNECTION": "db://localhost:7777/my-db",
        "LOCAL_BEAR": "panda"
      },
      "mapping": {
        ".+_TIMEOUT": "1000"
      }
    },
    "fs": {
      "mode": "write",
      "read_write": ".+\\.json" ,
      "read_only": [ ".+\\.yaml", ".+important-file\\.txt" ],
      "local": [ ".+\\.js", ".+\\.mjs" ]
    },
    "network": {
      "incoming": {
        "mode": "steal",
        "http_filter": {
          "header_filter": "^baggage: .*mirrord-session={{ key }}.*$"
        },
        "port_mapping": [[ 7777, 8888 ]],
        "ignore_localhost": false,
        "ignore_ports": [9999, 10000]
      },
      "outgoing": {
        "tcp": true,
        "udp": true,
        "filter": {
          "local": ["tcp://1.1.1.0/24:1337", "1.1.5.0/24", "google.com", ":53"]
        },
        "ignore_localhost": false,
        "unix_streams": "bear.+"
      },
      "dns": {
        "enabled": true,
        "filter": {
          "local": ["1.1.1.0/24:1337", "1.1.5.0/24", "google.com"]
        }
      }
    },
    "copy_target": {
      "scale_down": false
    }
  },
  "operator": true,
  "kubeconfig": "~/.kube/config",
  "sip_binaries": "bash",
  "telemetry": true,
  "kube_context": "my-cluster"
}
```
