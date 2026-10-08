---
title: Postman
date: 2025-12-16T12:00:00.000Z
lastmod: 2025-12-16T12:00:00.000Z
draft: false
toc: true
tags:
  - oss
  - team
  - enterprise
description: Connect Postman to your cluster via mirrord.
---

By routing Postman traffic through a local SOCKS5 proxy, you can send API requests to kubernetes internal services (e.g., `http://my-service:8080`) directly from your local machine.

## Prerequisites

This guide assumes you have already set up a local proxy using `microsocks` and `mirrord`. If you haven't done this yet, please follow the [setup guide](./README.md) first to launch the proxy.

## Configuration

1. Ensure the prerequisite [SOCKS5 proxy](./README.md) is running with `mirrord` and targeting the desired cluster.
2. Open Postman and open Settings (click the gear icon in the top right > **Settings**).
3. Select the **Proxy** tab.
4. Under **Proxy configurations for sending requests**:
   * Toggle **Use custom proxy configuration** to **ON**.
   * Enable **HTTP** and **HTTPS**.
   * Select **socks5h** as the proxy server protocol.
   * Enter `127.0.0.1` as the proxy server hostname.
   * Enter `1080` as the proxy server post (or the port you configured).
5. Ensure **Proxy Auth** is **OFF**.
6. Close the settings window.

## Verify  

You can now use cluster DNS names in your requests.
* Create a new request.
* Enter an internal URL (e.g., `http://my-service.namespace.svc.cluster.local:8080`).
* Click **Send**.
