# CLAUDE.md

Context for Claude Code when working with the mirrord repository.

## Quick Reference

```bash
# Build
cargo build

# Check packages (agent is Linux-only)
cargo check -p mirrord-layer --keep-going
cargo check -p mirrord-protocol --keep-going
cargo check -p mirrord-agent --target x86_64-unknown-linux-gnu --keep-going

# Integration tests
cargo test -p mirrord-layer
```

**Key paths:**
- Protocol messages: `mirrord/protocol/src/codec.rs`
- Agent main loop: `mirrord/agent/src/entrypoint.rs`
- Layer hooks: `mirrord/layer/src/file/hooks.rs`, `mirrord/layer/src/socket/hooks.rs`
- Intproxy routing: `mirrord/intproxy/src/lib.rs`
- Configuration: `mirrord/config/src/lib.rs`

## Architecture

mirrord lets developers run local processes in the context of a Kubernetes cluster. It intercepts syscalls locally and executes them remotely in a target pod's environment.

### Component Overview

```
┌─────────────────────────────────────────────────────────────────────────┐
│  LOCAL MACHINE                                                          │
│  ┌──────────────────────┐     ┌──────────────────────┐                  │
│  │   User Application   │     │      CLI (mirrord)   │                  │
│  │  ┌────────────────┐  │     │  - Starts intproxy   │                  │
│  │  │     Layer      │  │     │  - Resolves target   │                  │
│  │  │ (LD_PRELOAD)   │◄─┼─────┤  - Sets up env vars  │                  │
│  │  └───────┬────────┘  │     └──────────────────────┘                  │
│  └──────────┼───────────┘                                               │
│             │ Unix socket / TCP                                         │
│  ┌──────────▼───────────┐                                               │
│  │       Intproxy       │  Routes messages, manages connections         │
│  │  (or Operator proxy) │  Background tasks: files, incoming, outgoing  │
│  └──────────┬───────────┘                                               │
└─────────────┼───────────────────────────────────────────────────────────┘
              │ TCP (k8s port-forward or operator connection)
┌─────────────┼───────────────────────────────────────────────────────────┐
│  KUBERNETES │ CLUSTER                                                   │
│  ┌──────────▼───────────┐                                               │
│  │        Agent         │  Ephemeral pod, runs in target's namespace    │
│  │  - File operations   │  Has access to target's fs, network, env      │
│  │  - DNS resolution    │  Uses iptables for traffic stealing           │
│  │  - Traffic steal     │                                               │
│  │  - Outgoing conns    │                                               │
│  └──────────────────────┘                                               │
└─────────────────────────────────────────────────────────────────────────┘
```

### The Three Tiers

**Layer** (`mirrord-layer`) - Injected into the user's process via `LD_PRELOAD` (Linux) or `DYLD_INSERT_LIBRARIES` (macOS). Uses Frida GUM to hook libc functions. When a hooked function is called, the layer either:
- Handles it locally (bypass)
- Sends a `ClientMessage` to the proxy and waits for a `DaemonMessage` response

**Proxy** - Routes messages between layer and agent. Two variants:
- **Intproxy** (`mirrord-intproxy`): Runs locally in open-source mode
- **Operator**: Runs in-cluster for paid version (separate repo at `../operator/`)

**Agent** (`mirrord-agent`) - Ephemeral pod that performs operations in the target's context. Network tasks run in the **target pod's network namespace** for correct DNS resolution and routing.

### Why Namespace Matters

The agent spawns network tasks in the target's **network namespace** because:
- **DNS**: Queries go through target's network stack, using its DNS servers and search domains
- **Outgoing connections**: Use target's routing tables and network policies, appear to originate from the target
- **Incoming traffic (steal)**: iptables rules operate on the target's network interfaces

File access uses `/proc/<pid>/root/...` paths which provide the target's filesystem view without requiring mount namespace entry. Environment variables come from the container runtime API (Docker/containerd inspect) supplemented by `/proc/<pid>/environ`.

## Protocol

Messages flow Layer → Proxy → Agent and back. Defined in `mirrord/protocol/src/codec.rs`:

- `ClientMessage`: Layer-to-agent requests (file ops, DNS, connections, port subscriptions)
- `DaemonMessage`: Agent-to-layer responses

Uses bincode for serialization. Version negotiation happens at connection start via `SwitchProtocolVersion`/`SwitchProtocolVersionResponse`.

**Exhaustiveness requirement**: All components must handle all message variants. Rust's exhaustive matching enforces this. When adding a new variant, update:
1. Agent handler (`entrypoint.rs`)
2. Intproxy routing (`lib.rs`)
3. CLI tools (dump, port_forward, etc.)
4. Operator router (separate repo)

Use `cargo check -p <package> --keep-going` to see all missing patterns at once.

## Layer Details

### Hook Mechanism

`HookManager` in `hooks.rs` uses Frida GUM to intercept functions:
```rust
// Replaces libc export with detour function
hook_manager.hook_export_or_any("open", open_detour as *mut c_void)?;
```

The `DetourGuard` pattern prevents recursive hooking during detour execution.

### File Hooks (`file/hooks.rs`)

Hooked: `open`, `openat`, `read`, `write`, `close`, `stat`, `access`, `readdir`, `mkdir`, `unlink`, etc.

**Flow**:
1. `open_detour` intercepts call
2. Checks if path should be handled remotely (vs bypass for local paths)
3. Sends `FileRequest::Open` via proxy
4. Receives `FileResponse::Open` with remote fd
5. Maps local fd → remote fd in `OPEN_FILES` static

**Bypass cases**: Paths matching local patterns, mirrord temp dirs, certain system paths.

### Socket Hooks (`socket/hooks.rs`)

Hooked: `socket`, `bind`, `listen`, `accept`, `connect`, `recv`, `send`, `getaddrinfo`, etc.

**Incoming traffic** (steal/mirror): Layer calls `bind`/`listen` → sends port subscription to agent → agent uses iptables to redirect traffic → layer receives via `accept`.

**Outgoing traffic**: Layer calls `connect` → sends to agent → agent makes real connection in target namespace → data proxied back.

**DNS**: `getaddrinfo` intercepted → `GetAddrInfoRequest` sent → agent resolves using target's DNS config.

### Socket Tracking

`SOCKETS` hashmap tracks user sockets. Serialized to `MIRRORD_SHARED_SOCKETS` env var for child process inheritance across `exec*` calls.

## Agent Details

### Main Loop (`entrypoint.rs`)

`ClientConnectionHandler::start()` runs a `tokio::select!` loop handling:
- Incoming `ClientMessage`s from connection
- Responses from background task APIs (mirror, stealer, outgoing, DNS)
- Cancellation

### Background Tasks

- **TcpMirrorApi**: Sniffs TCP traffic without redirecting (mirror mode)
- **TcpStealerApi**: Redirects traffic via iptables (steal mode)
- **TcpOutgoingApi/UdpOutgoingApi**: Handle outgoing connections
- **DnsWorker**: DNS resolution using target's resolv.conf/hosts

### Traffic Stealing

Uses iptables REDIRECT rules to capture incoming traffic:
```
-A PREROUTING -p tcp --dport <target_port> -j REDIRECT --to-port <agent_port>
```

The agent has a parent/child process model: parent cleans up iptables on exit (even if child OOMs).

### File Operations (`file.rs`)

`FileManager` handles file requests, maintaining remote fd state. Operations performed on target filesystem via `/proc/<pid>/root/...` paths.

## Intproxy Details

### Architecture (`lib.rs`)

`IntProxy` manages background tasks via `BackgroundTasks<MainTaskId, ProxyMessage>`:

- `LayerInitializer`: Accepts new layer connections
- `LayerConnection` (per layer): Handles layer messages
- `AgentConnection`: Manages agent connection, reconnection
- `FilesProxy`: Routes file operations
- `IncomingProxy`: Routes incoming traffic messages
- `OutgoingProxy`: Routes outgoing connection messages
- `SimpleProxy`: Handles env vars, DNS, hostname

### Message Routing

Layer messages are dispatched to appropriate proxy based on type:
- File requests → FilesProxy
- TCP/UDP outgoing → OutgoingProxy
- Port subscriptions, stolen data → IncomingProxy
- Env vars, DNS → SimpleProxy

Each proxy maintains state for request/response matching using message IDs.

### Failover

Supports reconnection to agent with configurable retry logic. PingPong task monitors connection health.

## Configuration

`LayerConfig` (`config/src/lib.rs`) is the root config struct with:

- `target`: What to connect to (pod, deployment, statefulset, etc.)
- `feature.env`: Environment variable handling (include/exclude/override)
- `feature.fs`: Filesystem mode (read-only, read-write, local, disabled)
- `feature.network.incoming`: Traffic mode (mirror, steal, off), HTTP filtering, port mapping
- `feature.network.outgoing`: TCP/UDP interception, filters
- `feature.network.dns`: DNS interception toggle
- `agent`: Agent configuration (image, resources, etc.)

Config sources: JSON/YAML file → environment variables → CLI args (highest priority).

## Development Workflows

### Adding a New Protocol Message

1. Add variant to `ClientMessage` or `DaemonMessage` in `codec.rs`
2. Add request/response types in appropriate module (file.rs, dns.rs, etc.)
3. Handle in agent's `handle_client_message()`
4. Route in intproxy (or add to unexpected pattern)
5. Handle in CLI tools
6. Update Operator router (separate repo)

### Adding a New Syscall Hook

1. Create detour function in `layer/src/{file,socket}/hooks.rs`
2. Implement operation logic in corresponding `ops.rs`
3. Register hook in layer setup
4. May need new `FileRequest`/`ClientMessage` variant
5. Implement agent handler

### Cross-Repository Development

The Operator repo (`../operator/`) depends on mirrord crates. Workflow:
1. Make changes in mirrord
2. Commit and push to fork
3. Update operator's Cargo.toml with new commit:
   ```toml
   [workspace.dependencies.mirrord-protocol]
   git = "https://github.com/<fork>/mirrord.git"
   rev = "<new-commit>"
   ```

## Crate Overview

| Crate | Purpose |
|-------|---------|
| `mirrord-layer` | Syscall interception via LD_PRELOAD |
| `mirrord-agent` | Remote operations in target namespace |
| `mirrord-intproxy` | Local message routing proxy |
| `mirrord-protocol` | Message definitions (ClientMessage, DaemonMessage) |
| `mirrord-cli` | Command-line interface |
| `mirrord-config` | Configuration parsing/validation |
| `mirrord-kube` | Kubernetes API interactions |
| `mirrord-operator` | CRD definitions (used by operator repo) |

## Key Patterns

**Detour pattern**: Hook function checks `DetourGuard`, bypasses if guard not acquired (prevents recursion).

**Remote fd mapping**: Layer maintains `OPEN_FILES` mapping local fds to remote fds with Arc for dup support.

**Version requirements**: `LazyLock<VersionReq>` statics gate features by protocol version.

**Background task pattern**: Tasks communicate via mpsc channels, managed by `BackgroundTasks` wrapper with cancellation tokens.

**Namespace execution**: Agent spawns network tasks in dedicated runtime using `BgTaskRuntime` with namespace context.
