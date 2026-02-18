mirrord allows for a high degree of customization when it comes to which features you want to
enable, and how they should function.

All of the configuration fields have a default value, so a minimal configuration would be no
configuration at all.

The configuration supports templating using the [Tera](https://keats.github.io/tera/docs/) template engine.
Currently we don't provide additional values to the context, if you have anything you want us to
provide please let us know.

To use a configuration file in the CLI, use the `-f <CONFIG_PATH>` flag.
Or if using VSCode Extension or JetBrains plugin, simply create a `.mirrord/mirrord.json` file
or use the UI.

To help you get started, here are examples of a basic configuration file, and a complete
configuration file containing all fields.

### Basic `config.json` {#root-basic}

```json
{
  "target": "pod/bear-pod",
  "feature": {
    "env": true,
    "fs": "read",
    "network": true
  }
}
```

### Basic `config.json` with templating {#root-basic-templating}

```json
{
  "target": "{{ get_env(name="TARGET", default="pod/fallback") }}",
  "feature": {
    "env": true,
    "fs": "read",
    "network": true
  }
}
```

### Complete `config.json` {#root-complete}

 Don't use this example as a starting point, it's just here to show you all the available
 options.
```json
{
  "accept_invalid_certificates": false,
  "skip_processes": "ide-debugger",
  "target": {
    "path": "pod/bear-pod",
    "namespace": "default"
  },
  "connect_tcp": null,
  "agent": {
    "log_level": "info",
    "json_log": false,
    "labels": { "user": "meow" },
    "annotations": { "cats.io/inject": "enabled" },
    "namespace": "default",
    "image": "ghcr.io/metalbear-co/mirrord:latest",
    "image_pull_policy": "IfNotPresent",
    "image_pull_secrets": [ { "secret-key": "secret" } ],
    "ttl": 30,
    "ephemeral": false,
    "communication_timeout": 30,
    "startup_timeout": 360,
    "flush_connections": true,
    "metrics": "0.0.0.0:9000",
  },
  "feature": {
    "env": {
      "include": "DATABASE_USER;PUBLIC_ENV",
      "exclude": "DATABASE_PASSWORD;SECRET_ENV",
      "override": {
        "DATABASE_CONNECTION": "db://localhost:7777/my-db",
        "LOCAL_BEAR": "panda"
      },
      "mapping": {
        ".+_TIMEOUT": "1000"
      }
    },
    "fs": {
      "mode": "write",
      "read_write": ".+\\.json" ,
      "read_only": [ ".+\\.yaml", ".+important-file\\.txt" ],
      "local": [ ".+\\.js", ".+\\.mjs" ]
    },
    "network": {
      "incoming": {
        "mode": "steal",
        "http_filter": {
          "header_filter": "host: api\\..+"
        },
        "port_mapping": [[ 7777, 8888 ]],
        "ignore_localhost": false,
        "ignore_ports": [9999, 10000]
      },
      "outgoing": {
        "tcp": true,
        "udp": true,
        "filter": {
          "local": ["tcp://1.1.1.0/24:1337", "1.1.5.0/24", "google.com", ":53"]
        },
        "ignore_localhost": false,
        "unix_streams": "bear.+"
      },
      "dns": {
        "enabled": true,
        "filter": {
          "local": ["1.1.1.0/24:1337", "1.1.5.0/24", "google.com"]
        }
      }
    },
    "copy_target": {
      "scale_down": false
    }
  },
  "operator": true,
  "kubeconfig": "~/.kube/config",
  "sip_binaries": "bash",
  "telemetry": true,
  "kube_context": "my-cluster"
}
```

# Options {#root-options}

## accept_invalid_certificates {#root-accept_invalid_certificates}

Controls whether or not mirrord accepts invalid TLS certificates (e.g. self-signed
certificates).

If not provided, mirrord will use value from the kubeconfig.

## agent {#root-agent}

Configuration for the mirrord-agent pod that is spawned in the Kubernetes cluster.

**Note:** this configuration is ignored when using the mirrord Operator.
Agent configuration is done by the cluster admin.

We provide sane defaults for this option, so you don't have to set up anything here.

```json
{
  "agent": {
    "log_level": "info",
    "json_log": false,
    "namespace": "default",
    "image": "ghcr.io/metalbear-co/mirrord:latest",
    "image_pull_policy": "IfNotPresent",
    "image_pull_secrets": [ { "secret-key": "secret" } ],
    "ttl": 30,
    "ephemeral": false,
    "communication_timeout": 30,
    "startup_timeout": 360,
    "flush_connections": false,
    "exclude_from_mesh": false
    "inject_headers": false,
    "max_body_buffer_size": 65535,
    "max_body_buffer_timeout": 1000
  }
}
```

### agent.annotations {#agent-annotations}

Allows setting up custom annotations for the agent Job and Pod.

```json
{
  "agent": {
    "annotations": {
      "cats.io/inject": "enabled"
      "prometheus.io/scrape": "true",
      "prometheus.io/port": "9000"
    }
  }
}
```

### agent.check_out_of_pods {#agent-check_out_of_pods}

Determine if to check whether there is room for agent job in target node. (Not applicable
when using ephemeral containers feature)

Can be disabled if the check takes too long and you are sure there is enough resources on
each node

### agent.clean_iptables_on_start {#agent-clean_iptables_on_start}

Clean leftover iptables rules and start the new agent instead of erroring out when there
are existing mirrord rules in the target's iptables.

### agent.communication_timeout {#agent-communication_timeout}

Controls how long the agent lives when there are no connections.

Each connection has its own heartbeat mechanism, so even if the local application has no
messages, the agent stays alive until there are no more heartbeat messages.

### agent.disable_mesh_sidecar_injection {#agent-disable_mesh_sidecar_injection}

Add relevant labels and annotations to agent pods/jobs to
prevent service mesh sidecar injections. Defaults to true.

Only affects istio, linkerd, kuma.

### agent.disabled_capabilities {#agent-disabled_capabilities}

If nothing is disabled here, agent uses:
1. `NET_ADMIN`,
2. `SYS_PTRACE`,
3. `SYS_ADMIN`.

Has no effect when using the targetless mode,
as targetless agent containers have no capabilities.

### agent.dns {#agent-dns}

Configuration options for how the agent performs DNS resolution.

### agent.dns.attempts {#agent-dns-attempts}

Specifies the number of DNS resolution attempts the agent will make before failing.
Setting this too high may cause the internal proxy to time out and exit.

### agent.dns.timeout {#agent-dns-timeout}

Specifies how long (in seconds) the agent will wait for a DNS response before timing out.
If not specified the agent uses a default value of 1 second.
Setting this too high may cause the internal proxy to time out and exit.

### agent.ephemeral {#agent-ephemeral}

Runs the agent as an
[ephemeral container](https://kubernetes.io/docs/concepts/workloads/pods/ephemeral-containers/).

Not compatible with targetless runs.

Defaults to `false`.

### agent.exclude_from_mesh {#agent-exclude_from_mesh}

When running the agent as an ephemeral container, use this option to exclude
the agent's port from the service mesh sidecar proxy.

### agent.flush_connections {#agent-flush_connections}

Flushes existing connections when starting to steal, might fix issues where connections
aren't stolen (due to being already established)

Defaults to `true`.

### agent.image {#agent-image}

Name of the agent's docker image.

Useful when a custom build of mirrord-agent is required, or when using an internal
registry.

Defaults to the latest stable image `"ghcr.io/metalbear-co/mirrord:latest"`.

```json
{
  "agent": {
    "image": "internal.repo/images/mirrord:latest"
  }
}
```

Complete setup:

```json
{
  "agent": {
    "image": {
      "registry": "internal.repo/images/mirrord",
      "tag": "latest"
    }
  }
}
```

Can also be controlled via `MIRRORD_AGENT_IMAGE`, `MIRRORD_AGENT_IMAGE_REGISTRY`, and
`MIRRORD_AGENT_IMAGE_TAG`. `MIRRORD_AGENT_IMAGE` takes precedence, followed by config
values for registry/tag, then environment variables for registry/tag.

### agent.image_pull_policy {#agent-image_pull_policy}

Controls when a new agent image is downloaded.

Supports `"IfNotPresent"`, `"Always"`, `"Never"`, or any valid kubernetes
[image pull policy](https://kubernetes.io/docs/concepts/containers/images/#image-pull-policy)

Defaults to `"IfNotPresent"`

### agent.image_pull_secrets {#agent-image_pull_secrets}

List of secrets the agent pod has access to.

Takes an array of entries with the format `{ name: <secret-name> }`.

Read more [here](https://kubernetes.io/docs/concepts/containers/images/#referring-to-an-imagepullsecrets-on-a-pod).

```json
{
  "agent": {
    "image_pull_secrets": [
      { "name": "secret-key-1" },
      { "name": "secret-key-2" }
    ]
  }
}
```

### agent.inject_headers {#agent-inject_headers}

Sets whether `Mirrord-Agent` headers are injected into HTTP
responses that went through the agent.

Possible values for the header:

- `passed-through`: set when the request was not sent to the local app (perhaps because it
  didn't match the filters)

- `forwarded-to-client`: set when the request was sent to the local app

### agent.json_log {#agent-json_log}

Controls whether the agent produces logs in a human-friendly format, or json.

```json
{
  "agent": {
    "json_log": true
  }
}
```

### agent.labels {#agent-labels}

Allows setting up custom labels for the agent Job and Pod.

```json
{
  "agent": {
    "labels": { "user": "meow", "state": "asleep" }
  }
}
```

### agent.log_level {#agent-log_level}

Log level for the agent.


Supports `"trace"`, `"debug"`, `"info"`, `"warn"`, `"error"`, or any string that would work
with `RUST_LOG`.

```json
{
  "agent": {
    "log_level": "mirrord=debug,warn"
  }
}
```

### agent.max_body_buffer_size {#agent-max_body_buffer_size}

Maximum size, in bytes, of HTTP request body buffers. Used for
temporarily storing bodies of incoming HTTP requests to run
body filters. HTTP body filters will not match any requests
with bodies larger than this.

### agent.max_body_buffer_timeout {#agent-max_body_buffer_timeout}

Maximum timeout, in milliseconds, for receiving HTTP request
bodies. HTTP body filters will not match any requests whose
bodies do not arrive within this timeout.

### agent.metrics {#agent-metrics}

Enables prometheus metrics for the agent pod.

You might need to add annotations to the agent pod depending on how prometheus is
configured to scrape for metrics.

```json
{
  "agent": {
    "metrics": "0.0.0.0:9000"
  }
}
```

### agent.namespace {#agent-namespace}

Namespace where the agent shall live.

**Note:** ignored in targetless runs or when the agent is run as an ephemeral container.

Defaults to the current kubernetes namespace.

### agent.nftables {#agent-nftables}

Determines which iptables backend will be used for traffic redirection.

If set to `true`, the agent will use iptables-nft.
If set to `false`, the agent will use iptables-legacy.
If not set, the agent will try to detect the correct backend at runtime.

### agent.node_selector {#agent-node_selector}

Allows setting up custom node selector for the agent Pod. Applies only to targetless runs,
as targeted agent always runs on the same node as its target container.

```json
{
  "agent": {
    "node_selector": { "kubernetes.io/hostname": "node1" }
  }
}
```

### agent.priority_class {#agent-priority_class}

Specifies the priority class to assign to the agent pod.

```json
{
  "agent": {
    "priority_class": "my-priority-class-name"
  }
}
```

In some cases, the agent pod may fail to schedule due to node resource constraints.
Setting a priority class allows you to explicitly assign an existing priority class
from your cluster to the agent pod, increasing its priority relative to other workloads.

### agent.privileged {#agent-privileged}

Run the mirror agent as privileged container.
Defaults to `false`.

Might be needed in strict environments such as Bottlerocket.

Has no effect when using the targetless mode,
as targetless agent containers are never privileged.

### agent.resources {#agent-resources}

Set pod resource requirements. (not with ephemeral agents)
Default is
```json
{
  "agent": {
    "resources": {
      "requests":
      {
        "cpu": "1m",
        "memory": "1Mi"
      },
      "limits":
      {
        "cpu": "100m",
        "memory": "100Mi"
      }
    }
  }
}
```

### agent.security_context {#agent-security_context}

Agent pod security context (not with ephemeral agents).
Support seccomp profile and app armor profile.

### agent.service_account {#agent-service_account}

Allows setting up custom Service Account for the agent Job and Pod.

```json
{
  "agent": {
    "service_account": "my-service-account"
  }
}
```

### agent.startup_timeout {#agent-startup_timeout}

Controls how long to wait for the agent to finish initialization.

If initialization takes longer than this value, mirrord exits.

Defaults to `60`.

### agent.tolerations {#agent-tolerations}

Set pod tolerations. (not with ephemeral agents).

Defaults to `operator: Exists`.

```json
{
  "agent": {
    "tolerations": [
        {
          "key": "meow", "operator": "Exists", "effect": "NoSchedule"
        }
    ]
  }
}
```

Set to an empty array to have no tolerations at all

### agent.ttl {#agent-ttl}

Controls how long the agent pod persists for after the agent exits (in seconds).

Can be useful for collecting logs.

Defaults to `1`.

## baggage {#root-baggage}

OpenTelemetry (OTel) / W3C baggage propagator. This is used in HTTP requests sent to the
operator to manually set values in the trace span, which can help when processing traces.
See [OTel docs](https://opentelemetry.io/docs/specs/otel/context/env-carriers/#environment-variable-names)

Only relevant for use with the operator. For more details, read the [docs on monitoring](https://metalbear.com/mirrord/docs/managing-mirrord/monitoring).

## ci {#root-ci}

Configuration for mirrord for CI.

```json
{
  "ci": {
    "output_dir": "/tmp/mirrord/",
  }
}
```

### ci.output_dir {#ci-output_dir}

Path to a directory where `mirrord ci` will flush application's stdout and stderr.

Defaults to `/tmp/mirrord/`.

## container {#root-container}

Unstable: `mirrord container` command specific config.

### container.cli_extra_args {#container-cli_extra_args}

Any extra args to use when creating the sidecar mirrord-cli container.

This is useful when you want to use portforwarding, passing `-p local:container` won't work
for main command but adding them here will work
```json
{
  "container": {
    "cli_extra_args": ["-p", "local:container"]
  }
}
```

### container.cli_image {#container-cli_image}

Tag of the `mirrord-cli` image you want to use.

Defaults to `"ghcr.io/metalbear-co/mirrord-cli:<cli version>"`.

### container.cli_image_lib_path {#container-cli_image}

Path of the mirrord-layer lib inside the specified mirrord-cli image.

### container.cli_prevent_cleanup {#container-cli_extra_args}

Don't add `--rm` to sidecar command to prevent cleanup.

### container.cli_tls_path {#container-cli_tls_path}

When using`mirrord container` with external_proxy TLS enabled (is enabled by default), you
can specify the path where the certificate `.pem` file will be created, in the cli
container.

Defaults to `"/opt/mirrord/tls/mirrord-tls.pem"`.

### container.override_host_ip {#container-override_host_ip}

Allows to override the IP address for the internal proxy to use
when connecting to the host machine from within the container.

```json5
{
  "container": {
    "override_host_ip": "172.17.0.1" // usual resolution of value from `host.docker.internal`
  }
}
```

This should be useful if your host machine is exposed with a different IP address than the
one bound as host.

- If you're running inside WSL, and encountering problems, try setting
  `external_proxy.host_ip` T `0.0.0.0`, and this to the internal container runtime address
  (for docker, this  would be what `host.docker.internal` resolved to, which by default is
  `192.168.65.254`). You can find this ip by resolving it from inside a running container,
  e.g. `docker run --rm -it {image-with-nslookup} nslookup host.docker.internal`

### container.platform {#container-platform}

Platform specification for the target container (e.g., "linux/amd64", "linux/arm64").

When specified, the target container will run with this platform, while the internal proxy
container will still run on the native platform and contain both architectures (x64/arm64).
The LD_PRELOAD will automatically use the correct architecture.

```json
{
  "container": {
    "platform": "linux/amd64"
  }
}
```

## experimental {#root-experimental}

mirrord Experimental features.
This shouldn't be used unless someone from MetalBear/mirrord tells you to.

### _experimental_ applev {#experimental-applev}

Configuration for inspecting and modifying apple variables. macOS only.

### _experimental_ browser_extension_config {#experimental-browser_extension_config}

mirrord will open a URL for initiating mirrord browser extension to
automatically inject HTTP header that matches the HTTP filter configured in
`feature.network.incoming.http_filter.header_filter`.

### _experimental_ disable_reuseaddr {#experimental-disable_reuseaddr}

Disables the `SO_REUSEADDR` socket option on sockets that mirrord steals/mirrors.
On macOS the application can use the same address many times but then we don't steal it
correctly. This probably should be on by default but we want to gradually roll it out.
<https://github.com/metalbear-co/mirrord/issues/2819>
This option applies only on macOS.

### _experimental_ dlopen_cgo {#experimental-dlopen_cgo}

Useful when the user's application loads a c-shared golang library dynamically.

Defaults to `false`.

### _experimental_ dns_permission_error_fatal {#experimental-dns_permission_error_fatal}

Whether to terminate the session when a permission denied error
occurs during DNS resolution. This error often means that the Kubernetes cluster is
hardened, and the mirrord-agent is not fully functional without `agent.privileged`
enabled.

Defaults to `true`

### _experimental_ enable_exec_hooks_linux {#experimental-enable_exec_hooks_linux}

Enables exec hooks on Linux. Enable Linux hooks can fix issues when the application
shares sockets with child commands (e.g Python web servers with reload),
but the feature is not stable and may cause other issues.

### _experimental_ force_hook_connect {#experimental-force_hook_connect}

Forces hooking all instances of the connect function.
In very niche cases the connect function has multiple exports and this flag
makes us hook all of the instances. <https://linear.app/metalbear/issue/MBE-1385/mirrord-container-curl-doesnt-work-for-php-curl>

### _experimental_ hide_ipv6_interfaces {#experimental-hide_ipv6_interfaces}

Enables `getifaddrs` hook that removes IPv6 interfaces from the list returned by libc.

### _experimental_ hook_rename {#experimental-hook_rename}

Enables hooking the `rename` function.

Useful if you need file remapping and your application uses `rename`, i.e. `php-fpm`,
`twig`, to create and rename temporary files.

### _experimental_ idle_local_http_connection_timeout {#experimental-idle_local_http_connection_timeout}

Sets a timeout for idle local HTTP connections (in milliseconds).

HTTP requests stolen with a filter are delivered to the local application
from a HTTP connection made from the local machine. Once a request is delivered,
the connection is cached for some time, so that it can be reused to deliver
the next request.

This timeout determines for how long such connections are cached.

Set to 0 to disable caching local HTTP connections (connections will be dropped as soon as
the request is delivered).

Defaults to 3000ms.

### _experimental_ ignore_system_proxy_config {#experimental-ignore_system_proxy_config}

Disables any system wide proxy configuration for affecting the running application.

### _experimental_ latency {#experimental-latency}

Configuration for adding artificial latency to outgoing network operations.

Configuration for adding artificial latency to outgoing network operations.
Useful for testing application behavior under network delay conditions.

### _experimental_ latency.receive_delay {#experimental-latency-receive_delay}

Delay in milliseconds for outgoing receive operations (Agent → Layer).

Defaults to `0` (no delay).

### _experimental_ latency.transmit_delay {#experimental-latency-transmit_delay}

Delay in milliseconds for outgoing send operations (Layer → Agent).

Defaults to `0` (no delay).

### _experimental_ non_blocking_tcp_connect {#experimental-non_blocking_tcp_connect}

Enables better support for outgoing connections using non-blocking TCP sockets.

Defaults to `false`.

### _experimental_ sip_log_destination {#experimental-sip_log_destination}

Writes basic fork-safe SIP patching logs to a destination file.
Useful for seeing the state of SIP when `stdout` may be affected by another process.

### _experimental_ tcp_ping4_mock {#experimental-tcp_ping4_mock}

<https://github.com/metalbear-co/mirrord/issues/2421#issuecomment-2093200904>

### _experimental_ trust_any_certificate {#experimental-trust_any_certificate}

Enables trusting any certificate on macOS, useful for <https://github.com/golang/go/issues/51991#issuecomment-2059588252>

### _experimental_ use_dev_null {#experimental-use_dev_null}

Uses /dev/null for creating local fake files (should be better than using /tmp)

## external_proxy {#root-external_proxy}

Configuration for the external proxy mirrord spawns when using the `mirrord container` command.
This proxy is used to allow the internal proxy running in sidecar to connect to the mirrord
agent.

If you get `ConnectionRefused` errors, increasing the timeouts a bit might solve the issue.

```json
{
  "external_proxy": {
    "start_idle_timeout": 30,
    "idle_timeout": 5
  }
}
```

### external_proxy.host_ip {#external_proxy-host_ip}

Specify a custom host ip addr to listen on.

This address must be accessible from within the container.
If not specified, mirrord will try and resolve a local address to use.

- If you're running inside WSL, and encountering problems, try setting this to `0.0.0.0`,
  and `container.override_host_ip` to the internal container runtime address (for docker,
  this would be what `host.docker.internal` resolved to, which by default is
  `192.168.65.254`).

### external_proxy.idle_timeout {#external_proxy-idle_timeout}

How much time to wait while we don't have any active connections before exiting.

Common cases would be running a chain of processes that skip using the layer
and don't connect to the proxy.

```json
{
  "external_proxy": {
    "idle_timeout": 30
  }
}
```

### external_proxy.json_log {#external_proxy-json_log}

Whether the proxy should output logs in JSON format. If false, logs are output in
human-readable format.

Defaults to true.

### external_proxy.log_destination {#external_proxy-log_destination}

Set the log destination for the external proxy.

1. If the provided path ends with a separator (`/` on UNIX, `\` on Windows), it will be
   treated as a path to directory where the log file should be created.
2. Otherwise, if the path exists, mirrord will check if it's a directory or not.
3. Otherwise, it will be treated as a path to the log file.

mirrord will auto create all parent directories.

Defaults to a randomized path inside the temporary directory.

### external_proxy.log_level {#external_proxy-log_level}

Set the log level for the external proxy.

The value should follow the RUST_LOG convention (i.e `mirrord=trace`).

Defaults to `mirrord=info,warn`.

### external_proxy.start_idle_timeout {#external_proxy-start_idle_timeout}

How much time to wait for the first connection to the external proxy in seconds.

Common cases would be running with dlv or any other debugger, which sets a breakpoint
on process execution, delaying the layer startup and connection to the external proxy.

```json
{
  "external_proxy": {
    "start_idle_timeout": 60
  }
}
```

## feature {#root-feature}

Controls mirrord features.

See the
[technical reference, Technical Reference](https://metalbear.com/mirrord/docs/reference/)
to learn more about what each feature does.

The [`env`](#feature-env), [`fs`](#feature-fs) and [`network`](#feature-network) options
have support for a shortened version, that you can see [here](#root-shortened).

```json
{
  "feature": {
    "env": {
      "include": "DATABASE_USER;PUBLIC_ENV",
      "exclude": "DATABASE_PASSWORD;SECRET_ENV",
      "override": {
        "DATABASE_CONNECTION": "db://localhost:7777/my-db",
        "LOCAL_BEAR": "panda"
      }
    },
    "fs": {
      "mode": "write",
      "read_write": ".+\\.json" ,
      "read_only": [ ".+\\.yaml", ".+important-file\\.txt" ],
      "local": [ ".+\\.js", ".+\\.mjs" ]
    },
    "network": {
      "incoming": {
        "mode": "steal",
        "http_filter": {
          "header_filter": "host: api\\..+"
        },
        "port_mapping": [[ 7777, 8888 ]],
        "ignore_localhost": false,
        "ignore_ports": [9999, 10000]
      },
      "outgoing": {
        "tcp": true,
        "udp": true,
        "filter": {
          "local": ["tcp://1.1.1.0/24:1337", "1.1.5.0/24", "google.com", ":53"]
        },
        "ignore_localhost": false,
        "unix_streams": "bear.+"
      },
      "dns": false
    },
    "copy_target": false,
    "hostname": true
  }
}
```

### feature.copy_target {#feature-copy_target}

Creates a new copy of the target. mirrord will use this copy instead of the original target
(e.g. intercept network traffic). This feature requires a [mirrord operator](https://metalbear.com/mirrord/docs/overview/teams/?utm_source=copytarget).

This feature is not compatible with rollout targets and running without a target
(`targetless` mode).

Generated configuration for copy target feature.

#### feature.copy_target.exclude_containers {#feature-copy_target-exclude_containers}

Set a list of containers to be ignored by copy_target

#### feature.copy_target.exclude_init_containers {#feature-copy_target-exclude_init_containers}

Set a list of init containers to be ignored by copy_target

#### feature.copy_target.scale_down {#feature-copy_target-scale_down}

If this option is set, mirrord will scale down the target deployment to 0 for the time
the copied pod is alive.

This option is compatible only with deployment targets.
```json
    {
      "scale_down": true
    }
```

### feature.db_branches {#feature-db_branches}

Configuration for the database branching feature.

A list of configurations for database branches.

```json
{
  "feature": {
    "db_branches": [
      {
        "name": "my-database-name",
        "ttl_secs": 120,
        "type": "mysql",
        "version": "8.0",
        "connection": {
          "url": {
            "type": "env",
            "variable": "DB_CONNECTION_URL"
          }
        }
      }
    ]
  }
}
```

Configuration for a database branch.

Example:

```json
{
  "id": "my-branch-db",
  "name": "my-database-name",
  "ttl_secs": 120,
  "type": "mysql",
  "version": "8.0",
  "connection": {
    "url": {
      "type": "env",
      "variable": "DB_CONNECTION_URL"
    }
  }
}
```

When configuring a branch for MongoDB, set `type` to `mongodb`.

MySQL and Postgres database branch config objects share the following fields.

#### feature.db_branches[].connection (type: mysql, pg, mongodb) {#feature-db_branches-sql-connection}

`connection` describes how to get the connection information to the source database.
When the branch database is ready for use, Mirrord operator will replace the connection
information with the branch database's.

Different ways of connecting to the source database.

Example:

A single complete connection URL stored in an environment variable accessible from
the target pod template.

```json
{
  "url": {
    "type": "env",
    "variable": "DB_CONNECTION_URL"
  }
}
```

#### feature.db_branches[].creation_timeout_secs (type: mysql, pg, mongodb) {#feature-db_branches-sql-creation_timeout_secs}

The timeout in seconds to wait for a database branch to become ready after creation.
Defaults to 60 seconds. Adjust this value based on your database size and cluster
performance.

#### feature.db_branches[].id (type: mysql, pg, mongodb) {#feature-db_branches-sql-id}

Users can choose to specify a unique `id`. This is useful for reusing or sharing
the same database branch among Kubernetes users.

#### feature.db_branches[].name (type: mysql, pg, mongodb) {#feature-db_branches-sql-name}

When source database connection detail is not accessible to mirrord operator, users
can specify the database `name` so it is included in the connection options mirrord
uses as the override.

#### feature.db_branches[].ttl_secs (type: mysql, pg, mongodb) {#feature-db_branches-sql-ttl_secs}

Mirrord operator starts counting the TTL when a branch is no longer used by any session.
The time-to-live (TTL) for the branch database is set to 300 seconds by default.
Users can set `ttl_secs` to customize this value according to their need. Please note
that longer TTL paired with frequent mirrord session turnover can result in increased
resource usage. For this reason, branch database TTL caps out at 15 min.

#### feature.db_branches[].version (type: mysql, pg, mongodb) {#feature-db_branches-sql-version}

Mirrord operator uses a default version of the database image unless `version` is given.

Users can choose from the following copy mode to bootstrap their MongoDB branch database:

- Empty

  Creates an empty database. If the source DB connection options are found from the chosen
  target, mirrord operator extracts the database name and create an empty DB. Otherwise, mirrord
  operator looks for the `name` field from the branch DB config object. This option is useful
  for users that run DB migrations themselves before starting the application.

- All

  Copies both schema and data of all collections. Supports optional collection filters
  to copy only specific collections or filter documents within collections.

Configuration for copying a specific collection.

Example:

```json
{
  "users": {
    "filter": "{\"name\": {\"$in\": [\"alice\", \"bob\"]}}"
  },
  "orders": {
    "filter": "{\"created_at\": {\"$gt\": 1759948761}}"
  }
}
```

With the config above, only alice and bob from the `users` collection and orders
created after the given timestamp will be copied.

A MongoDB query filter in JSON format. Documents matching this filter will be copied.

Configuration for copying a specific collection.

Example:

```json
{
  "users": {
    "filter": "{\"name\": {\"$in\": [\"alice\", \"bob\"]}}"
  },
  "orders": {
    "filter": "{\"created_at\": {\"$gt\": 1759948761}}"
  }
}
```

With the config above, only alice and bob from the `users` collection and orders
created after the given timestamp will be copied.

A MongoDB query filter in JSON format. Documents matching this filter will be copied.

When configuring a branch for MySQL, set `type` to `mysql`.

MySQL and Postgres database branch config objects share the following fields.

#### feature.db_branches[].connection (type: mysql, pg, mongodb) {#feature-db_branches-sql-connection}

`connection` describes how to get the connection information to the source database.
When the branch database is ready for use, Mirrord operator will replace the connection
information with the branch database's.

Different ways of connecting to the source database.

Example:

A single complete connection URL stored in an environment variable accessible from
the target pod template.

```json
{
  "url": {
    "type": "env",
    "variable": "DB_CONNECTION_URL"
  }
}
```

#### feature.db_branches[].creation_timeout_secs (type: mysql, pg, mongodb) {#feature-db_branches-sql-creation_timeout_secs}

The timeout in seconds to wait for a database branch to become ready after creation.
Defaults to 60 seconds. Adjust this value based on your database size and cluster
performance.

#### feature.db_branches[].id (type: mysql, pg, mongodb) {#feature-db_branches-sql-id}

Users can choose to specify a unique `id`. This is useful for reusing or sharing
the same database branch among Kubernetes users.

#### feature.db_branches[].name (type: mysql, pg, mongodb) {#feature-db_branches-sql-name}

When source database connection detail is not accessible to mirrord operator, users
can specify the database `name` so it is included in the connection options mirrord
uses as the override.

#### feature.db_branches[].ttl_secs (type: mysql, pg, mongodb) {#feature-db_branches-sql-ttl_secs}

Mirrord operator starts counting the TTL when a branch is no longer used by any session.
The time-to-live (TTL) for the branch database is set to 300 seconds by default.
Users can set `ttl_secs` to customize this value according to their need. Please note
that longer TTL paired with frequent mirrord session turnover can result in increased
resource usage. For this reason, branch database TTL caps out at 15 min.

#### feature.db_branches[].version (type: mysql, pg, mongodb) {#feature-db_branches-sql-version}

Mirrord operator uses a default version of the database image unless `version` is given.

Users can choose from the following copy mode to bootstrap their MySQL branch database:

- Empty

  Creates an empty database. If the source DB connection options are found from the chosen
  target, mirrord operator extracts the database name and create an empty DB. Otherwise, mirrord
  operator looks for the `name` field from the branch DB config object. This option is useful
  for users that run DB migrations themselves before starting the application.

- Schema

  Creates an empty database and copies schema of all tables.

- All

  Copies both schema and data of all tables. This option shall only be used
  when the data volume of the source database is minimal.

In addition to copying an empty database or all tables' schema, mirrord operator
will copy data from the source DB when an array of table configs are specified.

Example:

```json
{
  "users": {
    "filter": "my_db.users.name = 'alice' OR my_db.users.name = 'bob'"
  },
  "orders": {
    "filter": "my_db.orders.created_at > 1759948761"
  }
}
```

With the config above, only alice and bob from the `users` table and orders
created after the given timestamp will be copied.

In addition to copying an empty database or all tables' schema, mirrord operator
will copy data from the source DB when an array of table configs are specified.

Example:

```json
{
  "users": {
    "filter": "my_db.users.name = 'alice' OR my_db.users.name = 'bob'"
  },
  "orders": {
    "filter": "my_db.orders.created_at > 1759948761"
  }
}
```

With the config above, only alice and bob from the `users` table and orders
created after the given timestamp will be copied.

When configuring a branch for PostgreSQL, set `type` to `pg`.

MySQL and Postgres database branch config objects share the following fields.

#### feature.db_branches[].connection (type: mysql, pg, mongodb) {#feature-db_branches-sql-connection}

`connection` describes how to get the connection information to the source database.
When the branch database is ready for use, Mirrord operator will replace the connection
information with the branch database's.

Different ways of connecting to the source database.

Example:

A single complete connection URL stored in an environment variable accessible from
the target pod template.

```json
{
  "url": {
    "type": "env",
    "variable": "DB_CONNECTION_URL"
  }
}
```

#### feature.db_branches[].creation_timeout_secs (type: mysql, pg, mongodb) {#feature-db_branches-sql-creation_timeout_secs}

The timeout in seconds to wait for a database branch to become ready after creation.
Defaults to 60 seconds. Adjust this value based on your database size and cluster
performance.

#### feature.db_branches[].id (type: mysql, pg, mongodb) {#feature-db_branches-sql-id}

Users can choose to specify a unique `id`. This is useful for reusing or sharing
the same database branch among Kubernetes users.

#### feature.db_branches[].name (type: mysql, pg, mongodb) {#feature-db_branches-sql-name}

When source database connection detail is not accessible to mirrord operator, users
can specify the database `name` so it is included in the connection options mirrord
uses as the override.

#### feature.db_branches[].ttl_secs (type: mysql, pg, mongodb) {#feature-db_branches-sql-ttl_secs}

Mirrord operator starts counting the TTL when a branch is no longer used by any session.
The time-to-live (TTL) for the branch database is set to 300 seconds by default.
Users can set `ttl_secs` to customize this value according to their need. Please note
that longer TTL paired with frequent mirrord session turnover can result in increased
resource usage. For this reason, branch database TTL caps out at 15 min.

#### feature.db_branches[].version (type: mysql, pg, mongodb) {#feature-db_branches-sql-version}

Mirrord operator uses a default version of the database image unless `version` is given.

Users can choose from the following copy mode to bootstrap their PostgreSQL branch database:

- Empty

  Creates an empty database. If the source DB connection options are found from the chosen
  target, mirrord operator extracts the database name and create an empty DB. Otherwise, mirrord
  operator looks for the `name` field from the branch DB config object. This option is useful
  for users that run DB migrations themselves before starting the application.

- Schema

  Creates an empty database and copies schema of all tables.

- All

  Copies both schema and data of all tables. This option shall only be used
  when the data volume of the source database is minimal.

In addition to copying an empty database or all tables' schema, mirrord operator
will copy data from the source DB when an array of table configs are specified.

Example:

```json
{
  "users": {
    "filter": "name = 'alice' OR name = 'bob'"
  },
  "orders": {
    "filter": "created_at > '2025-01-01'"
  }
}
```

With the config above, only alice and bob from the `users` table and orders
created after the given timestamp will be copied.

In addition to copying an empty database or all tables' schema, mirrord operator
will copy data from the source DB when an array of table configs are specified.

Example:

```json
{
  "users": {
    "filter": "name = 'alice' OR name = 'bob'"
  },
  "orders": {
    "filter": "created_at > '2025-01-01'"
  }
}
```

With the config above, only alice and bob from the `users` table and orders
created after the given timestamp will be copied.

#### feature.db_branches[].iam_auth (type: pg) {#feature-db_branches-pg-iam_auth}

IAM authentication for the source database.
Use this when your source database (AWS RDS, GCP Cloud SQL) requires IAM authentication
instead of password-based authentication.

Environment variable sources follow the same pattern as `connection.url`:
- `{ "type": "env", "variable": "VAR_NAME" }` - direct env var from pod spec
- `{ "type": "env_from", "variable": "VAR_NAME" }` - from configMapRef/secretRef

For AWS RDS/Aurora IAM authentication, set `type` to `"aws_rds"`.

Example:
```json
{
  "iam_auth": {
    "type": "aws_rds",
    "region": { "type": "env", "variable": "MY_AWS_REGION" },
    "access_key_id": { "type": "env_from", "variable": "AWS_KEY" }
  }
}
```

The init container must have AWS credentials (via IRSA, instance profile, or env vars).

Parameters:
- `region`: AWS region. If not specified, uses AWS_REGION or AWS_DEFAULT_REGION.
- `access_key_id`: AWS Access Key ID. If not specified, uses AWS_ACCESS_KEY_ID.
- `secret_access_key`: AWS Secret Access Key. If not specified, uses AWS_SECRET_ACCESS_KEY.
- `session_token`:  AWS Session Token (for temporary credentials). If not specified, uses
  AWS_SESSION_TOKEN.

For GCP Cloud SQL IAM authentication, set `type` to `"gcp_cloud_sql"`.

Example for GCP Cloud SQL with credentials from a secret:
```json
{
  "iam_auth": {
    "type": "gcp_cloud_sql",
    "credentials_json": { "type": "env_from", "variable": "GOOGLE_APPLICATION_CREDENTIALS_JSON" }
  }
}
```

The init container must have GCP credentials (via Workload Identity or service account key).
Use either `credentials_json` OR `credentials_path`, not both.

Parameters:
- `credentials_json`: Inline service account JSON key content. Specify the env var that
  contains the raw JSON content of the service account key. Example: ` { "type": "env",
  "variable": "GOOGLE_APPLICATION_CREDENTIALS_JSON" } `.
- `credentials_path`: Path to service account JSON key file. Specify the env var that
  contains the file path to the service account key. The file must be accessible from the
  init container. Example: `{"type": "env", "variable": "GOOGLE_APPLICATION_CREDENTIALS"}`.
- `project`: GCP project ID. If not specified, uses GOOGLE_CLOUD_PROJECT or GCP_PROJECT.

When configuring a branch for Redis, set `type` to `redis`.

Example with URL-based connection:
```json
{
  "type": "redis",
  "location": "local",
  "connection": {
    "url": {
      "type": "env",
      "variable": "REDIS_URL"
    }
  }
}
```

Example with separated settings:
```json
{
  "type": "redis",
  "location": "local",
  "connection": {
    "host": { "type": "env", "variable": "REDIS_HOST" },
    "port": 6379,
    "password": { "type": "env", "variable": "REDIS_PASSWORD" }
  }
}
```

#### feature.db_branches[].connection (type: redis) {#feature-db_branches-redis-connection}

Connection configuration for the Redis instance.

Supports either a complete URL or separated connection parameters.
If both are provided, `url` takes precedence.

The following fields can be sourced via remote environment variable:
- url
- host
- password
- username

Example:
```json
"connection": {
    "host": { "type": "env", "variable": "REDIS_HOST" },
    "port": 6379,
    "password": { "type": "env", "variable": "REDIS_PASSWORD" }
}
```

##### feature.db_branches[].connection.database (type: redis)

Redis database number (default: 0).

##### feature.db_branches[].connection.host (type: redis)

Redis host/hostname.
Can be sourced from an environment variable.

##### feature.db_branches[].connection.password (type: redis)

Redis password for authentication.
Can be sourced from an environment variable.

##### feature.db_branches[].connection.port (type: redis)

Redis port (default: 6379).

##### feature.db_branches[].connection.tls (type: redis)

Enable TLS/SSL connection.

##### feature.db_branches[].connection.url (type: redis)

Complete Redis URL (e.g., `redis://user:pass@host:6379/0`).
Can be sourced from an environment variable.

##### feature.db_branches[].connection.username (type: redis)

Redis username (Redis 6+ ACL).
Can be sourced from an environment variable.

#### feature.db_branches[].id (type: redis) {#feature-db_branches-redis-id}

Optional unique identifier for reusing branches across sessions.

#### feature.db_branches[].local (type: redis) {#feature-db_branches-redis-local}

Local Redis runtime configuration.
Only used when `location` is `local`.

Configuration for local Redis runtime.

##### feature.db_branches[].local.container_command (type: redis)

Custom path to the container command.
If not provided, uses the runtime name from PATH (e.g., "docker").
Example: `/usr/local/bin/docker` or `/home/user/.local/bin/podman`

##### feature.db_branches[].local.container_runtime (type: redis)

Which container runtime to use (Docker, Podman, or nerdctl).
Only applies when `runtime` is `container` or `auto`.

Container runtimes supported by mirrord.

Docker container runtime. (default)

nerdctl container runtime (containerd).

Podman container runtime.

##### feature.db_branches[].local.options (type: redis)

Additional Redis configuration options.

Example:
```json
{
  "args": ["--maxmemory", "256mb", "--appendonly", "yes"]
}
```

Raw arguments passed directly to redis-server or as Docker CMD args.
Use standard Redis config syntax (e.g., "--maxmemory 256mb").

##### feature.db_branches[].local.port (type: redis)

Local port to bind Redis to (default: 6379).

##### feature.db_branches[].local.runtime (type: redis)

Runtime backend for local Redis: `container`, `redis_server`, or `auto`.

For container-based runtimes, mirrord spawns the Redis image in a container.
For `redis_server`, it runs the native binary directly.

Backends:
- `container` (default) - Uses a container runtime (Docker/Podman/nerdctl), configured via
  `container_runtime`.
- `redis_server` - Uses native redis-server binary
- `auto` - Tries container first, falls back to redis-server

##### feature.db_branches[].local.server_command (type: redis)

Custom path to the redis-server binary.
If not provided, uses "redis-server" from PATH.
Example: `/opt/redis/bin/redis-server`

##### feature.db_branches[].local.version (type: redis)

Redis version/tag to use (default: "7-alpine").
Used as the container image tag.

#### feature.db_branches[].location (type: redis) {#feature-db_branches-redis-location}

Where the Redis instance should run.
- `local`: Spawns a local Redis instance managed by mirrord.
- `remote`: Uses the remote Redis (default behavior, no-op).

### feature.env {#feature-env}

Allows the user to set or override the local process' environment variables with the ones
from the remote pod.

Can be set to one of the options:

1. `false` - Disables the feature, won't have remote environment variables.
2. `true` - Enables the feature, will obtain remote environment variables.
3. object - see below (means `true` + additional configuration).

Which environment variables to load from the remote pod are controlled by setting either
[`include`](#feature-env-include) or [`exclude`](#feature-env-exclude).

See the environment variables [reference](https://metalbear.com/mirrord/docs/reference/env/) for more details.

```json
{
  "feature": {
    "env": {
      "include": "DATABASE_USER;PUBLIC_ENV;MY_APP_*",
      "exclude": "DATABASE_PASSWORD;SECRET_ENV",
      "override": {
        "DATABASE_CONNECTION": "db://localhost:7777/my-db",
        "LOCAL_BEAR": "panda"
      },
      "mapping": {
        ".+_TIMEOUT": "1000"
      }
    }
  }
}
```

#### feature.env.env_file {#feature-env-env-file}

Allows for passing environment variables from an env file.

These variables will override environment fetched from the remote target.

#### feature.env.exclude {#feature-env-exclude}

Include the remote environment variables in the local process that are **NOT** specified by
this option.
Variable names can be matched using `*` and `?` where `?` matches exactly one occurrence of
any character and `*` matches arbitrary many (including zero) occurrences of any character.

Some of the variables that are excluded by default:
`PATH`, `HOME`, `HOMEPATH`, `CLASSPATH`, `JAVA_EXE`, `JAVA_HOME`, `PYTHONPATH`.

Can be passed as a list or as a semicolon-delimited string (e.g. `"VAR;OTHER_VAR"`).

#### feature.env.include {#feature-env-include}

Include only these remote environment variables in the local process.
Variable names can be matched using `*` and `?` where `?` matches exactly one occurrence of
any character and `*` matches arbitrary many (including zero) occurrences of any character.

Can be passed as a list or as a semicolon-delimited string (e.g. `"VAR;OTHER_VAR"`).

Some environment variables are excluded by default (`PATH` for example), including these
requires specifying them with `include`

#### feature.env.load_from_process {#feature-env-load_from_process}

Allows for changing the way mirrord loads remote environment variables.
If set, the variables are fetched after the user application is started.

This setting is meant to resolve issues when using mirrord via the IntelliJ plugin on WSL
and the remote environment contains a lot of variables.

#### feature.env.mapping {#feature-env-mapping}

Specify map of patterns that if matched will replace the value according to specification.

*Capture groups are allowed.*

Example:
```json
{
  ".+_TIMEOUT": "10000"
  "LOG_.+_VERBOSITY": "debug"
  "(\w+)_(\d+)": "magic-value"
}
```

Will do the next replacements for environment variables that match:

* `CONNECTION_TIMEOUT: 500` => `CONNECTION_TIMEOUT: 10000`

* `LOG_FILE_VERBOSITY: info` => `LOG_FILE_VERBOSITY: debug`

* `DATA_1234: common-value` => `DATA_1234: magic-value`

#### feature.env.override {#feature-env-override}

Allows setting or overriding environment variables (locally) with a custom value.

For example, if the remote pod has an environment variable `REGION=1`, but this is an
undesirable value, it's possible to use `override` to set `REGION=2` (locally) instead.

Environment specified here will also override variables passed via the env file.

#### feature.env.unset {#feature-env-unset}

Allows unsetting environment variables in the executed process.

This is useful for when some system/user-defined environment like `AWS_PROFILE` make the
application behave as if it's running locally, instead of using the remote settings.
The unsetting happens from extension (if possible)/CLI and when process initializes.
In some cases, such as Go the env might not be able to be modified from the process itself.
This is case insensitive, meaning if you'd put `AWS_PROFILE` it'd unset both `AWS_PROFILE`
and `Aws_Profile` and other variations.

### feature.fs {#feature-fs}

Allows the user to specify the default behavior for file operations:

1. `"read"` or `true` - Read from the remote file system (default)
2. `"write"` - Read/Write from the remote file system.
3. `"local"` or `false` - Read from the local file system.
4. `"localwithoverrides"` - perform fs operation locally, unless the path matches a pre-defined
   or user-specified exception.

> Note: by default, some paths are read locally or remotely, regardless of the selected FS mode.
> This is described in further detail below.

Besides the default behavior, the user can specify behavior for specific regex patterns.
Case insensitive.

1. `"read_write"` - List of patterns that should be read/write remotely.
2. `"read_only"` - List of patterns that should be read only remotely.
3. `"local"` - List of patterns that should be read locally.
4. `"not_found"` - List of patters that should never be read nor written. These files should be
   treated as non-existent.
4. `"mapping"` - Map of patterns and their corresponding replacers. The replacement happens before any specific behavior as defined above or mode (uses [`Regex::replace`](https://docs.rs/regex/latest/regex/struct.Regex.html#method.replace))

The logic for choosing the behavior is as follows:


1. Check agains "mapping" if path needs to be replaced, if matched then continue to next step
   with new path after replacements otherwise continue as usual.
2. Check if one of the patterns match the file path, do the corresponding action. There's no
   specified order if two lists match the same path, we will use the first one (and we do not
   guarantee what is first).

   **Warning**: Specifying the same path in two lists is unsupported and can lead to undefined
   behaviour.

3. There are pre-defined exceptions to the set FS mode.
  1. Paths that match [the patterns defined here](https://github.com/metalbear-co/mirrord/tree/latest/mirrord/layer/src/file/filter/read_local_by_default.rs)
     are read locally by default.
  2. Paths that match [the patterns defined here](https://github.com/metalbear-co/mirrord/tree/latest/mirrord/layer/src/file/filter/read_remote_by_default.rs)
     are read remotely by default when the mode is `localwithoverrides`.
  3. Paths that match [the patterns defined here](https://github.com/metalbear-co/mirrord/tree/latest/mirrord/layer/src/file/filter/not_found_by_default.rs)
     under the running user's home directory will not be found by the application when the mode
     is not `local`.

  In order to override that default setting for a path, or a pattern, include it the
  appropriate pattern set from above. E.g. in order to read files under `/etc/` remotely even
  though it is covered by [the set of patterns that are read locally by default](https://github.com/metalbear-co/mirrord/tree/latest/mirrord/layer/src/file/filter/read_local_by_default.rs),
    add `"^/etc/."` to the `read_only` set.

4. If none of the above match, use the default behavior (mode).

For more information, check the file operations
[technical reference](https://metalbear.com/mirrord/docs/reference/fileops/).

```json
{
  "feature": {
    "fs": {
      "mode": "write",
      "read_write": ".+\\.json" ,
      "read_only": [ ".+\\.yaml", ".+important-file\\.txt" ],
      "local": [ ".+\\.js", ".+\\.mjs" ],
      "not_found": [ "\\.config/gcloud" ]
    }
  }
}
```

#### feature.fs.local {#feature-fs-local}

Specify file path patterns that if matched will be opened locally.

#### feature.fs.mapping {#feature-fs-mapping}

Specify map of patterns that if matched will replace the path according to specification.

*Capture groups are allowed.*

Example:
```json
{
  "^/home/(?<user>\\S+)/dev/tomcat": "/etc/tomcat"
  "^/home/(?<user>\\S+)/dev/config/(?<app>\\S+)": "/mnt/configs/${user}-$app"
}
```
Will do the next replacements for any io operaton

`/home/johndoe/dev/tomcat/context.xml` => `/etc/tomcat/context.xml`
`/home/johndoe/dev/config/api/app.conf` => `/mnt/configs/johndoe-api/app.conf`

- Relative paths: this feature (currently) does not apply mappings to relative paths, e.g.
  `../dev`.

#### feature.fs.mode {#feature-fs-mode}

### feature.fs.mode {#feature-fs-mode}

Configuration for enabling read-only or read-write file operations.

These options are overriden by user specified overrides and mirrord default overrides.

If you set [`"localwithoverrides"`](#feature-fs-mode-localwithoverrides) then some files
can be read/write remotely based on our default/user specified.
Default option for general file configuration.

The accepted values are: `"local"`, `"localwithoverrides`, `"read"`, or `"write`.

#### feature.fs.mode.local {#feature-fs-mode-local}

mirrord won't do anything fs-related, all operations will be local.

#### feature.fs.mode.localwithoverrides {#feature-fs-mode-localwithoverrides}

mirrord will run overrides on some file operations, but most will be local.

#### feature.fs.mode.read {#feature-fs-mode-read}

mirrord will read files from the remote, but won't write to them.

#### feature.fs.mode.write {#feature-fs-mode-write}

mirrord will read/write from the remote.

#### feature.fs.not_found {#feature-fs-not_found}

Specify file path patterns that if matched will be treated as non-existent.

#### feature.fs.read_only {#feature-fs-read_only}

Specify file path patterns that if matched will be read from the remote.
if file matching the pattern is opened for writing or read/write it will be opened locally.

#### feature.fs.read_write {#feature-fs-read_write}

Specify file path patterns that if matched will be read and written to the remote.

#### feature.fs.readonly_file_buffer {#feature-fs-readonly_file_buffer}

Sets buffer size for read-only remote files in bytes. By default, the value is
128000 bytes, or 128 kB.

Setting the value to 0 disables file buffering.
Otherwise, read-only remote files will be read in chunks and buffered locally.
This improves performance when the user application reads data in small portions.

### feature.hostname {#feature-hostname}

Should mirrord return the hostname of the target pod when calling `gethostname`

### feature.network {#feature-network}

Controls mirrord network operations.

See the network traffic [reference](https://metalbear.com/mirrord/docs/reference/traffic/)
for more details.

```json
{
  "feature": {
    "network": {
      "incoming": {
        "mode": "steal",
        "http_filter": {
          "header_filter": "host: api\\..+"
        },
        "port_mapping": [[ 7777, 8888 ]],
        "ignore_localhost": false,
        "ignore_ports": [9999, 10000]
      },
      "outgoing": {
        "tcp": true,
        "udp": true,
        "filter": {
          "local": ["tcp://1.1.1.0/24:1337", "1.1.5.0/24", "google.com", ":53"]
        },
        "ignore_localhost": false,
        "unix_streams": "bear.+"
      },
      "dns": {
        "enabled": true,
        "filter": {
          "local": ["1.1.1.0/24:1337", "1.1.5.0/24", "google.com"]
        }
      }
    }
  }
}
```

#### feature.network.dns {#feature-network-dns}

Resolve DNS via the remote pod.

Defaults to `true`.

Mind that:
- DNS resolving can be done in multiple ways. Some frameworks use `getaddrinfo`/`gethostbyname`
  functions, while others communicate directly with the DNS server at port `53` and perform a
  sort of manual resolution. Just enabling the `dns` feature in mirrord might not be enough. If
  you see an address resolution error, try enabling the [`fs`](#feature-fs) feature, and setting
  `read_only: ["/etc/resolv.conf"]`.
- DNS filter currently works only with frameworks that use `getaddrinfo`/`gethostbyname`
  functions.

##### feature.network.dns.filter {#feature-network-dns-filter}

Unstable: the precise syntax of this config is subject to change.

List of addresses/ports/subnets that should be resolved through either the remote pod or local
app, depending how you set this up with either `remote` or `local`.

You may use this option to specify when DNS resolution is done from the remote pod (which
is the default behavior when you enable remote DNS), or from the local app (default when
you have remote DNS disabled).

Takes a list of values, such as:

- Only queries for hostname `my-service-in-cluster` will go through the remote pod.

```json
{
  "remote": ["my-service-in-cluster"]
}
```

- Only queries for addresses in subnet `1.1.1.0/24` with service port `1337`` will go through
  the remote pod.

```json
{
  "remote": ["1.1.1.0/24:1337"]
}
```

- Only queries for hostname `google.com` with service port `1337` or `7331` will go through the
  remote pod.

```json
{
  "remote": ["google.com:1337", "google.com:7331"]
}
```

- Only queries for `localhost` with service port `1337` will go through the local app.

```json
{
  "local": ["localhost:1337"]
}
```

- Only queries with service port `1337` or `7331` will go through the local app.

```json
{
  "local": [":1337", ":7331"]
}
```

Valid values follow this pattern: `[name|address|subnet/mask][:port]`.

When filters are specified under `local`, matching DNS queries will go through the local
app , everything else will go through the remote pod.

When filters are specified under `remote`, matching DNS queries will go through the remote
pod, everything else will go through local.

#### feature.network.incoming {#feature-network-incoming}

Controls the incoming TCP traffic feature.

See the incoming [reference](https://metalbear.com/mirrord/docs/reference/traffic/#incoming) for more
details.

Incoming traffic supports 3 [modes](#feature-network-incoming-mode) of operation:

1. Mirror (**default**): Sniffs the TCP data from a port, and forwards a copy to the interested
   listeners;

2. Steal: Captures the TCP data from a port, and forwards it to the local process.

3. Off: Disables the incoming network feature.

This field can either take an object with more configuration fields (that are documented below),
or alternatively -
- A boolean:
  - `true`: use the default configuration, same as not specifying this field at all.
  - `false`: disable incoming configuration.
- One of the incoming [modes](#feature-network-incoming-mode) (lowercase).

Examples:

Steal all the incoming traffic:

```json
{
  "feature": {
    "network": {
      "incoming": "steal"
    }
  }
}
```

Disable the incoming traffic feature:

```json
{
  "feature": {
    "network": {
      "incoming": false
    }
  }
}
```

Steal only traffic that matches the
[`http_filter`](#feature-network-incoming-http_filter) (steals only HTTP traffic).

```json
{
  "feature": {
    "network": {
      "incoming": {
        "mode": "steal",
        "http_filter": {
          "header_filter": "host: api\\..+"
        },
        "port_mapping": [[ 7777, 8888 ]],
        "ignore_localhost": false,
        "ignore_ports": [9999, 10000],
        "listen_ports": [[80, 8111]]
      }
    }
  }
}
```

##### feature.network.incoming.http_filter {#feature-network-incoming-http-filter}

Filter configuration for the HTTP traffic stealer feature.

Allows the user to set a filter (regex) for the HTTP headers, so that the stealer traffic
feature only captures HTTP requests that match the specified filter, forwarding unmatched
requests to their original destinations.

Only does something when [`feature.network.incoming.mode`](#feature-network-incoming-mode) is
set as `"steal"`, ignored otherwise.

For example, to filter based on header:
```json
{
  "header_filter": "host: api\\..+"
}
```
Setting that filter will make mirrord only steal requests with the `host` header set to hosts
that start with "api", followed by a dot, and then at least one more character.

For example, to filter based on path:
```json
{
  "path_filter": "^/api/"
}
```
Setting this filter will make mirrord only steal requests to URIs starting with "/api/".


This can be useful for filtering out Kubernetes liveness, readiness and startup probes.
For example, for avoiding stealing any probe sent by kubernetes, you can set this filter:
```json
{
  "header_filter": "^User-Agent: (?!kube-probe)"
}
```
Setting this filter will make mirrord only steal requests that **do** have a user agent that
**does not** begin with "kube-probe".

Similarly, you can exclude certain paths using a negative look-ahead:
```json
{
  "path_filter": "^(?!/health/)"
}
```
Setting this filter will make mirrord only steal requests to URIs that do not start with
"/health/".

With `all_of` and `any_of`, you can use multiple HTTP filters at the same time.

If you want to steal HTTP requests that match **every** pattern specified, use `all_of`.
For example, this filter steals only HTTP requests to endpoint `/api/my-endpoint` that contain
header `x-debug-session` with value `121212`.
```json
{
  "all_of": [
    { "header": "^x-debug-session: 121212$" },
    { "path": "^/api/my-endpoint$" }
  ]
}
```

If you want to steal HTTP requests that match **any** of the patterns specified, use `any_of`.
For example, this filter steals HTTP requests to endpoint `/api/my-endpoint`
**and** HTTP requests that contain header `x-debug-session` with value `121212`.
```json
{
 "any_of": [
   { "path": "^/api/my-endpoint$"},
   { "header": "^x-debug-session: 121212$" }
 ]
}
```

##### feature.network.incoming.http_filter.all_of {#feature-network-incoming-http_filter-all_of}

An array of HTTP filters.

Each inner filter specifies either header or path regex.
Requests must match all of the filters to be stolen.

Cannot be an empty list.

Example:
```json
{
  "all_of": [
    { "header": "x-user: my-user$" },
    { "path": "^/api/v1/my-endpoint" }
    { "method": "post" }
  ]
}
```

##### feature.network.incoming.http_filter.any_of {#feature-network-incoming-http_filter-any_of}

An array of HTTP filters.

Each inner filter specifies either header or path regex.
Requests must match at least one of the filters to be stolen.

Cannot be an empty list.

Example:
```json
{
  "any_of": [
    { "header": "^x-user: my-user$" },
    { "path": "^/api/v1/my-endpoint" }
    { "method": "post" }
  ]
}
```

##### feature.network.incoming.http_filter.body_filter {#feature-network-incoming-http-body-filter}

Matches the request based on the contents of its body.

Currently only JSON body filtering is supported.

##### feature.network.incoming.inner_filter.body_filter.json {#feature-network-incoming-inner-body-filter-json}

Tries to parse the body as a JSON object and find (a) matching subobjects(s).

`query` should be a valid JSONPath (RFC 9535) query string.
`matches` should be a regex. Supports regexes validated by the
[`fancy-regex`](https://docs.rs/fancy-regex/latest/fancy_regex/) crate

Example:
```json
"http_filter": {
  "body_filter": {
    "body": "json",
    "query": "$.library.books[*]",
    "matches": "^\\d{3,5}$"
  }
}
```
will match
```json
{
  "library": {
    "books": [
      34555,
      1233,
      234
      23432
    ]
  }
}
```

The filter will match if there is at least one query result.

Non-string matches are stringified before being compared to
the regex. To filter query results by type, the `typeof`
[function extension](https://www.rfc-editor.org/rfc/rfc9535.html#name-function-extensions)
is provided. It takes in a single `NodesType` parameter and
returns `"null" | "bool" | "number" | "string" | "array" | "object"`,
depending on the type of the argument. If not all nodes in the
argument have the same type, it returns `nothing`.

Example:

```json
"body_filter": {
  "body": "json",
  "query": "$.books[?(typeof(@) == 'number')]",
  "matches": "4$"
}
```
will match

```json
{
  "books": [
    1111,
    2222,
    4444
  ]
}
```

but not

```json
{
  "books": [
    "1111",
    "2222",
    "4444"
  ]
}
```



To use with with `all_of` or `any_of`, use the following syntax:
```json
"http_filter": {
  "all_of": [
    {
      "path": "/buildings"
    },
    {
      "body": "json",
      "query": "$.library.books[*]",
      "matches": "^\\d{3,5}$"
    }
  ]
}
```

##### feature.network.incoming.http_filter.header_filter {#feature-network-incoming-http-header-filter}


Supports regexes validated by the
[`fancy-regex`](https://docs.rs/fancy-regex/latest/fancy_regex/) crate.

The HTTP traffic feature converts the HTTP headers to `HeaderKey: HeaderValue`,
case-insensitive.

##### feature.network.incoming.http_filter.method_filter {#feature-network-incoming-http-method-filter}


Supports standard [HTTP methods](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Methods), and non-standard HTTP methods.

Case-insensitive. If the request method matches the filter, the request is stolen.

##### feature.network.incoming.http_filter.path_filter {#feature-network-incoming-http-path-filter}


Supports regexes validated by the
[`fancy-regex`](https://docs.rs/fancy-regex/latest/fancy_regex/) crate.

Case-insensitive. Tries to find match in the path (without query) and path+query.
If any of the two matches, the request is stolen.

##### feature.network.incoming.http_filter.ports {#feature-network-incoming-http_filter-ports}

Activate the HTTP traffic filter only for these ports. When
absent, filtering will be done for all ports.

##### feature.network.incoming.https_delivery {#feature-network-incoming-https_delivery}

DEPRECATED: use `tls_delivery` instead.

Stolen TLS traffic can be delivered to the local application either as TLS or as plain TCP.
Note that stealing TLS traffic requires mirrord Operator support.

To have the stolen TLS traffic delivered with plain TCP, use:

```json
{
  "protocol": "tcp"
}
```

To have the traffic delivered with TLS, use:
```json
{
  "protocol": "tls"
}
```

By default, the local mirrord TLS client will trust any certificate presented by the local
application's TLS server. To override this behavior, you can either:

1. Specify a list of paths to trust roots. These paths can lead either to PEM files or PEM file
   directories. Each found certificate will be used as a trust anchor.
2. Specify a path to the cartificate chain used by the server.

Example with trust roots:
```json
{
  "protocol": "tls",
  "trust_roots": ["/path/to/cert.pem", "/path/to/cert/dir"]
}
```

Example with certificate chain:
```json
{
  "protocol": "tls",
  "server_cert": "/path/to/cert.pem"
}
```

To make a TLS connection to the local application's server,
mirrord's TLS client needs a server name. You can supply it manually like this:
```json
{
  "protocol": "tls",
  "server_name": "my.test.server.name"
}
```

If you don't supply the server name:

1. If `server_cert` is given, and the found end-entity certificate contains a valid server name,
   this server name will be used;
2. Otherwise, if the original client supplied an SNI extension, the server name from that
   extension will be used;
3. Otherwise, if the stolen request's URL contains a valid server name, that server name will be
   used;
4. Otherwise, `localhost` will be used.

##### feature.network.incoming.tls_delivery.protocol {#feature-network-incoming-tls_delivery-protocol}

Protocol to use when delivering the TLS traffic locally.


Path to a PEM file containing the certificate chain used by the local application's
TLS server.

This file must contain at least one certificate.
It can contain entries of other types, e.g private keys, which are ignored.

##### feature.network.incoming.tls_delivery.server_name {#feature-network-incoming-tls_delivery-server_name}

Server name to use when making a connection.

Must be a valid DNS name or an IP address.

##### feature.network.incoming.tls_delivery.trust_roots {#feature-network-incoming-tls_delivery-trust_roots}

Paths to PEM files and directories with PEM files containing allowed root certificates.

Directories are not traversed recursively.

Each certificate found in the files is treated as an allowed root.
The files can contain entries of other types, e.g private keys, which are ignored.

##### feature.network.incoming.ignore_localhost {#feature-network-incoming-ignore_localhost}

##### feature.network.incoming.ignore_ports {#feature-network-incoming-ignore_ports}

Ports to ignore when mirroring/stealing traffic, these ports will remain local.

Can be especially useful when
[`feature.network.incoming.mode`](#feature-network-incoming-mode) is set to `"steal"`,
and you want to avoid redirecting traffic from some ports (for example, traffic from
a health probe, or other heartbeat-like traffic).

Mutually exclusive with [`feature.network.incoming.ports`](#feature-network-ports).

##### feature.network.incoming.listen_ports {#feature-network-incoming-listen_ports}

Mapping for local ports to actually used local ports.
When application listens on a port while steal/mirror is active
we fallback to random ports to avoid port conflicts.
Using this configuration will always use the specified port.
If this configuration doesn't exist, mirrord will try to listen on the original port
and if it fails it will assign a random port

This is useful when you want to access ports exposed by your service locally
For example, if you have a service that listens on port `80` and you want to access it,
you probably can't listen on `80` without sudo, so you can use `[[80, 4480]]`
then access it on `4480` while getting traffic from remote `80`.
The value of `port_mapping` doesn't affect this.

##### feature.network.incoming.mode {#feature-network-incoming-mode}

Allows selecting between mirrorring or stealing traffic.

Can be set to either `"mirror"` (default), `"steal"` or `"off"`.

- `"mirror"`: Sniffs on TCP port, and send a copy of the data to listeners.
- `"off"`: Disables the incoming network feature.
- `"steal"`: Supports 2 modes of operation:

1. Port traffic stealing: Steals all TCP data from a port, which is selected whenever the user
   listens in a TCP socket (enabling the feature is enough to make this work, no additional
   configuration is needed);

2. HTTP traffic stealing: Steals only HTTP traffic, mirrord tries to detect if the incoming data
   on a port is HTTP (in a best-effort kind of way, not guaranteed to be HTTP), and steals the
   traffic on the port if it is HTTP;

##### feature.network.incoming.on_concurrent_steal {#feature-network-incoming-on_concurrent_steal}

(Operator Only): Allows overriding port locks

Can be set to either `"continue"` or `"override"`.

- `"continue"`: Continue with normal execution
- `"override"`: If port lock detected then override it with new lock and force close the
  original locking connection.

##### feature.network.incoming.port_mapping {#feature-network-incoming-port_mapping}

Mapping for local ports to remote ports.

This is useful when you want to mirror/steal a port to a different port on the remote
machine. For example, your local process listens on port `9333` and the container listens
on port `80`. You'd use `[[9333, 80]]`

##### feature.network.incoming.ports {#feature-network-incoming-ports}

When set, traffic will only be mirrored/stolen on these ports,
and other ports will remain local. Otherwise, all ports are
mirrored/stolen.

Mutually exclusive with
[`feature.network.incoming.ignore_ports`](#feature-network-ignore_ports).

##### feature.network.incoming.tls_delivery {#feature-network-incoming-tls_delivery}

(Operator Only): configures how mirrord delivers stolen TLS traffic
to the local application.

Stolen TLS traffic can be delivered to the local application either as TLS or as plain TCP.
Note that stealing TLS traffic requires mirrord Operator support.

To have the stolen TLS traffic delivered with plain TCP, use:

```json
{
  "protocol": "tcp"
}
```

To have the traffic delivered with TLS, use:
```json
{
  "protocol": "tls"
}
```

By default, the local mirrord TLS client will trust any certificate presented by the local
application's TLS server. To override this behavior, you can either:

1. Specify a list of paths to trust roots. These paths can lead either to PEM files or PEM file
   directories. Each found certificate will be used as a trust anchor.
2. Specify a path to the cartificate chain used by the server.

Example with trust roots:
```json
{
  "protocol": "tls",
  "trust_roots": ["/path/to/cert.pem", "/path/to/cert/dir"]
}
```

Example with certificate chain:
```json
{
  "protocol": "tls",
  "server_cert": "/path/to/cert.pem"
}
```

To make a TLS connection to the local application's server,
mirrord's TLS client needs a server name. You can supply it manually like this:
```json
{
  "protocol": "tls",
  "server_name": "my.test.server.name"
}
```

If you don't supply the server name:

1. If `server_cert` is given, and the found end-entity certificate contains a valid server name,
   this server name will be used;
2. Otherwise, if the original client supplied an SNI extension, the server name from that
   extension will be used;
3. Otherwise, if the stolen request's URL contains a valid server name, that server name will be
   used;
4. Otherwise, `localhost` will be used.

##### feature.network.incoming.tls_delivery.protocol {#feature-network-incoming-tls_delivery-protocol}

Protocol to use when delivering the TLS traffic locally.


Path to a PEM file containing the certificate chain used by the local application's
TLS server.

This file must contain at least one certificate.
It can contain entries of other types, e.g private keys, which are ignored.

##### feature.network.incoming.tls_delivery.server_name {#feature-network-incoming-tls_delivery-server_name}

Server name to use when making a connection.

Must be a valid DNS name or an IP address.

##### feature.network.incoming.tls_delivery.trust_roots {#feature-network-incoming-tls_delivery-trust_roots}

Paths to PEM files and directories with PEM files containing allowed root certificates.

Directories are not traversed recursively.

Each certificate found in the files is treated as an allowed root.
The files can contain entries of other types, e.g private keys, which are ignored.

#### feature.network.ipv6 {#feature-network-ipv6}

Enable ipv6 support. Turn on if your application listens to incoming traffic over IPv6,
or connects to other services over IPv6.

#### feature.network.outgoing {#feature-network-outgoing}

Tunnel outgoing network operations through mirrord.

See the outgoing [reference](https://metalbear.com/mirrord/docs/reference/traffic/#outgoing) for more
details.

You can use either the `remote` or `local` value to turn outgoing traffic tunneling on or off.

```json
{
  "feature": {
    "network": {
      "outgoing": "remote"
    }
  }
}
```

Alternatively, you can use more fine-grained configuration. The `remote` and `local` config for
this feature are **mutually** exclusive.

```json
{
  "feature": {
    "network": {
      "outgoing": {
        "tcp": true,
        "udp": true,
        "ignore_localhost": false,
        "filter": {
          "local": ["tcp://1.1.1.0/24:1337", "1.1.5.0/24", "google.com", ":53"]
        },
        "unix_streams": "bear.+"
      }
    }
  }
}
```

##### feature.network.outgoing.filter {#feature.network.outgoing.filter}

Filters that are used to send specific traffic from either the remote pod or the local app

List of addresses/ports/subnets that should be sent through either the remote pod or local app,
depending how you set this up with either `remote` or `local`.

You may use this option to specify when outgoing traffic is sent from the remote pod (which
is the default behavior when you enable outgoing traffic), or from the local app (default when
you have outgoing traffic disabled).

Takes a list of values, such as:

- Only UDP traffic on subnet `1.1.1.0/24` on port 1337 will go through the remote pod.

```json
{
  "remote": ["udp://1.1.1.0/24:1337"]
}
```

- Only UDP and TCP traffic on resolved address of `google.com` on port `1337` and `7331` will go
  through the remote pod.
```json
{
  "remote": ["google.com:1337", "google.com:7331"]
}
```

- Only TCP traffic on `localhost` on port 1337 will go through the local app, the rest will be
  emmited remotely in the cluster.

```json
{
  "local": ["tcp://localhost:1337"]
}
```

- Only outgoing traffic on port `1337` and `7331` will go through the local app.
```json
{
  "local": [":1337", ":7331"]
}
```

Valid values follow this pattern: `[protocol]://[name|address|subnet/mask]:[port]`.

When filters are specified under `local`, matching traffic will go through the local app,
everything else will go through the remote pod.

When filters are specified under `remote`, matching traffic will go through the remote pod,
everything else will go through local.

##### feature.network.outgoing.ignore_localhost {#feature.network.outgoing.ignore_localhost}

Defaults to `false`.

##### feature.network.outgoing.tcp {#feature.network.outgoing.tcp}

Defaults to `true`.

##### feature.network.outgoing.udp {#feature.network.outgoing.udp}

Defaults to `true`.

##### feature.network.outgoing.unix_streams {#feature.network.outgoing.unix_streams}

Connect to these unix streams remotely (and to all other paths locally).

You can either specify a single value or an array of values.
Each value is interpreted as a regular expression
([Supported Syntax](https://docs.rs/regex/1.7.1/regex/index.html#syntax)).

When your application connects to a unix socket, the target address will be converted to a
string (non-utf8 bytes are replaced by a placeholder character) and matched against the set
of regexes specified here. If there is a match, mirrord will connect your application with
the target unix socket address on the target pod. Otherwise, it will leave the connection
to happen locally on your machine.

### feature.preview {#feature-preview}

Configuration for preview environments.

Controls the lifetime and creation behavior of preview sessions.

```json
{
  "feature": {
    "preview": {
      "image": "my-registry/my-app:latest",
      "ttl_mins": 60,
      "creation_timeout_secs": 60
    }
  }
}
```

### feature.preview.creation_timeout_secs {#feature-preview-creation_timeout_secs}

How long (in seconds) the CLI waits for the preview session to become ready.
If the session hasn't reached `Ready` within this time, the CLI deletes it.

### feature.preview.image {#feature-preview-image}

Container image to run in the preview pod.
The image must be pre-built and pushed to a registry accessible by the cluster.

### feature.preview.ttl_mins {#feature-preview-ttl_mins}

How long (in minutes) the preview session is allowed to live after creation.
The operator will terminate the session when this time elapses.

### feature.split_queues {#feature-split_queues}

Define filters to split queues by, and make your local application consume only messages
that match those filters.
If you don't specify any filter for a queue that is however declared in the
`MirrordWorkloadQueueRegistry` of the target you're using, a match-nothing filter
will be used, and your local application will not receive any messages from that queue.

```json
{
  "feature": {
    "split_queues": {
      "first-queue": {
        "queue_type": "SQS",
        "message_filter": {
          "wows": "so wows",
          "coolz": "^very"
        }
      },
      "second-queue": {
        "queue_type": "SQS",
        "message_filter": {
          "who": "you$"
        }
      },
      "third-queue": {
        "queue_type": "Kafka",
        "message_filter": {
          "who": "you$"
        }
      },
      "fourth-queue": {
        "queue_type": "Kafka",
        "message_filter": {
          "wows": "so wows",
          "coolz": "^very"
        }
      },
    }
  }
}
```

Amazon Simple Queue Service and Kafka are supported.

More queue types might be added in the future.

When a newer client sends a new filter kind to an older operator, that does not yet know
about that filter type, the filter will be deserialized to unknown.

## internal_proxy {#root-internal_proxy}

Configuration for the internal proxy mirrord spawns for each local mirrord session
that local layers use to connect to the remote agent

This is seldom used, but if you get `ConnectionRefused` errors, you might
want to increase the timeouts a bit.

```json
{
  "internal_proxy": {
    "start_idle_timeout": 30,
    "idle_timeout": 5
  }
}
```

### internal_proxy.idle_timeout {#internal_proxy-idle_timeout}

How much time to wait while we don't have any active connections before exiting.

Common cases would be running a chain of processes that skip using the layer
and don't connect to the proxy.

```json
{
  "internal_proxy": {
    "idle_timeout": 30
  }
}
```

### internal_proxy.json_log {#internal_proxy-json_log}

Whether the proxy should output logs in JSON format. If false, logs are output in
human-readable format.

Defaults to true.

### internal_proxy.log_destination {#internal_proxy-log_destination}

Set the log destination for the internal proxy.

1. If the provided path ends with a separator (`/` on UNIX, `\` on Windows), it will be
   treated as a path to directory where the log file should be created.
2. Otherwise, if the path exists, mirrord will check if it's a directory or not.
3. Otherwise, it will be treated as a path to the log file.

mirrord will auto create all parent directories.

Defaults to a randomized path inside the temporary directory.

### internal_proxy.log_level {#internal_proxy-log_level}

Set the log level for the internal proxy.

The value should follow the RUST_LOG convention (i.e `mirrord=trace`).

Defaults to `mirrord=info,warn`.

### internal_proxy.process_logging_interval {#internal_proxy-process_logging_interval}

How often to log information about connected processes in seconds.

This feature logs details about processes that are currently connected to the internal
proxy, including their PID, process name, command line, and connection status.

```json
{
  "internal_proxy": {
    "process_logging_interval": 60
  }
}
```

### internal_proxy.start_idle_timeout {#internal_proxy-start_idle_timeout}

How much time to wait for the first connection to the proxy in seconds.

Common cases would be running with dlv or any other debugger, which sets a breakpoint
on process execution, delaying the layer startup and connection to proxy.

```json
{
  "internal_proxy": {
    "start_idle_timeout": 60
  }
}
```

## key {#root-key}

An identifier for a mirrord session.

This key can be referenced in your configuration using the `{{ key }}` template variable.
For example, you can use it in HTTP filters: `"header_filter": "x-session: key-{{ key }}"`.

Priority (highest to lowest):
1. CLI argument: `mirrord exec --key my-key`
2. Config file: `{ "key": "my-key" }`
3. Fallback: A unique key is randomly generated if neither option is provided

```json
{
  "key": "my-session-key",
  "feature": {
    "network": {
      "incoming": {
        "http_filter": {
          "header_filter": "x-session: key-{{ key }}"
        }
      }
    }
  }
}
```

Session key for traffic filtering.

Distinguishes between user-provided keys (from CLI or config file)
and auto-generated keys (UUID v4).

Auto-generated UUID v4 when no key was provided.

Key provided by user via CLI argument or config file.

## kube_context {#root-kube_context}

Kube context to use from the kubeconfig file.
Will use current context if not specified.

```json
{
  "kube_context": "mycluster"
}
```

## kubeconfig {#root-kubeconfig}

Path to a kubeconfig file, if not specified, will use `KUBECONFIG`, or `~/.kube/config`, or
the in-cluster config.

```json
{
  "kubeconfig": "~/bear/kube-config"
}
```

## operator {#root-operator}

Whether mirrord should use the operator.
If not set, mirrord will first attempt to use the operator, but continue without it in case
of failure.

## profile {#root-profile}

Name of the mirrord profile to use.

To select a cluster-wide profile

```json
{
  "profile": "my-profile-name"
}
```

To select a namespaced profile

```json
{
  "profile": "my-namespace/my-profile-name"
}
```

## sip_binaries {#root-sip_binaries}

Binaries to patch (macOS SIP).

Use this when mirrord isn't loaded to protected binaries that weren't automatically
patched.

Runs `endswith` on the binary path (so `bash` would apply to any binary ending with `bash`
while `/usr/bin/bash` would apply only for that binary).

```json
{
  "sip_binaries": ["bash", "python"]
}
```

## skip_build_tools {#root-skip_build_tools}

Allows mirrord to skip build tools. Useful when running command lines that build and run
the application in a single command.

Defaults to `true`.

Build-Tools: `["as", "cc", "ld", "go", "air", "asm", "cc1", "cgo", "dlv", "gcc", "git",
"link", "math", "cargo", "hpack", "rustc", "compile", "collect2", "cargo-watch",
"debugserver"]`

## skip_extra_build_tools {#root-skip_build_tools}

Allows mirrord to skip the specified build tools. Useful when running command lines that
build and run the application in a single command.

Must also enable [`skip_build_tools`](#root-skip_build_tools) for this to take an effect.

It's similar to [`skip_processes`](#root-skip_processes), except that here it also skips
SIP patching.

Accepts a single value, or an array of values.

```json
{
 "skip_extra_build_tools": ["bash", "node"]
}
```

## skip_processes {#root-skip_processes}

Allows mirrord to skip unwanted processes.

Useful when process A spawns process B, and the user wants mirrord to operate only on
process B.
Accepts a single value, or an array of values.

```json
{
 "skip_processes": ["bash", "node"]
}
```

## skip_sip {#root-skip_sip}

Allows mirrord to skip patching (macOS SIP) unwanted processes.

When patching is skipped, mirrord will no longer be able to load into
the process and its child processes.

Defaults to `{ "skip_sip": "git" }`

When specified, the given value will replace the default list rather than
being added to.

## startup_retry {#root-startup_retry}

Controls how many times, and how often mirrord retries its initial Kubernetes API requests (e.g.
for resolving the target or connecting to the mirrord Operator).

If you're having cluster connectivity issues when **starting** mirrord, consider increasing
[`max_retries`](#startup_retry-max_retries) and changing both
[`min_ms`](#startup_retry-min_ms) and [`max_ms`](#startup_retry-max_ms) to have mirrord retry
some of its initial Kubernetes API requests.

```json
{
  "startup_retry": {
    "min_ms": 500,
    "max_ms": 5000,
    "max_retries": 2,
  }
}
```

### startup_retry.max_ms {#startup_retry-max_ms}

Sets the max interval (in milliseconds) of retries for Kubernetes API requests made by
mirrord during startup (e.g. for resolving the target or connecting to the mirrord
Operator).

Defaults to `5000` milliseconds.

### startup_retry.max_retries {#startup_retry-max_retries}

Sets the max amount of retries for Kubernetes API requests made by mirrord during startup
(e.g. for resolving the target or connecting to the mirrord Operator).

If you want to **disable** request retries, set this value to `0`.

Defaults to `2`.

### startup_retry.min_ms {#startup_retry-min_ms}

Sets the min interval (in milliseconds) of retries for Kubernetes API requests made by
mirrord during startup (e.g. for resolving the target or connecting to the mirrord
Operator).

Defaults to `500` milliseconds.

## target {#root-target}

Specifies the target and namespace to target.

The simplified configuration supports:

- `targetless`
- `pod/{pod-name}[/container/{container-name}]`;
- `deployment/{deployment-name}[/container/{container-name}]`;
- `rollout/{rollout-name}[/container/{container-name}]`;
- `job/{job-name}[/container/{container-name}]`;
- `cronjob/{cronjob-name}[/container/{container-name}]`;
- `statefulset/{statefulset-name}[/container/{container-name}]`;
- `service/{service-name}[/container/{container-name}]`;

Please note that:

- `job`, `cronjob`, `statefulset` and `service` targets require the mirrord Operator
- `job` and `cronjob` targets require the [`copy_target`](#feature-copy_target) feature

Shortened setup with a target:

```json
{
 "target": "pod/bear-pod"
}
```

The setup above will result in a session targeting the `bear-pod` Kubernetes pod
in the user's default namespace. A target container will be chosen by mirrord.

Shortened setup with a target container:

```json
{
  "target": "pod/bear-pod/container/bear-pod-container"
}
```

The setup above will result in a session targeting the `bear-pod-container` container
in the `bear-pod` Kubernetes pod in the user's default namespace.

Complete setup with a target container:

```json
{
 "target": {
   "path": {
     "pod": "bear-pod",
     "container": "bear-pod-container"
   },
   "namespace": "bear-pod-namespace"
 }
}
```

The setup above will result in a session targeting the `bear-pod-container` container
in the `bear-pod` Kubernetes pod in the `bear-pod-namespace` namespace.

Setup with a namespace for a targetless run:

```json
{
  "target": {
    "path": "targetless",
    "namespace": "bear-namespace"
  }
}
```

The setup above will result in a session without any target.
Remote outgoing traffic and DNS will be done from the `bear-namespace` namespace.

### target.namespace {#target-namespace}

Namespace where the target lives.

For targetless runs, this the namespace in which remote networking is done.

Defaults to the Kubernetes user's default namespace (defined in Kubernetes context).

### target.path {#target-path}

Specifies the Kubernetes resource to target.

If not given, defaults to `targetless`.

Note: targeting services and whole workloads is available only in mirrord for Teams.
If you target a workload without the mirrord Operator, it will choose a random pod replica
to work with.

Supports:
- `targetless`
- `pod/{pod-name}[/container/{container-name}]`;
- `deployment/{deployment-name}[/container/{container-name}]`;
- `rollout/{rollout-name}[/container/{container-name}]`;
- `job/{job-name}[/container/{container-name}]`; (requires mirrord Operator and the
  [`copy_target`](#feature-copy_target) feature)
- `cronjob/{cronjob-name}[/container/{container-name}]`; (requires mirrord Operator and the
  [`copy_target`](#feature-copy_target) feature)
- `statefulset/{statefulset-name}[/container/{container-name}]`; (requires mirrord
  Operator)
- `service/{service-name}[/container/{container-name}]`; (requires mirrord Operator)
- `replicaset/{replicaset-name}[/container/{container-name}]`; (requires mirrord Operator)

## telemetry {#root-telemetry}
Controls whether or not mirrord sends telemetry data to MetalBear cloud.
Telemetry sent doesn't contain personal identifiers or any data that
should be considered sensitive. It is used to improve the product.
[For more information](https://github.com/metalbear-co/mirrord/blob/main/TELEMETRY.md)

## traceparent {#root-traceparent}

OpenTelemetry (OTel) / W3C trace context. This is used in HTTP requests sent to the
operator to manually set the parent trace of the entry point, which can help when
processing traces.
See [OTel docs](https://opentelemetry.io/docs/specs/otel/context/env-carriers/#environment-variable-names)

Only relevant for use with the operator. For more details, read the [docs on monitoring](https://metalbear.com/mirrord/docs/managing-mirrord/monitoring).

## use_proxy {#root-use_proxy}

When disabled, mirrord will remove `HTTP[S]_PROXY` env variables before
doing any network requests. This is useful when the system sets a proxy
but you don't want mirrord to use it.
This also applies to the mirrord process (as it just removes the env).
If the remote pod sets this env, the mirrord process will still use it.
