 mirrord allows for a high degree of customization when it comes to which features you want to
 enable, and how they should function.

 All of the configuration fields have a default value, so a minimal configuration would be no
 configuration at all.

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

 ### Complete `config.json` {#root-complete}

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
     "namespace": "default",
     "image": "ghcr.io/metalbear-co/mirrord:latest",
     "image_pull_policy": "IfNotPresent",
     "image_pull_secrets": [ { "secret-key": "secret" } ],
     "ttl": 30,
     "ephemeral": false,
     "communication_timeout": 30,
     "startup_timeout": 360,
     "network_interface": "eth0",
     "pause": false,
     "flush_connections": true
   },
   "feature": {
     "env": {
       "include": "DATABASE_USER;PUBLIC_ENV",
       "exclude": "DATABASE_PASSWORD;SECRET_ENV",
       "overrides": {
         "DATABASE_CONNECTION": "db://localhost:7777/my-db",
         "LOCAL_BEAR": "panda"
       }
     },
     "fs": {
       "mode": "write",
       "read_write": ".+\.json" ,
       "read_only": [ ".+\.yaml", ".+important-file\.txt" ],
       "local": [ ".+\.js", ".+\.mjs" ]
     },
     "network": {
       "incoming": {
         "mode": "steal",
         "http_header_filter": {
           "filter": "host: api\..+",
           "ports": [80, 8080]
         },
         "port_mapping": [[ 7777, 8888 ]],
         "ignore_localhost": false,
         "ignore_ports": [9999, 10000]
       },
       "outgoing": {
         "tcp": true,
         "udp": true,
         "ignore_localhost": false,
         "unix_streams": "bear.+"
       },
       "dns": false
     },
     "capture_error_trace": false
   },
   "operator": true,
   "kubeconfig": "~/.kube/config",
   "sip_binaries": "bash"
 }
 ```

 # Options {#root-options}

 ## pause {#root-pause}

 Controls target pause feature. Unstable.

 With this feature enabled, the remote container is paused while this layer is connected to
 the agent.

 Defaults to `false`.

 ## operator {#root-operator}

 Allow to lookup if operator is installed on cluster and use it.

 Defaults to `true`.

 ## accept_invalid_certificates {#root-accept_invalid_certificates}

 Controls whether or not mirrord accepts invalid TLS certificates (e.g. self-signed
 certificates).

 Defaults to `false`.

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

 ## connect_tcp {#root-connect_tpc}

 IP:PORT to connect to instead of using k8s api, for testing purposes.

 ```json
 {
   "connect_tcp": "10.10.0.100:7777"
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

 ## agent {#root-agent}

 Configuration for the mirrord-agent pod that is spawned in the Kubernetes cluster.

 We provide sane defaults for this option, so you don't have to set up anything here.

 ```json
 {
   "agent": {
     "log_level": "info",
     "namespace": "default",
     "image": "ghcr.io/metalbear-co/mirrord:latest",
     "image_pull_policy": "IfNotPresent",
     "image_pull_secrets": [ { "secret-key": "secret" } ],
     "ttl": 30,
     "ephemeral": false,
     "communication_timeout": 30,
     "startup_timeout": 360,
     "network_interface": "eth0",
     "pause": false,
     "flush_connections": false,
   }
 }
 ```

 ### agent.communication_timeout {#agent-communication_timeout}

 Controls how long the agent lives when there are no connections.

 Each connection has its own heartbeat mechanism, so even if the local application has no
 messages, the agent stays alive until there are no more heartbeat messages.

 ### agent.ttl {#agent-ttl}

 Controls how long the agent pod persists for after the agent exits (in seconds).

 Can be useful for collecting logs.

 Defaults to `1`.

 ### agent.startup_timeout {#agent-startup_timeout}

 Controls how long to wait for the agent to finish initialization.

 If initialization takes longer than this value, mirrord exits.

 Defaults to `60`.

 ### agent.flush_connections {#agent-flush_connections}

 Flushes existing connections when starting to steal, might fix issues where connections
 aren't stolen (due to being already established)

 Defaults to `true`.

 ### agent.ephemeral {#agent-ephemeral}

 Runs the agent as an
 [ephemeral container](https://kubernetes.io/docs/concepts/workloads/pods/ephemeral-containers/)

 Defaults to `false`.

 ### agent.network_interface {#agent-network_interface}

 Which network interface to use for mirroring.

 The default behavior is try to access the internet and use that interface. If that fails
 it uses `eth0`.

 ### agent.namespace {#agent-namespace}

 Namespace where the agent shall live.

 Defaults to the current kubernetes namespace.

 ### agent.image_pull_policy {#agent-image_pull_policy}

 Controls when a new agent image is downloaded.

 Supports `"IfNotPresent"`, `"Always"`, `"Never"`, or any valid kubernetes
 [image pull policy](https://kubernetes.io/docs/concepts/containers/images/#image-pull-policy)

 Defaults to `"IfNotPresent"`

 ### agent.image_pull_secrets {#agent-image_pull_secrets}

 List of secrets the agent pod has access to.

 Takes an array of hash with the format `{ name: <secret-name> }`.

 Read more [here](https://kubernetes.io/docs/concepts/containers/images/).

 ```json
 {
   "agent": {
     "image_pull_secrets": [
       { "very-secret": "secret-key" },
       { "very-secret": "keep-your-secrets" }
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
   "agent": {
     "image": "internal.repo/images/mirrord:latest"
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

 ### target.path {#target-path}

 Specifies the running pod (or deployment) to mirror.

 Supports:
 - `pod/{sample-pod}`;
 - `podname/{sample-pod}`;
 - `deployment/{sample-deployment}`;
 - `container/{sample-container}`;
 - `containername/{sample-container}`.

 ### target.namespace {#target-namespace}

 Namespace where the target lives.

 Defaults to `"default"`.

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
       "overrides": {
         "DATABASE_CONNECTION": "db://localhost:7777/my-db",
         "LOCAL_BEAR": "panda"
       }
     },
     "fs": {
       "mode": "write",
       "read_write": ".+\.json" ,
       "read_only": [ ".+\.yaml", ".+important-file\.txt" ],
       "local": [ ".+\.js", ".+\.mjs" ]
     },
     "network": {
       "incoming": {
         "mode": "steal",
         "http_header_filter": {
           "filter": "host: api\..+",
           "ports": [80, 8080]
         },
         "port_mapping": [[ 7777, 8888 ]],
         "ignore_localhost": false,
         "ignore_ports": [9999, 10000]
       },
       "outgoing": {
         "tcp": true,
         "udp": true,
         "ignore_localhost": false,
         "unix_streams": "bear.+"
       },
       "dns": false
     },
     "capture_error_trace": false
   }
 }
 ```

 ## feature.capture_error_trace {#feature-capture_error_trace}

 Controls the crash reporting feature.

 With this feature enabled, mirrord generates a nice crash report log.

 Defaults to `false`.

 ## feature.fs {#feature-fs}
 Allows the user to specify the default behavior for file operations:

 1. `"read"` - Read from the remote file system (default)
 2. `"write"` - Read/Write from the remote file system.
 3. `"local"` - Read from the local file system.
 5. `"disable"` - Disable file operations.

 Besides the default behavior, the user can specify behavior for specific regex patterns.
 Case insensitive.

 1. `"read_write"` - List of patterns that should be read/write remotely.
 2. `"read_only"` - List of patterns that should be read only remotely.
 3. `"local"` - List of patterns that should be read locally.

 The logic for choosing the behavior is as follows:

 1. Check if one of the patterns match the file path, do the corresponding action. There's
 no specified order if two lists match the same path, we will use the first one (and we
 do not guarantee what is first).

 **Warning**: Specifying the same path in two lists is unsupported and can lead to undefined
 behaviour.

 2. Check our "special list" - we have an internal at compile time list
 for different behavior based on patterns to provide better UX.

 3. If none of the above match, use the default behavior (mode).

 For more information, check the file operations
 [technical reference](https://mirrord.dev/docs/reference/fileops/).

 ```json
 {
   "feature": {
     "fs": {
       "mode": "write",
       "read_write": ".+\.json" ,
       "read_only": [ ".+\.yaml", ".+important-file\.txt" ],
       "local": [ ".+\.js", ".+\.mjs" ]
     }
   }
 }
 ```

 ### feature.fs.local {#feature-fs-local}

 Specify file path patterns that if matched will be opened locally.

 ### feature.fs.read_write {#feature-fs-read_write}

 Specify file path patterns that if matched will be read and written to the remote.

 ### feature.fs.read_only {#feature-fs-read_only}

 Specify file path patterns that if matched will be read from the remote.
 if file matching the pattern is opened for writing or read/write it will be opened locally.

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
       "include": "DATABASE_USER;PUBLIC_ENV",
       "exclude": "DATABASE_PASSWORD;SECRET_ENV",
       "override": {
         "DATABASE_CONNECTION": "db://localhost:7777/my-db",
         "LOCAL_BEAR": "panda"
       }
     }
   }
 }
 ```

 ### feature.env.include {#feature-env-include}

 Include only these remote environment variables in the local process.

 Value is a list separated by ";".

 Some environment variables are excluded by default (`PATH` for example), including these
 requires specifying them with `include`

 ### feature.env.exclude {#feature-env-exclude}

 Include the remote environment variables in the local process that are **NOT** specified by
 this option.

 Some of the variables that are excluded by default:
 `PATH`, `HOME`, `HOMEPATH`, `CLASSPATH`, `JAVA_EXE`, `JAVA_HOME`, `PYTHONPATH`.

 Value is a list separated by ";".

 ### feature.env.override {#feature-env-override}

 Allows setting or overriding environment variables (locally) with a custom value.

 For example, if the remote pod has an environment variable `REGION=1`, but this is an
 undesirable value, it's possible to use `overrides` to set `REGION=2` (locally) instead.

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
         "http_header_filter": {
           "filter": "host: api\..+",
           "ports": [80, 8080]
         },
         "port_mapping": [[ 7777, 8888 ]],
         "ignore_localhost": false,
         "ignore_ports": [9999, 10000]
       },
       "outgoing": {
         "tcp": true,
         "udp": true,
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

 ### feature.network.incoming {#feature-network-incoming}
 Controls the incoming TCP traffic feature.

 See the incoming [reference](https://mirrord.dev/docs/reference/traffic/#incoming) for more
 details.

 Incoming traffic supports 2 modes of operation:

 1. Mirror (**default**): Sniffs the TCP data from a port, and forwards a copy to the interested
 listeners;

 2. Steal: Captures the TCP data from a port, and forwards it to the local process, see
 [`"mode": "steal"`](#feature-network-incoming-mode);

 Steals all the incoming traffic:

 ```json
 {
   "feature": {
     "network": {
       "incoming": "steal"
     }
   }
 }
 ```

 Steals only traffic that matches the
 [`http_header_filter`](#feature-network-incoming-http_header_filter) (steals only HTTP traffic).

 ```json
 {
   "feature": {
     "network": {
       "incoming": {
         "mode": "steal",
         "http_header_filter": {
           "filter": "host: api\..+",
           "ports": [80, 8080]
         },
         "port_mapping": [[ 7777, 8888 ]],
         "ignore_localhost": false,
         "ignore_ports": [9999, 10000]
       }
     }
   }
 }
 ```

 #### feature.network.incoming.ignore_ports {#feature-network-incoming-ignore_ports}

 Ports to ignore when mirroring/stealing traffic, these ports will remain local.

 Can be especially useful when
 [`feature.network.incoming.mode`](#feature-network-incoming-mode) is set to `"stealer"
 `, and you want to avoid redirecting traffic from some ports (for example, traffic from
 a health probe, or other heartbeat-like traffic).

 #### feature.network.incoming.port_mapping {#feature-network-incoming-port_mapping}

 Mapping for local ports to remote ports.

 This is useful when you want to mirror/steal a port to a different port on the remote
 machine. For example, your local process listens on port `9333` and the container listens
 on port `80`. You'd use `[[9333, 80]]`

 #### feature.network.incoming.ignore_localhost {#feature-network-incoming-ignore_localhost}

 #### feature.network.incoming.mode {#feature-network-incoming-mode}
 Allows selecting between mirrorring or stealing traffic.

 Can be set to either `"mirror"` (default) or `"steal"`.

 - `"mirror"`: Sniffs on TCP port, and send a copy of the data to listeners.
 - `"steal"`: Supports 2 modes of operation:

 1. Port traffic stealing: Steals all TCP data from a
   port, which is selected whenever the
 user listens in a TCP socket (enabling the feature is enough to make this work, no
 additional configuration is needed);

 2. HTTP traffic stealing: Steals only HTTP traffic, mirrord tries to detect if the incoming
 data on a port is HTTP (in a best-effort kind of way, not guaranteed to be HTTP), and
 steals the traffic on the port if it is HTTP;

 #### feature.network.incoming.filter {#feature-network-incoming-filter}
 Filter configuration for the HTTP traffic stealer feature.

 Allows the user to set a filter (regex) for the HTTP headers, so that the stealer traffic
 feature only captures HTTP requests that match the specified filter, forwarding unmatched
 requests to their original destinations.

 Only does something when [`feature.network.incoming.mode`](#feature-network-incoming-mode) is
 set as `"steal"`, ignored otherwise.

 ```json
 {
   "filter": "host: api\..+",
   "ports": [80, 8080]
 }
 ```

 ##### feature.network.incoming.http_header_filter.filter {#feature-network-incoming-http_header_filter-filter}

 Used to match against the requests captured by the mirrord-agent pod.

 Supports regexes validated by the
 [`fancy-regex`](https://docs.rs/fancy-regex/latest/fancy_regex/) crate.

 The HTTP traffic feature converts the HTTP headers to `HeaderKey: HeaderValue`,
 case-insensitive.

 ##### feature.network.incoming.http_header_filter.ports {#feature-network-incoming-http_header_filter-ports}

 Activate the HTTP traffic filter only for these ports.

 Other ports will still be stolen (when `"steal`" is being used), they're just not checked
 for HTTP filtering.

 ### feature.network.outgoing {#feature-network-outgoing}
 Tunnel outgoing network operations through mirrord.

 See the outgoing [reference](https://mirrord.dev/docs/reference/traffic/#outgoing) for more
 details.

 ```json
 {
   "feature": {
     "network": {
       "outgoing": {
         "tcp": true,
         "udp": true,
         "ignore_localhost": false,
         "unix_streams": "bear.+"
       }
     }
   }
 }
 ```

 #### feature.network.outgoing.tcp {#feature.network.outgoing.tcp}

 Defaults to `true`.

 #### feature.network.outgoing.udp {#feature.network.outgoing.udp}

 Defaults to `true`.

 #### feature.network.outgoing.ignore_localhost {#feature.network.outgoing.ignore_localhost}

 Defaults to `false`.

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

