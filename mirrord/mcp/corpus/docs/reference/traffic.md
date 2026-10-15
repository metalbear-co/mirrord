---
title: Network Traffic
date: 2022-08-08T08:48:45.000Z
lastmod: 2022-08-08T08:48:45.000Z
draft: false
images: []
menu:
  docs:
    parent: reference
weight: 140
toc: true
tags:
  - oss
  - team
  - enterprise
description: Reference to working with network traffic with mirrord
---

## Incoming

mirrord lets users debug incoming network traffic by mirroring or stealing the traffic sent to the remote pod.

### Mirroring

mirrord's default configuration is to mirror incoming TCP traffic from the remote pod, i.e. run the local process in the context of cloud environment without disrupting incoming traffic for the remote pod. Any responses by the local process to the mirrored requests are dropped, and so whatever application is running on the remote pod continues to operate normally while the traffic is mirrored to the local process.

Example - `user-service` a simple Kubernetes deployment and service that stores registered users.

```bash
bigbear@metalbear:~/mirrord$ minikube service list
|-------------|-------------------|--------------|---------------------------|
|  NAMESPACE  |       NAME        | TARGET PORT  |            URL            |
|-------------|-------------------|--------------|---------------------------|
| default     | kubernetes        | No node port |
| default     | user-service      |           80 | http://192.168.49.2:32000 |
| kube-system | kube-dns          | No node port |
|-------------|-------------------|--------------|---------------------------|

bigbear@metalbear:~/mirrord-demo$ curl -X POST -H "Content-type: application/json" -d "{\"Name\" : \"Metal\", \"Last\" : \"Bear\"}" http://192.168.49.2:32000/user
{"Last":"Bear","Name":"Metal"}

bigbear@metalbear:~/mirrord-demo$ curl http://192.168.49.2:31000/index.html
<html> <head>USERS</head><body><h1> MetalBear Users</h1><p>[{"Last":"Bear","Name":"Metal"}]</p></body></html>
```

```bash
bigbear@metalbear:~/mirrord$ kubectl get pods
NAME                                        READY   STATUS    RESTARTS      AGE
metalbear-bff-deployment-597cb4f957-485t5   1/1     Running   1 (15h ago)   16h
metalbear-deployment-85c754c75f-6k7mg       1/1     Running   1 (15h ago)   16h
```

To mirror traffic from remote services to the local development environment, run the services locally with mirrord

#### Window 1

```bash
bigbear@metalbear:~/mirrord-demo$ ../mirrord/target/debug/mirrord exec -c
--no-outgoing --target pod/metalbear-deployment-85c754c75f-6k7mg python3
user-service/service.py 
 * Serving Flask app 'service' (lazy loading)
 * Environment: production
   WARNING: This is a development server. Do not use it in a production deployment.
   Use a production WSGI server instead.
 * Debug mode: off
 * Running on all addresses (0.0.0.0)
   WARNING: This is a development server. Do not use it in a production deployment.
 * Running on http://127.0.0.1:33695
 * Running on http://172.16.0.4:33695 (Press CTRL+C to quit)
 127.0.0.1 - - [08/Sep/2022 15:34:34] "GET /users HTTP/1.1" 200
// ^ Received mirrored traffic from the remote pod
```

#### Window 2

```bash
bigbear@metalbear:~/mirrord-demo$ curl http://192.168.49.2:32000/users
[{"Last":"Bear","Name":"Metal"}]
```                                                                                                                                             

### Stealing

mirrord can steal network traffic, i.e. intercept it and send it to the local process instead of the remote pod. This means that all incoming traffic is only handled by the local process.

Example - running `user-service` with mirrord and `--steal` on:


#### Window 1

```bash
bigbear@metalbear:~/mirrord-demo$ ../mirrord/target/debug/mirrord exec -c 
--steal --target pod/metalbear-deployment-85c754c75f-6k7mg
python3 user-service/service.py 
 * Serving Flask app 'service' (lazy loading)
 * Environment: production
   WARNING: This is a development server. Do not use it in a production deployment.
   Use a production WSGI server instead.
 * Debug mode: off
 * Running on all addresses (0.0.0.0)
   WARNING: This is a development server. Do not use it in a production deployment.
 * Running on http://127.0.0.1:35215
 * Running on http://172.16.0.4:35215 (Press CTRL+C to quit) 
 127.0.0.1 - - [08/Sep/2022 15:48:40] "GET /users HTTP/1.1" 200 -
 127.0.0.1 - - [08/Sep/2022 15:50:40] "POST /user HTTP/1.1" 200 -
 127.0.0.1 - - [08/Sep/2022 15:50:55] "GET /users HTTP/1.1" 200 -
 127.0.0.1 - - [08/Sep/2022 16:57:51] "POST /user HTTP/1.1" 200 -
 127.0.0.1 - - [08/Sep/2022 16:57:54] "GET /users HTTP/1.1" 200 -
 ^Cbigbear@metalbear:~/mirrord-demo$ 
```

#### Window 2

```bash
// Before running mirrord with `--steal`
bigbear@metalbear:~/mirrord-demo$ curl http://192.168.49.2:32000/users
[{"Last":"Bear","Name":"Metal"}]

// After running with mirrord and `--steal` - local process responds
 instead of the remote
bigbear@metalbear:~/mirrord-demo$ curl http://192.168.49.2:32000/users
[]
bigbear@metalbear:~/mirrord-demo$ curl -X POST -H 
"Content-type: application/json" 
-d "{\"Name\" : \"Mehul\", \"Last\" : \"Arora\"}" http://192.168.49.2:32000/user

{"Last":"Arora","Name":"Mehul"}
bigbear@metalbear:~/mirrord-demo$ curl http://192.168.49.2:32000/users
[{"Last":"Arora","Name":"Mehul"}]
bigbear@metalbear:~/mirrord-demo$ curl -X POST -H 
"Content-type: application/json" 
-d "{\"Name\" : \"Alex\", \"Last\" : \"C\"}" http://192.168.49.2:32000/user
{"Last":"C","Name":"Alex"}
bigbear@metalbear:~/mirrord-demo$ curl http://192.168.49.2:32000/users
[{"Last":"Arora","Name":"Mehul"},{"Last":"C","Name":"Alex"}]

// After sending SIGINT to the local process
bigbear@metalbear:~/mirrord-demo$ curl http://192.168.49.2:32000/users
[{"Last":"Bear","Name":"Metal"}]
```

**Filtering Incoming Traffic by HTTP Headers**

mirrord lets you specify a regular expression to filter HTTP requests with. When specified, all the headers of each HTTP request that arrives at the remote target are checked against the regular expression. If any of the headers match, the request will be stolen, otherwise, it will be sent to the remote target. For each `Header-Name`, `Header-Value` pair, your regular expression will be matched against `Header-Name: Header-Value`. You can filter on any header, including the W3C trace propagation headers `baggage` or `tracestate` — for example `^baggage: .*mirrord-session=alice.*` (if you use OpenTelemetry or another tracing library, these headers are often propagated automatically). The regular expression is evaluated with the [fancy\_regex](https://docs.rs/fancy-regex/0.10.0/fancy_regex/index.html) rust crate.

**Specifying a Filter**

An HTTP filter can be specified in the mirrord configuration file by setting the incoming mode to `steal` and specifying a filter in [`feature.network.incoming.http_filter.header_filter`](https://metalbear.com/mirrord/docs/config#feature.network.incoming.http_filter.header_filter) or [`feature.network.incoming.http_filter.path_filter`](https://metalbear.com/mirrord/docs/config#feature.network.incoming.http_filter.path_filter).

**Setting Custom HTTP Ports**

The configuration also allows specifying custom HTTP ports under `feature.network.incoming.http_filter.ports`. By default, ports 80 and 8080 are used as HTTP ports if a filter is specified, which means that the mirrord agent checks each new connection on those ports for HTTP, and if the connection has valid HTTP messages, they are filtered with the header filter.

## Outgoing

mirrord's outgoing traffic feature intercepts outgoing requests from the local process and sends them through the remote pod instead. Responses are then routed back to the local process. A simple use case of this feature is enabling the local process to make an API call to another service in the k8s cluster, for example, a database read/write.

For UDP, outgoing traffic is currently only intercepted and forwarded by mirrord if the application binds a non-0 port and makes a `connect` call on the socket before sending out messages. Outgoing TCP and UDP forwarding are both enabled by default. It can be controlled individually for TCP and UDP or disabled altogether (see `mirrord exec --help`).

> **Note:** If the handling of incoming requests by your app involves outgoing API calls to other services, and mirrord is configured to mirror incoming traffic, then it might be the case that both the remote pod and the local process (which receives mirrored requests) make an outgoing API call to another service for the same incoming request. If that call is a write operation to a database, this could lead e.g. to duplicate lines in the database. You can avoid such an effect by switching from [traffic mirroring](traffic.md#mirroring) to [traffic stealing](traffic.md#stealing) mode. Alternatively, if the service your application makes an API call to is only reachable from within the Kubernetes cluster, you can disable outgoing traffic forwarding, which would make it impossible for your local process to reach that service.

## DNS Resolution

mirrord can resolve DNS queries in the context of the remote pod

Example - calling `getaddrinfo` to see if the query is resolved:

```bash
Python 3.8.10 (default, Jun 22 2022, 20:18:18) 
[GCC 9.4.0] on linux
Type "help", "copyright", "credits" or "license" for more information.
>>> import socket
>>> socket.getaddrinfo('localhost', None)
2022-09-08T17:37:50.735532Z  INFO mirrord_layer::socket::ops: getaddrinfo -> result Ok(
    0x00007f5508004760,
)
[(<AddressFamily.AF_INET6: 10>, <SocketKind.SOCK_STREAM: 1>, 6, '', ('::7074:e00d:557f:0', 0, 0, 97)), (<AddressFamily.AF_INET6: 10>, <SocketKind.SOCK_DGRAM: 2>, 17, '', ('::', 0, 0, 0)), (<AddressFamily.AF_INET6: 10>, <SocketKind.SOCK_RAW: 3>, 0, '', ('::90bf:f401:0:0', 0, 0, 245652448)), (<AddressFamily.AF_INET: 2>, <SocketKind.SOCK_STREAM: 1>, 6, '', ('127.0.0.1', 0)), (<AddressFamily.AF_INET: 2>, <SocketKind.SOCK_DGRAM: 2>, 17, '', ('127.0.0.1', 0)), (<AddressFamily.AF_INET: 2>, <SocketKind.SOCK_RAW: 3>, 0, '', ('127.0.0.1', 0))]
>>> socket.getaddrinfo('user-service', None)
2022-09-08T17:38:17.556108Z  INFO mirrord_layer::socket::ops: getaddrinfo -> result Ok(
    0x00007f5508003610,
)
[(<AddressFamily.AF_INET: 2>, <SocketKind.SOCK_STREAM: 1>, 6, '', ('10.106.158.180', 0)), (<AddressFamily.AF_INET: 2>, <SocketKind.SOCK_DGRAM: 2>, 17, '', ('10.106.158.180', 0)), (<AddressFamily.AF_INET: 2>, <SocketKind.SOCK_RAW: 3>, 0, '', ('10.106.158.180', 0))]
>>> 
```
