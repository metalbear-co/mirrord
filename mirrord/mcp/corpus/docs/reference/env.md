---
title: Environment Variables Reference
date: 2022-06-15T08:48:45.000Z
lastmod: 2022-06-15T08:48:45.000Z
draft: false
images: []
menu:
  docs:
    parent: reference
weight: 150
toc: true
description: Reference to including remote environment variables
tags:
  - oss
  - team
  - enterprise
---

# Environment Variables

### Overview

mirrord lets you run a local process in the context of remote environment i.e. environment variables present in the remote pod will be loaded into the local process.

For example, if you want your local process to access a remote database, the connection string configured in the remote pod's environment variable can be used by your local process.

### How does it work?

![mirrord - fileops](../.gitbook/assets/mirrord-env-vars.png)

mirrord-layer sends a message to mirrord-agent requesting remote environment variables, which are then set before the local process starts.

### Usage

To include/exclude environment variables selectively, use the `--override-env-vars-include` flag to include and `--override-env-vars-exclude` to exclude with environment variables specified in a `semicolon` separated list.

> **Note:** These flags are mutually exclusive. For example, if one chooses to exclude using the `--override-env-vars-exclude` flag, then there is no need to use `--override-env-vars-include="*"` to include all other environment variables.

By default, all environment variables are included.

**Example**

If on our target pod, we have the environment variable `ENV_VAR1` with the value `remote-value` and on our local machine we have `ENV_VAR1` with value `local-value`, then Running the python interpreter with mirrord would look like this:

```bash
MIRRORD_AGENT_IMAGE=test MIRRORD_AGENT_RUST_LOG=trace RUST_LOG=debug target/debug/mirrord exec -c --target pod/py-serv-deployment-ff89b5974-x9tjx python3

Python 3.9.13 (v3.9.13:6de2ca5339, May 17 2022, 11:23:25)
[Clang 6.0 (clang-600.0.57)] on darwin
Type "help", "copyright", "credits" or "license" for more information.
>>> import os
>>> print(os.environ['ENV_VAR1'])
remote-value
```

Logs

```bash
❯ MIRRORD_AGENT_IMAGE=test MIRRORD_AGENT_RUST_LOG=trace RUST_LOG=debug target/debug/mirrord exec -c --target pod/py-serv-deployment-ff89b5974-x9tjx python3
...
2022-07-01T17:18:33.744996Z DEBUG mirrord_layer: ClientMessage::GetEnvVarsRequest codec_result Ok(
    (),
)
2022-07-01T17:18:33.754270Z DEBUG mirrord_layer: DaemonMessage::GetEnvVarsResponse Ok(
    {
        "KUBERNETES_PORT": "tcp://10.96.0.1:443",
        "LANG": "C.UTF-8",
        "KUBERNETES_SERVICE_PORT": "443",
        "PY_SERV_PORT": "tcp://10.96.139.36:80",
        "KUBERNETES_PORT_443_TCP": "tcp://10.96.0.1:443",
        "PY_SERV_SERVICE_PORT": "80",
        "KUBERNETES_SERVICE_PORT_HTTPS": "443",
        "PYTHON_SETUPTOOLS_VERSION": "58.1.0",
        "PY_SERV_PORT_80_TCP_ADDR": "10.96.139.36",
        "PYTHON_GET_PIP_SHA256": "ba3ab8267d91fd41c58dbce08f76db99f747f716d85ce1865813842bb035524d",
        "ENV_VAR1": "remote-value",
        "KUBERNETES_SERVICE_HOST": "10.96.0.1",
        "KUBERNETES_PORT_443_TCP_PORT": "443",
        "HOSTNAME": "py-serv-deployment-ff89b5974-x9tjx",
        "KUBERNETES_PORT_443_TCP_ADDR": "10.96.0.1",
        "GPG_KEY": "E3FF2839C048B25C084DEBE9B26995E310250568",
        "PYTHON_GET_PIP_URL": "https://github.com/pypa/get-pip/raw/6ce3639da143c5d79b44f94b04080abf2531fd6e/public/get-pip.py",
        "PY_SERV_PORT_80_TCP": "tcp://10.96.139.36:80",
        "KUBERNETES_PORT_443_TCP_PROTO": "tcp",
        "PYTHON_VERSION": "3.9.13",
        "PY_SERV_PORT_80_TCP_PROTO": "tcp",
        "PY_SERV_PORT_80_TCP_PORT": "80",
        "PY_SERV_SERVICE_HOST": "10.96.139.36",
        "PYTHON_PIP_VERSION": "22.0.4",
    },
)!
...
```
