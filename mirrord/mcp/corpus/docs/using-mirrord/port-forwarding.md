---
title: "Port Forwarding"
description: "How to use mirrord for port forwarding"
date: 2024-09-03T15:39:44+01:00
lastmod: 2024-12-19T15:39:44+01:00
draft: false
menu:
  docs:
    parent: "using-mirrord"
weight: 140
toc: true
tags: ["oss", "team", "enterprise"]
---
## Regular port-forwarding

The port-forward command allows you to forward traffic from a local port to any
destination that the mirrord targeted pod has access to, in a similar way to
`kubectl port-forward`.
The traffic is forwarded as-if it was coming from the target pod, meaning it has access
to destinations that might be outside the cluster, like third-party APIs, depending
on what's accessible by the target pod.

{% hint style="warning" %}
To use port-forwarding with MySQL databases, at least version 3.177.0 of mirrord is required.
{% endhint %}

You can use the command like so:
```bash
mirrord port-forward --target <target-path> -L <local port>:<remote address>:<remote port>
```

For example, to forward traffic from localhost:8080 to an in-cluster service py-serv listening on port 80:
```bash
mirrord port-forward -L 8080:py-serv:80
```

## Reverse port-forwarding

It also allows for reverse port forwarding, where traffic is redirected from a port on the target pod or workload to a local port, like so:
```bash
mirrord port-forward --target <target-path> -R <remote port>:<local port>
```

For example, to forward traffic from an in-cluster deployment py-serv listening on port 80 to localhost:8080:
```bash
mirrord port-forward --target deployment/py-serv -R 80:8080
```

In addition, multiple ports can be forwarded in one direction or both directions simultaneously in the same command by providing each source and destination as a separate `-L` or `-R` argument.

Regular port forwarding with an `-L` can be done in targetless mode and does not require specifying any target. Reverse port forwarding always requires a target.

## More details

- The local port component of the `-L` argument is optional, and without it the same port will be used locally as on the remote.
- The same is true of the `-R` argument: if one port number is provided, it will be used for both local and remote ports.
- Port-forwarding only supports TCP, not UDP.
- The remote address can be an IPv4 address or a hostname - hostnames are resolved in the cluster.
- In regular port forwarding (`-L`) connections are made lazily and hostname resolution is attempted only data is sent to the local port.
- Reverse forwarding (`-R`) can read the `feature.network.incoming` section of a mirrord config file when the file is passed to the command with `-f`.
