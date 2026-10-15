---
title: Connecting Tools to the Cluster
date: 2025-12-16T12:00:00.000Z
lastmod: 2025-12-16T12:00:00.000Z
draft: false
toc: true
tags:
  - oss
  - team
  - enterprise
description: Use mirrord to connect local tools like browsers, Postman, and database clients to cluster services
---

Sometimes you need to use local tools like Web Browsers, Postman, or Database Clients, to interact with services that are only accessible from inside your cluster.

Instead of setting up complex port-forwards or VPNs, you can use `mirrord` to create a lightweight SOCKS5 tunnel. This allows any application that supports SOCKS5 to resolve internal DNS and connect to private cluster services as if it were running inside the cluster itself.

## Prerequisites

Install **microsocks** via your package manager.
* For macOS: `brew install microsocks`
* For Ubuntu/Debian: `apt install microsocks`

## Setup

Regardless of which tool you are connecting, the first step is to establish the tunnel.

1. Start the proxy by running one of the following commands in your terminal:
  - `mirrord exec microsocks` to run targetless in the default namespace
  - `mirrord exec -t deployment/my-deployment microsocks` to target a specific deployment
  - `mirrord exec -a my-namespace microsocks` to run within a specific namespace

2. Verify the proxy is working. microsocks listens on `127.0.0.1:1080` by default. Try calling a service health endpoint in your kubernetes cluster using `curl --socks5 socks5h://127.0.0.1:1080 <svc>.<namespace>.svc.cluster.local/health`

## Tool Guides

Once the proxy is running, choose your tool below to configure the connection:

* [Web Browsing](./web-browsing.md) - Using mirrord to browse using cluster networking.
* [Postman](./postman.md) - Send API requests via postman to kubernetes services.
