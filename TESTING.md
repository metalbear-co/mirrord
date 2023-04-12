# Testing Guide

### Prerequisites

- [Rust](https://www.rust-lang.org/)
- [Nodejs](https://nodejs.org/en/), [Expressjs](https://expressjs.com/)
- [Python](https://www.python.org/), [Flask](https://flask.palletsprojects.com/en/2.1.x/), [FastAPI](https://fastapi.tiangolo.com/)
- [Go](https://go.dev/)
- Kubernetes Cluster (local/remote)

### Setup a Kubernetes cluster

For E2E tests and testing mirrord manually you will need a working Kubernetes cluster. A minimal cluster can be easily setup locally using either of the following:

- [Minikube](https://minikube.sigs.k8s.io/)
- [Docker Desktop](https://www.docker.com/products/docker-desktop/)
- [Kind](https://kind.sigs.k8s.io/docs/user/quick-start/)

For the ease of illustration and testing, we will conform to using Minikube for the rest of the guide.

### Minikube

Download [Minikube](https://minikube.sigs.k8s.io/)

Start a Minikube cluster with preferred driver. Here we will use the Docker driver.

```bash
minikube start --driver=docker
```

### Prepare a cluster

 Build mirrord-agent Docker Image.

```bash
docker buildx build -t test . --file mirrord/agent/Dockerfile
```

```bash
❯ docker images
REPOSITORY                                     TAG       IMAGE ID       CREATED         SIZE
test                                           latest    5080c20a8222   2 hours ago     300MB
```

> **Note:** mirrord-agent is shipped as a container image as mirrord creates a job with this image, providing it with
> elevated permissions on the same node as the impersonated pod.

Load mirrord-agent image to Minikube.

```bash
minikube image load test
```

Switch Kubernetes context to `minikube`.

```bash
kubectl config get-contexts
```

```bash
kubectl config use-context minikube
```

## E2E Tests

The E2E tests create Kubernetes resources in the cluster that kubectl is configured to use and then run sample apps 
with the mirrord CLI. The mirrord CLI spawns an agent for the target on the cluster, and runs the test app, with the 
layer injected into it. Some test apps need to be compiled before they can be used in the tests 
([this should be automated in the future](https://github.com/metalbear-co/mirrord/issues/982)).

The basic command to run the E2E tests is:
```bash
cargo test --package tests
```

However, when running on macOS a universal binary has to be created first:
```bash
scripts/build_fat_mac.sh
```

And then in order to use that binary in the tests, run the tests like this:
```bash
MIRRORD_TESTS_USE_BINARY=../target/universal-apple-darwin/debug/mirrord cargo test -p tests
```

### Cleanup

The Kubernetes resources created by an E2E test are automatically deleted when the test exists successfully. However, failed tests by default don't delete their resources to allow debugging. You can force resource deletion by setting a `MIRRORD_E2E_FORCE_CLEANUP` variable to any value.

```bash
MIRRORD_E2E_FORCE_CLEANUP=y cargo test --package tests
```

All test resources share a common label `MIRRORD_E2E_TEST_RESOURCE=true`. To delete them, simply run:

```bash
kubectl delete namespaces,deployments,services -l MIRRORD_E2E_TEST_RESOURCE=true
```


## Integration Tests

The layer's integration tests test the hooks and their logic without actually using a Kubernetes cluster and spawning
an agent. The integration tests usually execute a test app and load the dynamic library of the layer into them. The 
tests set the layer to connect to a TCP/IP address instead of spawning a new agent. The tests then have to simulate the 
agent - they accept the layer's connection, receive the layers messages and answer them as the agent would.

Since they do not need to create Kubernetes resources and spawn agents, the integration tests complete much faster than 
the E2E tests, especially on GitHub Actions.

Therefore, whenever possible we create integration tests, and only resort to E2E tests when necessary.

### Running the Integration Tests

Some test apps need to be compiled before they can be used in the tests
([this should be automated in the future](https://github.com/metalbear-co/mirrord/issues/982)).

The basic command to run the integration tests is:
```bash
cargo test --package mirrord-layer
```

However, when running on macOS a dylib has to be created first:
```bash
scripts/build_fat_mac.sh
```

And then in order to use that dylib in the tests, run the tests like this:
```bash
MIRRORD_TEST_USE_EXISTING_LIB=../../target/universal-apple-darwin/debug/libmirrord_layer.dylib cargo test -p mirrord-layer
```

## Testing mirrord manually with a sample app.

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

### Build and run mirrord

#### macOS
```bash
scripts/build_fat_mac.sh
```

#### Linux
```bash
cargo build
```

#### Run mirrord with a local process

Sample web server - `app.js` (present at `sample/node/app.mjs` in the repo)

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
RUST_LOG=debug target/debug/mirrord exec -i test -l debug -c --target pod/py-serv-deployment-ff89b5974-x9tjx node sample/node/app.mjs
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
curl $(minikube service py-serv --url)
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

## Building the vscode extension
Please note you don't need to create a .vsix file and install it, in order to debug the extension. See
[the debugging guide](DEBUGGING.md#debugging-the-vscode-extension).

If for some reason you still want to build the extension, here are the instructions:

```commandline
cd vscode-ext
```

If you haven't built the mirrord binary yet, [do it now](#build-and-run-mirrord). Then copy the binary into the
vscode-ext directory. On macOS that would be:
```commandline
cp ../target/universal-apple-darwin/debug/mirrord .
```
(Change the path on Linux to wherever the binary is)

Then run
```bash
npm install
npm run compile
npm run package
```

You should see something like
```text
DONE  Packaged: /Users/you/Documents/projects/mirrord/vscode-ext/mirrord-3.34.0.vsix (11 files, 92.14MB)
```

## Building the JetBrains plugin

First, [build the mirrord binaries](#build-and-run-mirrord) if not yet built. Then:

```bash
cd intellij-ext
```

### On macOS

```bash
cp ../target/universal-apple-darwin/debug/libmirrord_layer.dylib .
touch libmirrord_layer.so
cp ../target/universal-apple-darwin/debug/mirrord bin/macos/
```

### On Linux x86-64

```bash
cp ../target/debug/libmirrord_layer.so .
touch libmirrord_layer.dylib
cp ../target/debug/mirrord bin/linux/x86-64/mirrord
```

### In order to "cross build"
Just include all the binaries:
```text
libmirrord_layer.dylib
libmirrord_layer.so
bin/macos/mirrord
bin/linux/x86-64/mirrord
bin/linux/arm64/mirrord
```

Then build the plugin:
```bash
./gradlew buildPlugin
```