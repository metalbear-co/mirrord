---
title: Filtering Outgoing Traffic
date: 2020-11-16T12:59:39.000Z
lastmod: 2020-11-16T12:59:39.000Z
draft: false
menu:
  docs:
    parent: using-mirrord
weight: 120
toc: true
tags:
  - oss
  - team
  - enterprise
description: Control which outgoing connections go through the cluster and which stay local
---

There are several features underlying mirrord's ability to let your local app send outgoing network requests to cluster resources:

1. By importing the remote target's environment variables, your app will send the request to the remote hostnames configured in them.
2. By intercepting DNS resolution, mirrord will resolve the remote hostnames to the remote pod's IP address.
3. Finally, by intercepting outgoing network requests, mirrord will send the request from the remote pod, allowing it to access resources that are only available from within the cluster.

However, sometimes you might have a resource in the cluster that you don't want to access from your local process - perhaps a shared database. This is what the outgoing filter is for. It allows you to specify a list of hostnames that should be resolved and accessed remotely, or a list of hostnames that should be resolved and accessed locally. That way, you can run a local instance of your database and have your local process read and write to it, while still running all other operations against the cluster.

For example, if you want your app to access the hostname `example-hostname.svc` locally, and everything else remotely, you can do it with the following configuration:

```json
{
  "feature": {
    "network": {
      "outgoing": {
        "filter": {
          "local": ["example-hostname.svc"]
        }
      }
    }
  }
}
```

You can see all the configuration options for the outgoing filter feature [here](https://metalbear.com/mirrord/docs/config#feature.network.outgoing.filter).

## What's next?

1. If you'd like to intercept traffic rather than mirror it so that your local process is the one answering the remote requests, check out [this guide](../incoming-traffic/filter-incoming-traffic.md). Note that you can even filter which traffic you intercept!
2. If you don't want to impersonate a remote target - for example, if you want to run a tool in the context of your cluster - check out our [guide on the targetless mode](../targetless.md).
3. If you just want to learn more about mirrord, why not check out our [architecture](../../reference/architecture.md) or [configuration](https://metalbear.com/mirrord/docs/config) sections?
