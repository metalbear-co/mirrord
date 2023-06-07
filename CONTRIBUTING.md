# Contributing

Before submitting pull request features, please discuss them with us first by opening an issue or a discussion.
We welcome new/junior/starting developers. Feel free to join to our [Discord channel](https://discord.gg/metalbear) for help and guidance.

If you would like to start working on an issue, please comment on the issue on GitHub, so that we can assign you to that
issue.

# Contents

- [Contents](#contents)
  - [Getting Started](#getting-started)
  - [Debugging mirrord](#debugging-mirrord)
  - [New Hook Guidelines](#new-hook-guidelines)

# Getting Started

The following guide details the steps to setup a local development environment for mirrord and run the E2E tests.

### Prerequisites

- [Rust](https://www.rust-lang.org/)
- [NodeJS](https://nodejs.org/en/), [ExpressJS](https://expressjs.com/)
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
 
Make sure you're [logged in to GHCR](https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry).

Then run:
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

The Kubernetes resources created by the E2E tests are automatically deleted when the test exits. However, you can preserve resources from failed tests for debugging. To do this, set the `MIRRORD_E2E_PRESERVE_FAILED` variable to any value.

```bash
MIRRORD_E2E_PRESERVE_FAILED=y cargo test --package tests
```

All test resources share a common label `mirrord-e2e-test-resource=true`. To delete them, simply run:

```bash
kubectl delete namespaces,deployments,services -l mirrord-e2e-test-resource=true
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

To build this project, you will first need a [Protocol Buffer Compiler](https://grpc.io/docs/protoc-installation/) installed.

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

# Debugging mirrord

## mirrord console

Debugging mirrord can get hard since we're running from another app flow, so the fact we're debugging might affect the program and make it unusable/buggy (due to sharing stdout with scripts/other applications).

The recommended way to do it is to use `mirrord-console`. It is a small application that receives log information from different mirrord instances and prints it, controlled via `RUST_LOG` environment variable.

To use mirrord console, run it:
`cargo run --bin mirrord-console`

Then run mirrord with the environment variable:
`MIRRORD_CONSOLE_ADDR=127.0.0.1:11233`

## Retrieving Agent Logs

By default, the agent's pod will complete and disappear shortly after the agent exits. In order to be able to retrieve 
the agent's logs after it crashes, set the agent's pod's TTL to a comfortable number of seconds. This configuration can
be specified either as a command line argument (`--agent-ttl`), environment variable (`MIRRORD_AGENT_TTL`), or in a
configuration file:
```toml
[agent]
ttl = 30
```

Then, when running with some reasonable TTL, you can retrieve the agent log like this:
```bash
kubectl logs -l app=mirrord --tail=-1 | less -R
```

This will retrieve the logs from all running mirrord agents, so it is only useful when just one agent pod exists.

If there are currently multiple agent pods running on your cluster, you would have to run
```bash
kubectl get pods
```
and find the name of the agent pod you're interested in, then run

```bash
kubectl logs <YOUR_POD_NAME> | less -R
```

where you would replace `<YOUR_POD_NAME>` with the name of the pod.

## Debugging the vscode extension
In order to debug the vscode extension, first [build the mirrord binary](#build-and-run-mirrord). Then run:
```bash
cd vscode-ext
npm install
npm run compile
```
Now you can just open the extension's code in vscode and run, using the "Launch Extension" run configuration. Another vscode window will start. You can set breakpoints
in the extension's code in the first window, and use the extension in the second window to reach the breakpoints.
When in debug mode, the extension will automatically use the debug mirrord binary.

If you want to see the layer's logs, [use the console](#mirrord-console) by setting
```json
            "env": {
                "RUST_LOG": "warn,mirrord=trace",
                "MIRRORD_CONSOLE_ADDR": "127.0.0.1:11233"
            }

```
in the launch configuration of the second window.

## Debugging the JetBrains plugin

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

Now open the extension's code in an IntelliJ IDEA. Create a new Gradle run configuration with a `runIde` task.
Running this configuration in debug will open a new IDE window.
You can set breakpoints in the extension's code in the first window, and use the extension in the second window to reach the breakpoints.

You can control which IDE is opened with a `PLATFORMTYPE` environment variable. For example, set `PLATFORMTYPE=IU` for IntelliJ IDEA Ultimate.
# New Hook Guidelines

Adding a feature to mirrord that introduces a new hook (file system, network) can be tricky and there are a lot of edge cases we might need to cover. 

In order to have a more structured approach, here’s the flow you should follow when working on such a feature.

1. Start with the use case. Write an example use case of the feature, for example “App needs to read credentials from a file”.
2. Write a minimal app that implements the use case - so in the case of our example, an app that reads credentials from a file. Start with either Node or Python as those are most common.
3. Figure out what functions need to be hooked in order for the behavior to be run through the mirrord-agent instead of locally. We suggest using `strace`.
4. Write a doc on how you would hook and handle the cases, for example:
    1. To implement use case “App needs to read credentials from a file*”
    2. I will hook `open` and `read` handling calls only with flags O_RDONLY.
    3. Once `open` is called, I’ll send a blocking request to the agent to open the remote file, returning the return code of the operation. 
    4. Create an fd using `memfd`. The result will be returned to the local app, and if successful we’ll save that fd into a HashMap that matches between local fd and remote fd/identifier. 
    5. When `read` is called, I will check if the fd being read was previously opened by us, and if it is we’ll send a blocking `read` request to the agent. The result will be sent back to the caller.
    6. And so on.
5. This doc should go later on to our mirrord docs for advanced developers so people can understand how stuff works
6. After approval of the implementation, you can start writing code, and add relevant e2e tests.