---
title: Incoming Traffic
tags:
  - oss
  - team
  - enterprise
description: How mirrord handles incoming network traffic from the cluster
---

mirrord lets your local process receive network traffic that would normally go to a pod in the cluster. There are two modes:

## Mirroring (default)

In the default configuration, mirrord **mirrors** incoming TCP traffic - your local process receives a copy of the traffic while the remote pod continues to handle it normally. Responses from your local process are dropped. This is a safe, read-only way to observe how your code handles real requests.

## Stealing

In **steal** mode, mirrord intercepts incoming traffic and redirects it to your local process instead of the remote pod. Your local process becomes the one responding to real requests. This is useful when you need to test how your code handles and responds to live traffic.

Steal mode can be configured in your mirrord config:

```json
{
  "feature": {
    "network": {
      "incoming": "steal"
    }
  }
}
```

To steal filtered traffic for several services at once, [`mirrord up`](../multiple-concurrent-sessions.md) manages multiple sessions from a single config file.

## What's in this section

- **[Filter Incoming Traffic](filter-incoming-traffic.md)** - Steal only a subset of traffic using HTTP header, W3C `baggage`/`tracestate`, path, or method filters
  - [Filtering by JSON Body](filtering-by-json-body.md) - Filter based on request body content
  - [Debug from Browser](debug-from-browser.md) - Use the Chrome extension to route browser traffic to your local process
- **[Steal HTTPS Requests](steal-https.md)** **[Teams]** - Decrypt and steal HTTPS traffic using TLS certificates
- **[Inspect Live Traffic](inspect-live-traffic.md)** - Monitor incoming traffic without running a local process
