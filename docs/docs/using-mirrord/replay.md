---
title: "Replaying Requests"
tags:
  - alpha
  - oss
  - team
  - enterprise

description: "Use mirrord replay to send an HTTP request that your session received to your local application again."
---

# Replaying Requests

`mirrord replay` sends an HTTP request that your session received to your local application again. Trigger a request once, then replay it after each code change, without running the services in front of yours again.

A replay goes straight from mirrord to your local application. The client that made the original request is not involved, so none of its timeouts run while you hold the request on a breakpoint.

{% hint style="info" %}
Needs mirrord `3.292.0` or newer.
{% endhint %}

## Which requests are kept

A session keeps the HTTP requests that it steals or mirrors, with their headers and bodies. It keeps them in memory on your machine, and only while the session runs. A request is kept from the moment it arrives, so it stays available when its client gives up before your application answers.

A session has these limits:

* A session keeps its latest 100 requests, and the latest 5 replays of each.
* A request with a body larger than 32 KiB is listed, but can't be replayed.
* A request that upgraded the connection, such as a WebSocket handshake, is listed, but can't be replayed.
* A request whose body the session did not get whole is listed, but can't be replayed. This happens while the body is still arriving, when the client stopped sending it, or when your application answered before all of it arrived.
* Requests from Kubernetes health probes are not kept.

The session needs its local API, which is on by default. The [`api`](https://metalbear.com/mirrord/docs/config/options#root-api) option controls it.

## Listing requests

```bash
mirrord replay list
```

```
 ID  METHOD  PATH                  STATUS  TOOK    AGE  REPLAYS
 1   POST    /internal/v1/charges  500     412 ms  11s  0
```

`STATUS` and `TOOK` describe how your application answered the request when it arrived from the cluster. They show `-` while your application has not answered.

## Replaying a request

```bash
mirrord replay send 1
```

The command waits for your application and prints its response, next to the status of the response to the original request. It asks your application for an uncompressed response, and prints up to 32 KiB of the body:

```
Replay 1 of request 1: 200 in 38 ms   (original: 500 in 412 ms)
content-type: application/json

{"id":"ch_8Kq2","status":"succeeded"}
```

To change the request before sending it, set or remove headers. Both flags can be repeated. The `content-length` and `transfer-encoding` headers can't be changed:

```bash
mirrord replay send 1 --header 'x-tenant: blue' --remove-header authorization
```

Calls that your application makes while it handles a replay still reach the cluster, the same way they do for any other request.

A replay is sent once. mirrord only sends it again when your application can't have received it, for example while your application restarts. A replay waits for the response headers as long as your application takes. It then reads the body for at most 30 seconds, and stops earlier when nothing arrives for 5 seconds, so it only shows the start of a body that never ends, such as an event stream.

## Showing a request

```bash
mirrord replay show 1
```

This prints the headers and body of the request, the status of the response to the original request, and the status of its latest replays. Values of headers and query parameters that usually carry secrets, such as `authorization`, `cookie`, and `token`, are printed as `[REDACTED]`. A replay still sends the original values. Bodies are printed as they are.

## Replaying from the Local UI

The session view of the [Local UI](local-ui.md) has a **Replay** tab. It lists the requests of the session, and shows each request with its replays and their responses. Select a request and press **Replay**.

## Choosing a session

While one local session is running, `mirrord replay` uses it. When several are running, pass the session ID that `mirrord session list` prints:

```bash
mirrord replay list --session-id <SESSION_ID>
```

## Output for scripts and AI agents

Pass `--format json` to any `mirrord replay` command to print JSON:

```bash
mirrord replay send 1 --format json
```
