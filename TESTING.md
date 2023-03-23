---
title: "Testing & Development"
description: "Setup environment for testing and developing mirrord"
date: 2022-06-15T08:48:45+00:00
lastmod: 2022-06-15T08:48:45+00:00
draft: false
images: []
menu:
  docs:
    parent: "testing"
weight: 110
toc: true
---


### Prerequisites

- [Rust](https://www.rust-lang.org/)
- [Nodejs](https://nodejs.org/en/), [Expressjs](https://expressjs.com/)
- [Python](https://www.python.org/), [Flask](https://flask.palletsprojects.com/en/2.1.x/)
- [Go](https://go.dev/)
- Kubernetes Cluster (local/remote)

### Setup a Kubernetes cluster

A minimal Kubernetes cluster can be easily setup locally using either of the following -

- [Minikube](https://minikube.sigs.k8s.io/)
- [Docker Desktop](https://www.docker.com/products/docker-desktop/)
- [Kind](https://kind.sigs.k8s.io/docs/user/quick-start/)

For the ease of illustration and testing, we will conform to using Docker Desktop for the rest of the guide.

#### Docker Desktop

Download [Docker Desktop](https://www.docker.com/products/docker-desktop/)

{{<figure src="mirrord-docker-desktop.png" alt="mirrord - Download Docker Desktop" class="white-background center large-width">}}

Enable Kubernetes in preferences, Apply and Restart

{{<figure src="mirrord-enable-kubernetes.png" alt="mirrord - Download Docker Desktop" class="white-background center large-width">}}

#### Preparing a cluster

Switch Kubernetes context to `docker-desktop`

```bash
kubectl config get-contexts
```

```bash
kubectl config use-context docker-desktop
```

From the root directory of the mirrord repository, create a new testing deployment and service:

```bash
kubectl apply -f sample/kubernetes/app.yaml
```

<details>
  <summary>sample/kubernetes/app.yaml</summary>

```yaml
apiVersion: apps/v1
kind: Deployment
metadata:
  name: py-serv-deployment
  labels:
    app: py-serv
spec:
  replicas: 1
  selector:
    matchLabels:
      app: py-serv
  template:
    metadata:
      labels:
        app: py-serv
    spec:
      containers:
        - name: py-serv
          image: ghcr.io/metalbear-co/mirrord-pytest:latest
          ports:
            - containerPort: 80
          env:
            - name: MIRRORD_FAKE_VAR_FIRST
              value: mirrord.is.running
            - name: MIRRORD_FAKE_VAR_SECOND
              value: "7777"

---
apiVersion: v1
kind: Service
metadata:
  labels:
    app: py-serv
  name: py-serv
spec:
  ports:
    - port: 80
      protocol: TCP
      targetPort: 80
      nodePort: 30000
  selector:
    app: py-serv
  sessionAffinity: None
  type: NodePort

```

</details>

Verify everything was created after applying the manifest

```bash
❯ kubectl get services
NAME         TYPE        CLUSTER-IP     EXTERNAL-IP   PORT(S)        AGE
kubernetes   ClusterIP   10.96.0.1      <none>        443/TCP        3h13m
py-serv      NodePort    10.96.139.36   <none>        80:30000/TCP   3h8m
❯ kubectl get deployments
NAME                 READY   UP-TO-DATE   AVAILABLE   AGE
py-serv-deployment   1/1     1            1           3h8m
❯ kubectl get pods
NAME                                 READY   STATUS    RESTARTS   AGE
py-serv-deployment-ff89b5974-x9tjx   1/1     Running   0          3h8m
```

### Build mirrord-agent Docker Image

```bash
docker build -t test . --file mirrord/agent/Dockerfile
```

```bash
❯ docker images
REPOSITORY                                     TAG       IMAGE ID       CREATED         SIZE
test                                           latest    5080c20a8222   2 hours ago     300MB
```

> **Note:** mirrord-agent is shipped as a container image as it creates a job with this image, providing it with
> elevated permissions on the same node as the impersonated pod.

### Build and run mirrord

| OSX | `cargo +nightly build --workspace --exclude mirrord-agent` |
| - | - |
| **Linux** | **`cargo +nightly build`** |

Run mirrord with a local process

Sample web server - `app.js`

<details>
  <summary>sample/node/app.mjs</summary>

```js
import { Buffer } from "node:buffer";
import { createServer } from "net";
import { open, readFile } from "fs/promises";

async function debug_file_ops() {
  try {
    const readOnlyFile = await open("/var/log/dpkg.log", "r");
    console.log(">>>>> open readOnlyFile ", readOnlyFile);

    let buffer = Buffer.alloc(128);
    let bufferResult = await readOnlyFile.read(buffer);
    console.log(">>>>> read readOnlyFile returned with ", bufferResult);

    const sampleFile = await open("/tmp/node_sample.txt", "w+");
    console.log(">>>>> open file ", sampleFile);

    const written = await sampleFile.write("mirrord sample node");
    console.log(">>>>> written ", written, " bytes to file ", sampleFile);

    let sampleBuffer = Buffer.alloc(32);
    let sampleBufferResult = await sampleFile.read(buffer);
    console.log(">>>>> read ", sampleBufferResult, " bytes from ", sampleFile);

    readOnlyFile.close();
    sampleFile.close();
  } catch (fail) {
    console.error("!!! Failed file operation with ", fail);
  }
}

// debug_file_ops();

const server = createServer();
server.on("connection", handleConnection);
server.listen(
  {
    host: "localhost",
    port: 80,
  },
  function () {
    console.log("server listening to %j", server.address());
  }
);

function handleConnection(conn) {
  var remoteAddress = conn.remoteAddress + ":" + conn.remotePort;
  console.log("new client connection from %s", remoteAddress);
  conn.on("data", onConnData);
  conn.once("close", onConnClose);
  conn.on("error", onConnError);

  function onConnData(d) {
    console.log("connection data from %s: %j", remoteAddress, d.toString());
    conn.write(d);
  }
  function onConnClose() {
    console.log("connection from %s closed", remoteAddress);
  }
  function onConnError(err) {
    console.log("Connection %s error: %s", remoteAddress, err.message);
  }
}

```

</details>

```bash
MIRRORD_AGENT_IMAGE=test MIRRORD_AGENT_RUST_LOG=debug RUST_LOG=debug target/debug/mirrord exec -c --target pod/py-serv-deployment-ff89b5974-x9tjx node sample/node/app.mjs
```
> **Note:** You need to change the pod name here to the name of the pod created on your system.


```
.
.
.
2022-06-30T05:14:01.592418Z DEBUG hyper::proto::h1::io: flushed 299 bytes
2022-06-30T05:14:01.657977Z DEBUG hyper::proto::h1::io: parsed 4 headers
2022-06-30T05:14:01.658075Z DEBUG hyper::proto::h1::conn: incoming body is empty
2022-06-30T05:14:01.661729Z DEBUG rustls::conn: Sending warning alert CloseNotify
2022-06-30T05:14:01.678534Z DEBUG mirrord_layer::sockets: getpeername hooked
2022-06-30T05:14:01.678638Z DEBUG mirrord_layer::sockets: getsockname hooked
2022-06-30T05:14:01.678713Z DEBUG mirrord_layer::sockets: accept hooked
2022-06-30T05:14:01.905378Z DEBUG mirrord_layer::sockets: socket called domain:30, type:1
2022-06-30T05:14:01.905639Z DEBUG mirrord_layer::sockets: bind called sockfd: 32
2022-06-30T05:14:01.905821Z DEBUG mirrord_layer::sockets: bind:port: 80
2022-06-30T05:14:01.906029Z DEBUG mirrord_layer::sockets: listen called
2022-06-30T05:14:01.906182Z DEBUG mirrord_layer::sockets: bind called sockfd: 32
2022-06-30T05:14:01.906319Z DEBUG mirrord_layer::sockets: bind: no socket found for fd: 32
2022-06-30T05:14:01.906467Z DEBUG mirrord_layer::sockets: getsockname called
2022-06-30T05:14:01.906533Z DEBUG mirrord_layer::sockets: getsockname: no socket found for fd: 32
2022-06-30T05:14:01.906852Z DEBUG mirrord_layer::sockets: listen: success
2022-06-30T05:14:01.907034Z DEBUG mirrord_layer::tcp: handle_listen -> listen Listen {
    fake_port: 51318,
    real_port: 80,
    ipv6: true,
    fd: 32,
}
Server listening on port 80
```

Send traffic to the Kubernetes Pod through the service

```bash
curl localhost:30000
```

Check the traffic was received by the local process

```bash
.
.
.
2022-06-30T05:17:31.877560Z DEBUG mirrord_layer::tcp: handle_incoming_message -> message Close(
    TcpClose {
        connection_id: 0,
    },
)
2022-06-30T05:17:31.877608Z DEBUG mirrord_layer::tcp_mirror: handle_close -> close TcpClose {
    connection_id: 0,
}
2022-06-30T05:17:31.877655Z DEBUG mirrord_layer::tcp: handle_incoming_message -> handled Ok(
    (),
)
2022-06-30T05:17:31.878193Z  WARN mirrord_layer::tcp_mirror: tcp_tunnel -> exiting due to remote stream closed!
2022-06-30T05:17:31.878255Z DEBUG mirrord_layer::tcp_mirror: tcp_tunnel -> exiting
OK - GET: Request completed
```

### Run E2E tests

Run Cargo test

```bash
cargo test --package tests
```
