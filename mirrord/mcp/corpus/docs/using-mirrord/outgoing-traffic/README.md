---
title: Outgoing Traffic
description: How mirrord handles outgoing network traffic from your local process
tags:
  - oss
  - team
  - enterprise
---

By default, mirrord intercepts outgoing network requests from your local process and routes them through the remote pod. This means your local code can access cluster-internal services - databases, APIs, message brokers - as if it were running inside the cluster.

This works through three mechanisms:

1. **Environment variables** - Your local process inherits the remote pod's environment variables, so connection strings and hostnames point to the right places
2. **DNS resolution** - mirrord resolves DNS queries in the context of the remote pod, so cluster-internal hostnames (like `my-service.default.svc.cluster.local`) resolve correctly
3. **Network interception** - Outgoing TCP and UDP connections are sent from the remote pod, giving your local process access to resources that are only reachable from within the cluster

Both outgoing TCP and UDP forwarding are enabled by default.

{% hint style="warning" %}
If mirrord is configured to **mirror** incoming traffic, both the remote pod and your local process may make outgoing API calls for the same incoming request. If those calls are write operations (e.g. database inserts), this could lead to duplicates. Consider using [steal mode](../incoming-traffic/README.md) or disabling outgoing traffic forwarding to avoid this.
{% endhint %}

## What's in this section

- **[Filter Outgoing Traffic](filter-outgoing-traffic.md)** - Control which outgoing connections go through the cluster and which stay local
