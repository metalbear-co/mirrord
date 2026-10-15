---
title: Web Browsing
date: 2024-01-30T17:03:00.000Z
lastmod: 2024-01-30T17:03:00.000Z
draft: false
menu:
  docs:
    parent: using-mirrord
weight: 135
toc: true
tags:
  - oss
  - team
  - enterprise
description: Using mirrord to browse using cluster networking
---

You can use mirrord to browse the web as if you were inside your cluster. This is useful for accessing internal tools (like Grafana, Kibana, or ArgoCD) that are not exposed to the public internet, or for verifying that a service is reachable from within the namespace.

## Prerequisites

This guide assumes you have already set up a local proxy using `microsocks` and `mirrord`. If you haven't done this yet, please follow the [setup guide](./README.md) first to launch the proxy.

Once the proxy is configured, [install "SOCKS5 Configurator" Chrome extension](https://chromewebstore.google.com/detail/socks5-configurator/hnpgnjkeaobghpjjhaiemlahikgmnghb)

## Configuration

1. Ensure the prerequisite [SOCKS5 proxy](./README.md) is running with `mirrord` and targeting the desired cluster.
2. In a Chrome window:
   1. Open the SOCKS5 Configurator extension
   2. Make sure the "SOCKS5 Proxy" is enabled
   3. Type in its respective textbox `127.0.0.1:1080`
   4. Hit the save button
3. That's it! You can verify your IP address has changed via a quick "what is my ip address" search in Google

## What's next?

1. If you'd like to intercept traffic rather than mirror it so that your local process is the one answering the remote requests, check out [this guide](../incoming-traffic/filter-incoming-traffic.md). Note that you can even filter which traffic you intercept!
2. If your local process reads from a queue, you might want to test out the [copy target feature](../copy-target.md), which temporarily creates a copy of the mirrord session target. With its `scaledown` flag it allows you to temporarily delete all replicas in your targeted rollout or deployment, so that none competes with your local process for queue messages.
3. If you just want to learn more about mirrord, why not check out our [architecture](../../reference/architecture.md) or [configuration](https://metalbear.com/mirrord/docs/config) sections?
