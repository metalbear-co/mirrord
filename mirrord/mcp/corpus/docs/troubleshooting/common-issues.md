---
title: Common Issues
date: 2020-11-16T12:59:39.000Z
lastmod: 2020-11-16T12:59:39.000Z
draft: false
images: []
menu:
  docs:
    parent: faq
weight: 140
toc: true
tags:
  - oss
  - team
  - enterprise
description: Some common issues and workarounds.
---

## I've run my program with mirrord, but it seems to have no effect

There are currently two known cases where mirrord cannot load into the application's process:

1.  Statically linked binaries. Since mirrord uses the dynamic linker to load into the application's process, it cannot load if the binary is statically linked. Support for statically linked binaries is planned for the long term, but for now you would have to make sure your binaries are dynamically linked in order to run them with mirrord. With Go programs, for example, it is as simple as adding `import "C"` to your program code. If you don't want to add an import to your Go program, you can alternatively build a dynamically linked binary using `go build -ldflags='-linkmode external'`. In VSCode, this can be done by adding `"buildFlags": "-ldflags='-linkmode external'"` to your `launch.json`.

    On Linux, append `-ldflags="-s=false"` to instruct `go run` not to omit the symbol table and debug information required by mirrord.
2.  If you are running mirrord on MacOS and the executable you are running is protected by [SIP](https://en.wikipedia.org/wiki/System_Integrity_Protection) (the application you are developing wouldn't be, but the binary that is used to execute it, e.g. `bash` for a bash script, might be protected), mirrord might have trouble loading into it (mirrord can generally bypass SIP, but there are still some unhandled edge cases). If that is the case, you could try copying the binary you're trying to run to an unprotected directory (e.g. anywhere in your home directory), changing the IDE run configuration or the CLI to use the copy instead of the original binary, and trying again. If it still doesn't work, also remove the signature from the copy with:

    `sudo codesign --remove-signature ./<your-binary>`

    Please let us know if you're having trouble with SIP by opening an issue on [GitHub](https://github.com/metalbear-co/mirrord) or talking to us on [Slack](https://metalbear.com/slack).

Another reason that mirrord might seem not to work is if your remote pod has more than one container. mirrord works at the level of the container, not the whole pod. If your pod runs multiple containers, you need to make sure mirrord targets the correct one by by specifying it explicitly in the [target configuration](https://metalbear.com/mirrord/docs/config#target). Note that we filter out the proxy containers added by popular service meshes automatically.

## When running a Go program on Linux, DNS and outgoing traffic filters seem to have no effect

This can be caused when Go resolves DNS without going through libc. Build your Go binary with the following environment variable: `GODEBUG=netdns=cgo`

## I've run my [Turbo](https://turbo.build/) task with mirrord, but it seems to have no effect

When executing a task Turbo strips most of the existing process environment, including internal mirrord variables required during libc call interception setup. There are two alternative ways to solve this problem:

1. Explicitly tell Turbo to pass mirrord environment to the task. To do this, merge the snippet below into your `turbo.json`. You should be able to run the task like `mirrord exec turbo dev`.

```json
{
  "globalPassThroughEnv": ["MIRRORD_*", "LD_PRELOAD", "DYLD_INSERT_LIBRARIES"]
}
```

2. Invoke mirrord inside the Turbo task command line itself.

## Incoming traffic to the remote target doesn't reach my local process

This could happen because the local process is listening on a different port than the remote target. You can either change the local process to listen on the same port as the remote target (don't worry about the port being used locally by other processes), or use the [`port_mapping` configuration ](https://metalbear.com/mirrord/docs/config#feature.network.incoming)to map the local port to a remote port.

## The remote target stops receiving remote traffic, but it doesn't reach my local process either

This can happen in some clusters using a service mesh when stealing incoming traffic. You can use this configuration to fix it:

```json
{"agent": {"flush_connections": false}}
```

## My application is trying to read a file locally instead of from the cluster

mirrord has a list of path patterns that are read locally by default regardless of the configured fs mode. You can override this behavior in the configuration.

Here you can find all the pre-defined exceptions:

1. Paths that match the patterns defined here ([Unix](https://github.com/metalbear-co/mirrord/blob/main/mirrord/layer-lib/src/file/unix/read_local_by_default.rs) | [Windows](https://github.com/metalbear-co/mirrord/blob/main/mirrord/layer-lib/src/file/windows/read_local_by_default.rs)) are read locally by default.
2. Paths that match the patterns defined here ([Unix](https://github.com/metalbear-co/mirrord/blob/main/mirrord/layer-lib/src/file/unix/read_remote_by_default.rs) | [Windows](https://github.com/metalbear-co/mirrord/blob/main/mirrord/layer-lib/src/file/windows/read_remote_by_default.rs)) are read remotely by default when the mode is `localwithoverrides`.
3. Paths that match the patterns defined here ([Unix](https://github.com/metalbear-co/mirrord/blob/main/mirrord/layer-lib/src/file/unix/not_found_by_default.rs) | [Windows](https://github.com/metalbear-co/mirrord/blob/main/mirrord/layer-lib/src/file/windows/not_found_by_default.rs)) under the running user's home directory will be failed to be found by default when the mode is not `local`.

In order to override that settings for a path or a pattern, add it to the appropriate set:

1. `feature.fs.read_only` if you want read operations to that path to happen remotely, but write operations to happen locally.
2. `feature.fs.read_write` if you want read and write operations to that path to happen remotely.
3. `feature.fs.local` if you want read and write operations to that path to happen locally.
4. `feature.fs.not_found` if you want the application to "think" that file does not exist.

## My local process fails to resolve the domain name of a Kubernetes service in the same cluster

If you've set `feature.fs.mode` to `local`, try changing it to `localwithoverrides`.

When the `local` mode is set, all files will be opened locally. This might prevent your process from resolving cluster-internal domain names correctly, because it can no longer read Kubelet-generated configuration files like `/etc/resolv.conf`. With `localwithoverrides`, such files are read from the remote pod instead.

## Old mirrord agent pods are not getting deleted after the mirrord run is completed

If an agent pod's status is `Running`, it means mirrord is probably still running locally as well. Once you terminate the local process, the agent pod's status should change to `Completed`.

On clusters with Kubernetes version v1.23 or higher, agent pods are [automatically cleaned up](https://kubernetes.io/docs/concepts/workloads/controllers/ttlafterfinished/) immediately (or after a [configurable TTL](https://metalbear.com/mirrord/docs/config#agent.ttl)). If your cluster is v1.23 or higher and mirrord agent pods are not being cleaned up automatically, [please open an issue on GitHub](https://github.com/metalbear-co/mirrord/issues/new?assignees=\&labels=bug\&projects=\&template=bug_report.yml\&title=Agent%20pods%20lingering%20after%20completion). As a temporary solution for cleaning up completed agent pods manually, you can run:

```shell
kubectl delete jobs --selector=app=mirrord --field-selector=status.successful=1
```

## My local process gets permission (EACCESS) error on file access or DNS can't resolve

If your cluster is running on Bottlerocket or has SELinux enabled, please try enabling the `privileged` flag in the agent configuration:

```json
{
  "agent": {
    "privileged": true
  }
}
```

## `mirrord operator status` fails with `503 Service Unavailable` on GKE

If private networking is enabled, it is likely due to firewall rules blocking the mirrord operator's API service from the API server. To fix this, add a firewall rule that allows your cluster's master nodes to access TCP port 443 in your cluster's pods. Please refer to the [GCP docs](https://cloud.google.com/kubernetes-engine/docs/how-to/private-clusters#add_firewall_rules) for information.

## My local process encounters unexpected certificate validation errors

When running processes locally versus in a container within Kubernetes, some languages handle certificate validation differently. For instance, a Go application on macOS will use the macOS Keychain for certificate validation, whereas the same application in a container will use different API calls. This discrepancy can lead to unexpected certificate validation errors when using tools like mirrord.

A specific issue with Go can be found [here](https://github.com/golang/go/issues/51991), where Go encounters certificate validation errors due to certain AWS services serving certificates that are deemed invalid by the macOS Keychain, but not by Go’s certificate validation in other environments.

To work around this issue (on macOS), you can use the following mirrord configuration:

```json
{
   "experimental": {"trust_any_certificate": true}
}
```

This configuration would make any certificate trusted for the process.

Other alternatives are to either disable certificate validation in your application or import the problematic certificate (or its root CA) into your macOS Keychain. For guidance on how to do this, refer to this [Apple support article](https://support.apple.com/guide/keychain-access/change-the-trust-settings-of-a-certificate-kyca11871/mac).

## Agent connection fails or drops when using an ephemeral agent with a service mesh

When running the agent as an [ephemeral container](https://metalbear.com/mirrord/docs/config#agent.ephemeral), the agent shares the network stack with the target pod. This means that incoming connections to the agent are handled by the service mesh, which might drop it for various reasons (lack of TLS, not HTTP, etc.) To work around that, set the agent.port to be static using `agent.port` in values.yaml when installing the operator, then add a port exclusion for the agent port in your service mesh's configuration. For example, if you use Istio and have set the agent port to 5000, you can add the following annotation for exclusion:

```
traffic.sidecar.istio.io/excludeInboundPorts: '50000'
```

## My mirrord fails to load `mirrord-layer` dynamic library with Carbon Black installed

When running mirrord, you might see an error indicating that it failed to load its required dynamic library, for example:

```shell
dyld[68792]: terminating because inserted dylib '/tmp/11832501046814586937-libmirrord_layer.dylib' could not be loaded: tried: '/tmp/11832501046814586937-libmirrord_layer.dylib' (code signing blocked mmap() of '/private/tmp/11832501046814586937-libmirrord_layer.dylib'), '/System/Volumes/Preboot/Cryptexes/OS/tmp/11832501046814586937-libmirrord_layer.dylib' (no such file), '/tmp/11832501046814586937-libmirrord_layer.dylib' (code signing blocked mmap() of '/private/tmp/11832501046814586937-libmirrord_layer.dylib'), '/private/tmp/11832501046814586937-libmirrord_layer.dylib' (code signing blocked mmap() of '/private/tmp/11832501046814586937-libmirrord_layer.dylib'), '/System/Volumes/Preboot/Cryptexes/OS/private/tmp/11832501046814586937-libmirrord_layer.dylib' (no such file), '/private/tmp/11832501046814586937-libmirrord_layer.dylib' (code signing blocked mmap() of '/private/tmp/11832501046814586937-libmirrord_layer.dylib')
```

This error can occur on systems with Carbon Black Endpoint Detection and Response (EDR) software installed. Carbon Black enforces strict controls over code execution, including blocking attempts to load dynamic libraries (shared objects) that are unsigned or located in potentially untrusted or ephemeral directories like `/tmp`.

mirrord, by design, extracts a temporary dynamic library at runtime — the `mirrord-layer` — which is written to a randomized path in `/tmp` (for example, `/tmp/11832501046814586937-libmirrord_layer.dylib`). This library is then dynamically injected into your local application so mirrord can intercept and redirect its network traffic transparently to a remote Kubernetes environment.

Carbon Black typically does not trust binaries or dynamic libraries residing in ephemeral directories like /tmp. As a result, Carbon Black treats the loading attempt as potentially suspicious and blocks it, which prevents the library from being injected and disrupts mirrord’s operation.

To resolve this problem, you can create an explicit exclusion policy in Carbon Black to permit loading of the `mirrord-layer` dynamic library. Your policy should permit loading dynamic libraries from a specific predictable directory, such as `/private/tmp/mirrord/**`.

## I can't mirrord to work with Remix/Vite

Remix and Vite use the `NODE_ENV` environment variable to determine the runtime configuration. Typically, your remote workload will have `NODE_ENV` set to `staging` or `production`, while locally you might run `npm run dev`, resulting in a mix of production, staging, and development settings across your Node, Vite, or Remix code. 

To ensure consistent behavior, you can override the remote `NODE_ENV` value by adding the following to your mirrord configuration:

```json
{
  "feature": {
    "env": {
      "override": {
        "NODE_ENV": "development"
      }
    }
  }
}
```

## I’m running a Next.js server with Nx and mirrord, but it doesn’t behave as expected

If your Next.js app is managed by Nx and uses a custom server (for example via `@nx/next:custom-server`), the local process may crash or behave incorrectly because mirrord mirrors conflicting environment variables from the remote target. The remote target (running in production) usually has variables set specifically for the container environment, such as:
- `NODE_ENV=production`
- `NX_NEXT_DIR=/app` (or another container-specific path)

When mirrord injects these values into your local process, they override the values that Nx automatically generates for your local environment. This causes the server to run in production mode or look for build artifacts in directories that do not exist on your machine.

To fix this, use `feature.env.exclude` to prevent mirrord from importing these specific variables. This allows your local Nx process to set the correct local values (e.g., dist/apps/my-service) natively.

```json
{
  "feature": {
    "env": {
      "exclude": [
        "NODE_ENV",
        "NX_NEXT_DIR"
      ]
    }
  }
}
```
Then run your Nx target with mirrord:
```
mirrord exec -f .mirrord/mirrord.json -- pnpm nx serve my-service
```
By excluding `NX_NEXT_DIR` and `NODE_ENV`, you stop the remote environment from interfering with your local build configuration. This ensures the custom server uses the correct local build path and runs in development mode, while mirrord continues to mirror traffic and other environment variables.

---

## Didn't find your issue here?

- Search or ask on our [Slack community](https://metalbear.com/slack)
- Open an issue on [GitHub](https://github.com/metalbear-co/mirrord/issues/new?assignees=&labels=bug&projects=&template=bug_report.yml)
- Email us at [hi@metalbear.com](mailto:hi@metalbear.com)
