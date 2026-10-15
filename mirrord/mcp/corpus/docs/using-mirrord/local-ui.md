---
title: "Local UI"
description: "Launch a local web dashboard that watches every active mirrord session"
date: 2026-04-28T00:00:00+00:00
lastmod: 2026-04-28T00:00:00+00:00
draft: false
menu:
  docs:
    parent: "using-mirrord"
weight: 175
toc: true
tags:
  - alpha
  - oss
  - team
  - enterprise
---

`mirrord ui` launches a small local web dashboard that watches every active mirrord session on your machine and, when you're connected to a mirrord operator, every operator session your kubeconfig can see across the cluster. It runs entirely on your laptop. Nothing in your cluster has to change to use it.

## What it shows

- **Local sessions** - each `mirrord exec` you have running locally, with its target, port subscriptions, processes, mirrord version, and a live event stream (file ops, DNS, HTTP requests, outgoing connections).
- **Operator sessions** - a roll-up of every active mirrord session in your cluster, grouped by session key, with target, owner, namespace, and HTTP filter. Useful for seeing what your teammates have running before you start your own session, and for picking a session to ride on from the [mirrord browser extension](incoming-traffic/debug-from-browser.md).
- **Events** - the interception events the operator publishes for [`mirrord subscribe`](subscribe.md), for every session in the selected context at once. Needs mirrord `3.262.0` and operator `3.210.0` or newer.

The dashboard updates live over a WebSocket as sessions start and end.

## Prerequisites

- A recent mirrord CLI (`3.198.0` or newer). The subcommand is available on macOS and Linux.
- A working kubeconfig pointing at a cluster running the [mirrord operator](../managing-mirrord/operator.md), if you want to see operator sessions. Without an operator, the dashboard still works for your own local sessions.

## Quick start

```bash
mirrord ui
```

The CLI starts an HTTP + WebSocket server bound to localhost, generates a one-shot auth token, prints some details, and opens it in your default browser.

```text
* New mirrord session monitor started
* Server PID:
 -> ...

* Web UI:
 -> http://127.0.0.1:59281/auth?token=...
* API token:
-> x-auth-token: ...

* mirrord session monitor ready!
  -> log file: ...
```

The token is required on every API request, so there's no need to expose the dashboard beyond your machine. Each invocation generates a fresh token.

The server runs in the background on your machine. The logs for the server will be printed to a temporary file in the default temporary directory, and its file path is printed when the server is started. To stop the server, run:

```bash
mirrord ui stop
```

### Options

To pick a **different port**:

```bash
mirrord ui --port 9000
```

To **skip opening the browser** when running the command:

```bash
mirrord ui --no-browser
```

To open using a **different browser**:

```bash
BROWSER="firefox" mirrord ui
```

## Authentication

The token is high-entropy and is bound to the running `mirrord ui` process. The `?token=...` query parameter is only accepted on a dedicated `/auth` entry point - the URL the CLI prints and opens for you. Visiting `/auth?token=...` validates the token, sets a `mirrord_token` cookie scoped to that origin, and redirects to the dashboard, so the token never appears in ordinary navigation or API URLs afterwards. Every other route authenticates via that cookie. When sending requests directly to the server, you can also set the token in the header `x-auth-token`.

Stopping the server with `mirrord ui stop` invalidates the token; the next run mints a new one.

## Browser extension auto-configure

If you have the [mirrord browser extension](incoming-traffic/debug-from-browser.md) installed, opening the Web UI automatically tells the extension which `mirrord ui` daemon to talk to and hands it the session token. No copy/paste, no settings page. From there the extension's Sessions tab shows the same operator sessions you see in the Web UI, with a one-click join.

## Troubleshooting

- **"Failed to bind"** - another process is on the chosen port. Pass `--port <other>`.
- **Empty operator sessions list** - your kubeconfig context isn't pointing at a cluster with the mirrord operator installed, or your user doesn't have permission to read `MirrordOperator.status`. The local sessions panel still works.
- **Dashboard says "unauthorized"** - the token in the URL didn't match. Re-open the URL the CLI printed; don't reuse a URL from a previous run.
- **"Another session monitor is already running"** - the server is already running. If it isn't behaving as you expect, trying stopping it with `mirrord ui stop` or by killing it manually using its process ID, and then restarting.

## What's next?

- Use the [browser extension](incoming-traffic/debug-from-browser.md) to inject a header that matches an operator session's HTTP filter and route browser traffic to the corresponding local layer.
- Read [Managing Sessions](../sharing-the-cluster/sessions.md) for the operator-side view of the same sessions and how to forcibly stop one.
