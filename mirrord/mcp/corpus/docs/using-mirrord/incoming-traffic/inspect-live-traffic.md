---
title: "Inspecting Live Traffic"
description: "Use mirrord dump to watch the incoming traffic to a workload in your cluster, live in your terminal."
date: 2025-08-11T00:00:00+03:00
lastmod: 2025-08-11T00:00:00+03:00
draft: false
menu:
  docs:
    parent: "using-mirrord"
toc: true
tags: ["oss", "team", "enterprise"]
---

# Inspecting Live Traffic

The `mirrord dump` command lets you inspect incoming traffic to a Kubernetes resource - such as a Deployment, Service, Pod, or StatefulSet - directly in your terminal.

When is this useful?
1. Debugging incoming traffic: 
If your service is returning errors and you’re not sure why, `mirrord dump` lets you see exactly what’s hitting the service - so you can determine whether traffic is reaching the service at all, or if it is arriving malformed, to quickly identify the source of errors.
2. Verifying mirroring: 
If you suspect `mirrord exec` isn’t working, you can run `mirrord dump`. If you see traffic appear, mirroring is functioning correctly, so you can rule that part out.
3. Onboarding mirrord in your organization:
Inspect real traffic to easily find the right filter for isolating each developer’s requests in shared environments.



## Prerequisites

Before you start, make sure you have:
1. `kubectl` configured to access your target cluster.
2. mirrord installed and working.
3. The details of your target resource (for example, the Deployment name and the port you want to inspect). The command cannot run in targetless mode.

## Using `mirrord dump`

1. Run a command like:
   ```
   mirrord dump -t deployment/my-deployment -p 80
    ```
2. mirrord will:
    - Deploy a mirrord agent (via operator or OSS) in the target Pod’s network namespace.
    - Listen for incoming traffic on the specified port.

3. You’ll see the raw output for each connection, request, and payload directly in your terminal:
```
Listening for traffic... Press Ctrl+C to stop

New connection established: Connection ID 0 from x.x.x.x:port to y.y.y.y:port
Connection ID 0: 93 bytes
Data: GET /health HTTP/1.1
Host: your-service
...
Connection ID 0 closed
```
4. Press `Ctrl+C` to stop the dump session.

### Automatically detecting ports on the target

{% hint style="info" %}
This feature requires at least mirrord version **3.191.0**.
{% endhint %}

If you want to listen for traffic on the TCP ports declared by the target container, you can omit the ports argument like so:
```
mirrord dump -t deployment/my-deployment
```

From mirrord version **3.282.0**, ports of other containers in the pod are not detected. To listen on one of those, or on a port that isn't declared, pass it with `-p`.

`mirrord dump` exits with an error when it can't detect any ports, which is always the case in [copy target](../copy-target.md) and multi-cluster sessions. Pass the ports with `-p`.
