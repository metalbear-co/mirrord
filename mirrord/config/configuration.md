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
    "network_interface": "eth0",
    "flush_connections": true
  },
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

Defaults to `false`.

## operator {#root-operator}

Whether mirrord should use the operator.
If not set, mirrord will first attempt to use the operator, but continue without it in case
of failure.

## skip_build_tools {#root-skip_build_tools}

Allows mirrord to skip build tools. Useful when running command lines that build and run
the application in a single command.

Defaults to `true`.

Build-Tools: `["as", "cc", "ld", "go", "air", "asm", "cc1", "cgo", "dlv", "gcc", "git",
"link", "math", "cargo", "hpack", "rustc", "compile", "collect2", "cargo-watch",
"debugserver"]`

## telemetry {#root-telemetry}
Controls whether or not mirrord sends telemetry data to MetalBear cloud.
Telemetry sent doesn't contain personal identifiers or any data that
should be considered sensitive. It is used to improve the product.
[For more information](https://github.com/metalbear-co/mirrord/blob/main/TELEMETRY.md)

## use_proxy {#root-use_proxy}

When disabled, mirrord will remove `HTTP[S]_PROXY` env variables before
doing any network requests. This is useful when the system sets a proxy
but you don't want mirrord to use it.
This also applies to the mirrord process (as it just removes the env).
If the remote pod sets this env, the mirrord process will still use it.

## connect_tcp {#root-connect_tpc}

IP:PORT to connect to instead of using k8s api, for testing purposes.

```json
{
  "connect_tcp": "10.10.0.100:7777"
}
```

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

## sip_binaries {#root-sip_binaries}

Binaries to patch (macOS SIP).

Use this when mirrord isn't loaded to protected binaries that weren't automatically
patched.

Runs `endswith` on the binary path (so `bash` would apply to any binary ending with `bash`
while `/usr/bin/bash` would apply only for that binary).

```json
{
 "sip_binaries": "bash;python"
}
```

## skip_processes {#root-skip_processes}

Allows mirrord to skip unwanted processes.

Useful when process A spawns process B, and the user wants mirrord to operate only on
process B.
Accepts a single value, or multiple values separated by `;`.

```json
{
 "skip_processes": "bash;node"
}
```

## agent {#root-agent}
Configuration for the mirrord-agent pod that is spawned in the Kubernetes cluster.

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
    "network_interface": "eth0",
    "flush_connections": false
  }
}
```

### agent.communication_timeout {#agent-communication_timeout}

Controls how long the agent lives when there are no connections.

Each connection has its own heartbeat mechanism, so even if the local application has no
messages, the agent stays alive until there are no more heartbeat messages.

### agent.startup_timeout {#agent-startup_timeout}

Controls how long to wait for the agent to finish initialization.

If initialization takes longer than this value, mirrord exits.

Defaults to `60`.

### agent.ttl {#agent-ttl}

Controls how long the agent pod persists for after the agent exits (in seconds).

Can be useful for collecting logs.

Defaults to `1`.

### agent.check_out_of_pods {#agent-check_out_of_pods}

Determine if to check whether there is room for agent job in target node. (Not applicable
when using ephemeral containers feature)

Can be disabled if the check takes too long and you are sure there is enough resources on
each node

### agent.ephemeral {#agent-ephemeral}

Runs the agent as an
[ephemeral container](https://kubernetes.io/docs/concepts/workloads/pods/ephemeral-containers/)

Defaults to `false`.

### agent.flush_connections {#agent-flush_connections}

Flushes existing connections when starting to steal, might fix issues where connections
aren't stolen (due to being already established)

Defaults to `true`.

### agent.json_log {#agent-json_log}

Controls whether the agent produces logs in a human-friendly format, or json.

```json
{
  "agent": {
    "json_log": true
  }
}
```

### agent.nftables {#agent-nftables}

Use iptables-nft instead of iptables-legacy.
Defaults to `false`.

Needed if your mesh uses nftables instead of iptables-legacy,

### agent.privileged {#agent-privileged}

Run the mirror agent as privileged container.
Defaults to `false`.

Might be needed in strict environments such as Bottlerocket.

### agent.annotations {#agent-annotations}

Allows setting up custom annotations for the agent Job and Pod.

```json
{
  "annotations": { "cats.io/inject": "enabled" }
}
```

### agent.image_pull_policy {#agent-image_pull_policy}

Controls when a new agent image is downloaded.

Supports `"IfNotPresent"`, `"Always"`, `"Never"`, or any valid kubernetes
[image pull policy](https://kubernetes.io/docs/concepts/containers/images/#image-pull-policy)

Defaults to `"IfNotPresent"`

### agent.labels {#agent-labels}

Allows setting up custom labels for the agent Job and Pod.

```json
{
  "labels": { "user": "meow", "state": "asleep" }
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

### agent.namespace {#agent-namespace}

Namespace where the agent shall live.
Note: Doesn't work with ephemeral containers.
Defaults to the current kubernetes namespace.

### agent.network_interface {#agent-network_interface}

Which network interface to use for mirroring.

The default behavior is try to access the internet and use that interface. If that fails
it uses `eth0`.

### agent.tolerations {#agent-tolerations}

Set pod tolerations. (not with ephemeral agents)
Default is
```json
[
  {
    "operator": "Exists"
  }
]
```

Set to an empty array to have no tolerations at all

### agent.dns {#agent-dns}

### agent.disabled_capabilities {#agent-disabled_capabilities}

Disables specified Linux capabilities for the agent container.
If nothing is disabled here, agent uses `NET_ADMIN`, `NET_RAW`, `SYS_PTRACE` and
`SYS_ADMIN`.

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

### agent.image {#agent-image}

Name of the agent's docker image.

Useful when a custom build of mirrord-agent is required, or when using an internal
registry.

Defaults to the latest stable image `"ghcr.io/metalbear-co/mirrord:latest"`.

```json
{
  "image": "internal.repo/images/mirrord:latest"
}
```

Complete setup:

```json
{
  "image": {
    "registry": "internal.repo/images/mirrord",
    "tag": "latest"
  }
}
```

### agent.resources {#agent-resources}

Set pod resource reqirements. (not with ephemeral agents)
Default is
```json
{
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
```

## target {#root-target}
Specifies the target and namespace to mirror, see [`path`](#target-path) for a list of
accepted values for the `target` option.

The simplified configuration supports:

- `pod/{sample-pod}/[container]/{sample-container}`;
- `podname/{sample-pod}/[container]/{sample-container}`;
- `deployment/{sample-deployment}/[container]/{sample-container}`;

Shortened setup:

```json
{
 "target": "pod/bear-pod"
}
```

Complete setup:

```json
{
 "target": {
   "path": {
     "pod": "bear-pod"
   },
   "namespace": "default"
 }
}
```

### target.namespace {#target-namespace}

Namespace where the target lives.

Defaults to `"default"`.

### target.path {#target-path}

Specifies the running pod (or deployment) to mirror.

Note: Deployment level steal/mirroring is available only in mirrord for Teams
If you use it without it, it will choose a random pod replica to work with.

Supports:
- `pod/{sample-pod}`;
- `podname/{sample-pod}`;
- `deployment/{sample-deployment}`;
- `container/{sample-container}`;
- `containername/{sample-container}`.
- `job/{sample-job}` (only when [`copy_target`](#feature-copy_target) is enabled).

# feature {#root-feature}
Controls mirrord features.

See the
[technical reference, Technical Reference](https://mirrord.dev/docs/reference/)
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

## feature.hostname {#feature-hostname}

Should mirrord return the hostname of the target pod when calling `gethostname`

## feature.fs {#feature-fs}
Allows the user to specify the default behavior for file operations:

1. `"read"` - Read from the remote file system (default)
2. `"write"` - Read/Write from the remote file system.
3. `"local"` - Read from the local file system.
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

The logic for choosing the behavior is as follows:

1. Check if one of the patterns match the file path, do the corresponding action. There's
no specified order if two lists match the same path, we will use the first one (and we
do not guarantee what is first).

    **Warning**: Specifying the same path in two lists is unsupported and can lead to undefined
    behaviour.

2. There are pre-defined exceptions to the set FS mode.
    1. Paths that match [the patterns defined here](https://github.com/metalbear-co/mirrord/tree/latest/mirrord/layer/src/file/filter/read_local_by_default.rs)
       are read locally by default.
    2. Paths that match [the patterns defined here](https://github.com/metalbear-co/mirrord/tree/latest/mirrord/layer/src/file/filter/read_remote_by_default.rs)
       are read remotely by default when the mode is `localwithoverrides`.
    3. Paths that match [the patterns defined here](https://github.com/metalbear-co/mirrord/tree/latest/mirrord/layer/src/file/filter/not_found_by_default.rs)
       under the running user's home directory will not be found by the application when the
       mode is not `local`.

    In order to override that default setting for a path, or a pattern, include it the
    appropriate pattern set from above. E.g. in order to read files under `/etc/` remotely even
    though it is covered by [the set of patterns that are read locally by default](https://github.com/metalbear-co/mirrord/tree/latest/mirrord/layer/src/file/filter/read_local_by_default.rs),
    add `"^/etc/."` to the `read_only` set.

3. If none of the above match, use the default behavior (mode).

For more information, check the file operations
[technical reference](https://mirrord.dev/docs/reference/fileops/).

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

### feature.fs.local {#feature-fs-local}

Specify file path patterns that if matched will be opened locally.

### feature.fs.not_found {#feature-fs-not_found}

Specify file path patterns that if matched will be treated as non-existent.

### feature.fs.read_only {#feature-fs-read_only}

Specify file path patterns that if matched will be read from the remote.
if file matching the pattern is opened for writing or read/write it will be opened locally.

### feature.fs.read_write {#feature-fs-read_write}

Specify file path patterns that if matched will be read and written to the remote.

### feature.fs.mode {#feature-fs-mode}
Configuration for enabling read-only or read-write file operations.

These options are overriden by user specified overrides and mirrord default overrides.

If you set [`"localwithoverrides"`](#feature-fs-mode-localwithoverrides) then some files
can be read/write remotely based on our default/user specified.
Default option for general file configuration.

The accepted values are: `"local"`, `"localwithoverrides`, `"read"`, or `"write`.

## feature.env {#feature-env}
Allows the user to set or override the local process' environment variables with the ones
from the remote pod.

Which environment variables to load from the remote pod are controlled by setting either
[`include`](#feature-env-include) or [`exclude`](#feature-env-exclude).

See the environment variables [reference](https://mirrord.dev/docs/reference/env/) for more details.

```json
{
  "feature": {
    "env": {
      "include": "DATABASE_USER;PUBLIC_ENV;MY_APP_*",
      "exclude": "DATABASE_PASSWORD;SECRET_ENV",
      "override": {
        "DATABASE_CONNECTION": "db://localhost:7777/my-db",
        "LOCAL_BEAR": "panda"
      }
    }
  }
}
```

### feature.env.load_from_process {#feature-env-load_from_process}

Allows for changing the way mirrord loads remote environment variables.
If set, the variables are fetched after the user application is started.

This setting is meant to resolve issues when using mirrord via the IntelliJ plugin on WSL
and the remote environment contains a lot of variables.

### feature.env.exclude {#feature-env-exclude}

Include the remote environment variables in the local process that are **NOT** specified by
this option.
Variable names can be matched using `*` and `?` where `?` matches exactly one occurrence of
any character and `*` matches arbitrary many (including zero) occurrences of any character.

Some of the variables that are excluded by default:
`PATH`, `HOME`, `HOMEPATH`, `CLASSPATH`, `JAVA_EXE`, `JAVA_HOME`, `PYTHONPATH`.

Can be passed as a list or as a semicolon-delimited string (e.g. `"VAR;OTHER_VAR"`).

### feature.env.include {#feature-env-include}

Include only these remote environment variables in the local process.
Variable names can be matched using `*` and `?` where `?` matches exactly one occurrence of
any character and `*` matches arbitrary many (including zero) occurrences of any character.

Can be passed as a list or as a semicolon-delimited string (e.g. `"VAR;OTHER_VAR"`).

Some environment variables are excluded by default (`PATH` for example), including these
requires specifying them with `include`

### feature.env.override {#feature-env-override}

Allows setting or overriding environment variables (locally) with a custom value.

For example, if the remote pod has an environment variable `REGION=1`, but this is an
undesirable value, it's possible to use `override` to set `REGION=2` (locally) instead.

### feature.env.unset {#feature-env-unset}

Allows unsetting environment variables in the executed process.

This is useful for when some system/user-defined environment like `AWS_PROFILE` make the
application behave as if it's running locally, instead of using the remote settings.
The unsetting happens from extension (if possible)/CLI and when process initializes.
In some cases, such as Go the env might not be able to be modified from the process itself.
This is case insensitive, meaning if you'd put `AWS_PROFILE` it'd unset both `AWS_PROFILE`
and `Aws_Profile` and other variations.

## feature.network {#feature-network}
Controls mirrord network operations.

See the network traffic [reference](https://mirrord.dev/docs/reference/traffic/)
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
      "dns": false
    }
  }
}
```

### feature.network.dns {#feature-network-dns}

Resolve DNS via the remote pod.

Defaults to `true`.

- Caveats: DNS resolving can be done in multiple ways, some frameworks will use
`getaddrinfo`, while others will create a connection on port `53` and perform a sort
of manual resolution. Just enabling the `dns` feature in mirrord might not be enough.
If you see an address resolution error, try enabling the [`fs`](#feature-fs) feature,
and setting `read_only: ["/etc/resolv.conf"]`.

### feature.network.incoming {#feature-network-incoming}
Controls the incoming TCP traffic feature.

See the incoming [reference](https://mirrord.dev/docs/reference/traffic/#incoming) for more
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

#### feature.network.incoming.ignore_ports {#feature-network-incoming-ignore_ports}

Ports to ignore when mirroring/stealing traffic, these ports will remain local.

Can be especially useful when
[`feature.network.incoming.mode`](#feature-network-incoming-mode) is set to `"steal"`,
and you want to avoid redirecting traffic from some ports (for example, traffic from
a health probe, or other heartbeat-like traffic).

Mutually exclusive with [`feature.network.incoming.ports`](#feature-network-ports).

#### feature.network.incoming.listen_ports {#feature-network-incoming-listen_ports}

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

#### feature.network.incoming.port_mapping {#feature-network-incoming-port_mapping}

Mapping for local ports to remote ports.

This is useful when you want to mirror/steal a port to a different port on the remote
machine. For example, your local process listens on port `9333` and the container listens
on port `80`. You'd use `[[9333, 80]]`

#### feature.network.incoming.ports {#feature-network-incoming-ports}

List of ports to mirror/steal traffic from. Other ports will remain local.

Mutually exclusive with
[`feature.network.incoming.ignore_ports`](#feature-network-ignore_ports).

#### feature.network.incoming.ignore_localhost {#feature-network-incoming-ignore_localhost}

#### feature.network.incoming.mode {#feature-network-incoming-mode}
Allows selecting between mirrorring or stealing traffic.

Can be set to either `"mirror"` (default), `"steal"` or `"off"`.

- `"mirror"`: Sniffs on TCP port, and send a copy of the data to listeners.
- `"off"`: Disables the incoming network feature.
- `"steal"`: Supports 2 modes of operation:

1. Port traffic stealing: Steals all TCP data from a
  port, which is selected whenever the
user listens in a TCP socket (enabling the feature is enough to make this work, no
additional configuration is needed);

2. HTTP traffic stealing: Steals only HTTP traffic, mirrord tries to detect if the incoming
data on a port is HTTP (in a best-effort kind of way, not guaranteed to be HTTP), and
steals the traffic on the port if it is HTTP;

#### feature.network.incoming.on_concurrent_steal {#feature-network-incoming-on_concurrent_steal}
(Operator Only): Allows overriding port locks

Can be set to either `"continue"` or `"override"`.

- `"continue"`: Continue with normal execution
- `"override"`: If port lock detected then override it with new lock and force close the
  original locking connection.

#### feature.network.incoming.http_filter {#feature-network-incoming-http-filter}
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

##### feature.network.incoming.http_filter.header_filter {#feature-network-incoming-http-header-filter}


Supports regexes validated by the
[`fancy-regex`](https://docs.rs/fancy-regex/latest/fancy_regex/) crate.

The HTTP traffic feature converts the HTTP headers to `HeaderKey: HeaderValue`,
case-insensitive.

##### feature.network.incoming.http_filter.path_filter {#feature-network-incoming-http-path-filter}


Supports regexes validated by the
[`fancy-regex`](https://docs.rs/fancy-regex/latest/fancy_regex/) crate.

Case-insensitive.

##### feature.network.incoming.http_filter.ports {#feature-network-incoming-http_filter-ports}

Activate the HTTP traffic filter only for these ports.

Other ports will *not* be stolen, unless listed in
[`feature.network.incoming.ports`](#feature-network-incoming-ports).

Set to [80, 8080] by default.

### feature.network.outgoing {#feature-network-outgoing}
Tunnel outgoing network operations through mirrord.

See the outgoing [reference](https://mirrord.dev/docs/reference/traffic/#outgoing) for more
details.

The `remote` and `local` config for this feature are **mutually** exclusive.

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

#### feature.network.outgoing.ignore_localhost {#feature.network.outgoing.ignore_localhost}

Defaults to `false`.

#### feature.network.outgoing.tcp {#feature.network.outgoing.tcp}

Defaults to `true`.

#### feature.network.outgoing.udp {#feature.network.outgoing.udp}

Defaults to `true`.

#### feature.network.outgoing.unix_streams {#feature.network.outgoing.unix_streams}

Connect to these unix streams remotely (and to all other paths locally).

You can either specify a single value or an array of values.
Each value is interpreted as a regular expression
([Supported Syntax](https://docs.rs/regex/1.7.1/regex/index.html#syntax)).

When your application connects to a unix socket, the target address will be converted to a
string (non-utf8 bytes are replaced by a placeholder character) and matched against the set
of regexes specified here. If there is a match, mirrord will connect your application with
the target unix socket address on the target pod. Otherwise, it will leave the connection
to happen locally on your machine.

#### feature.network.outgoing.filter {#feature.network.outgoing.filter}

Unstable: the precise syntax of this config is subject to change.
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

- Only UDP and TCP traffic on resolved address of `google.com` on port `1337` and `7331`
will go through the remote pod.
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

## feature.copy_target {#feature-copy_target}

Creates a new copy of the target. mirrord will use this copy instead of the original target
(e.g. intercept network traffic). This feature requires a [mirrord operator](https://mirrord.dev/docs/overview/teams/).

This feature is not compatible with rollout targets and running without a target
(`targetless` mode).
Allows the user to target a pod created dynamically from the orignal [`target`](#target).
The new pod inherits most of the original target's specification, e.g. labels.

```json
{
  "feature": {
    "copy_target": {
      "scale_down": true
    }
  }
}
```

```json
{
  "feature": {
    "copy_target": true
  }
}
```


### feature.copy_target.scale_down {#feature-copy_target-scale_down}

If this option is set, mirrord will scale down the target deployment to 0 for the time
the copied pod is alive.

This option is compatible only with deployment targets.
```json
    {
      "scale_down": true
    }
```

# experimental {#root-experimental}
mirrord Experimental features.
This shouldn't be used unless someone from MetalBear/mirrord tells you to.

## _experimental_ readlink {#fexperimental-readlink}

Enables the `readlink` hook.

## _experimental_ tcp_ping4_mock {#fexperimental-tcp_ping4_mock}

<https://github.com/metalbear-co/mirrord/issues/2421#issuecomment-2093200904>

# internal_proxy {#root-internal_proxy}
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

### internal_proxy.log_destination {#internal_proxy-log_destination}
Set the log file destination for the internal proxy.

### internal_proxy.log_level {#internal_proxy-log_level}
Set the log level for the internal proxy.
RUST_LOG convention (i.e `mirrord=trace`)
will only be used if log_destination is set

