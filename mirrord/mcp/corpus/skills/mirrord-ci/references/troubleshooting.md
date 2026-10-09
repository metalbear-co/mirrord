# Common Issues

> Source: https://github.com/metalbear-co/docs/tree/main/docs/troubleshooting

## I've run my program with mirrord, but it seems to have no effect

There are currently two known cases where mirrord cannot load into the application's process:

1.  Statically linked binaries. Since mirrord uses the dynamic linker to load into the application's process, it cannot load if the binary is statically linked. Support for statically linked binaries is planned for the long term, but for now you would have to make sure your binaries are dynamically linked in order to run them with mirrord. With Go programs, for example, it is as simple as adding `import "C"` to your program code. If you don't want to add an import to your Go program, you can alternatively build a dynamically linked binary using `go build -ldflags='-linkmode external'`. In VSCode, this can be done by adding `"buildFlags": "-ldflags='-linkmode external'"` to your `launch.json`.

    On Linux, append `-ldflags="-s=false"` to instruct `go run` not to omit the symbol table and debug information required by mirrord.

2.  If you are running mirrord on MacOS and the executable you are running is protected by [SIP](https://en.wikipedia.org/wiki/System_Integrity_Protection) (the application you are developing wouldn't be, but the binary that is used to execute it, e.g. `bash` for a bash script, might be protected), mirrord might have trouble loading into it (mirrord can generally bypass SIP, but there are still some unhandled edge cases). If that is the case, you could try copying the binary you're trying to run to an unprotected directory (e.g. anywhere in your home directory), changing the IDE run configuration or the CLI to use the copy instead of the original binary, and trying again. If it still doesn't work, also remove the signature from the copy with:

    `sudo codesign --remove-signature ./<your-binary>`

Another reason that mirrord might seem not to work is if your remote pod has more than one container. mirrord works at the level of the container, not the whole pod. If your pod runs multiple containers, you need to make sure mirrord targets the correct one by specifying it explicitly in the target configuration. Note that we filter out the proxy containers added by popular service meshes automatically.

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

This could happen because the local process is listening on a different port than the remote target. You can either change the local process to listen on the same port as the remote target (don't worry about the port being used locally by other processes), or use the `port_mapping` configuration to map the local port to a remote port.

## The remote target stops receiving remote traffic, but it doesn't reach my local process either

This can happen in some clusters using a service mesh when stealing incoming traffic. You can use this configuration to fix it:

```json
{"agent": {"flush_connections": false}}
```

## Traffic sent to the target through a Service ClusterIP is not stolen or mirrored on clusters using Cilium

On some clusters running Cilium as a kube-proxy replacement, requests sent to the target through a Service ClusterIP are handled by the remote target instead of your local process. Traffic sent directly to the pod IP (e.g. via `kubectl port-forward`) is stolen as expected.

Setting `bpf.hostLegacyRouting=true` on the Cilium Helm release works around it:

```bash
helm upgrade cilium cilium/cilium --namespace kube-system --reuse-values --set bpf.hostLegacyRouting=true
kubectl -n kube-system rollout restart daemonset cilium
```

The root cause is still under investigation — see [this issue](https://github.com/metalbear-co/mirrord/issues/4672) for updates.

See [this issue](https://github.com/metalbear-co/mirrord/issues/4672) for more details.

## My application is trying to read a file locally instead of from the cluster

mirrord has a list of path patterns that are read locally by default regardless of the configured fs mode. You can override this behavior in the configuration.

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

On clusters with Kubernetes version v1.23 or higher, agent pods are automatically cleaned up immediately (or after a configurable TTL). As a temporary solution for cleaning up completed agent pods manually, you can run:

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

If private networking is enabled, it is likely due to firewall rules blocking the mirrord operator's API service from the API server. To fix this, add a firewall rule that allows your cluster's master nodes to access TCP port 443 in your cluster's pods.

## My local process encounters unexpected certificate validation errors

When running processes locally versus in a container within Kubernetes, some languages handle certificate validation differently. For instance, a Go application on macOS will use the macOS Keychain for certificate validation, whereas the same application in a container will use different API calls.

To work around this issue (on macOS), you can use the following mirrord configuration:

```json
{
   "experimental": {"trust_any_certificate": true}
}
```

This configuration would make any certificate trusted for the process.

## Agent connection fails or drops when using an ephemeral agent with a service mesh

When running the agent as an ephemeral container, the agent shares the network stack with the target pod. This means that incoming connections to the agent are handled by the service mesh, which might drop it for various reasons (lack of TLS, not HTTP, etc.) To work around that, set the agent.port to be static using `agent.port` in values.yaml when installing the operator, then add a port exclusion for the agent port in your service mesh's configuration. For example, if you use Istio and have set the agent port to 5000, you can add the following annotation for exclusion:

```
traffic.sidecar.istio.io/excludeInboundPorts: '50000'
```

## I'm running a Next.js server with Nx and mirrord, but it doesn't behave as expected

If your Next.js app is managed by Nx and uses a custom server, the local process may crash or behave incorrectly because mirrord mirrors conflicting environment variables from the remote target.

To fix this, use `feature.env.exclude` to prevent mirrord from importing these specific variables:

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

## I can't get mirrord to work with Remix/Vite

Remix and Vite use the `NODE_ENV` environment variable to determine the runtime configuration. To ensure consistent behavior, you can override the remote `NODE_ENV` value:

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
