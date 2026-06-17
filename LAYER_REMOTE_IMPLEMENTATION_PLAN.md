# `mirrord-layer-remote` file-by-file implementation plan

This document describes the concrete implementation steps for turning `mirrord-layer-remote` into a real incoming-capable layer-like component, and the extra agent-side bookkeeping needed when the remote side does not talk to intproxy directly.

## Target topology

```text
local app
  -> local intproxy
  -> sessions manager
  -> remote agent
  -> remote layer
```

The goal is for the remote layer to behave like a normal `mirrord` layer from the application’s point of view, while the agent-side remote logic provides the equivalent of the current incoming subscription bookkeeping.

---

## Phase 1: keep the remote layer entrypoint small

### 1. `mirrord/layer/remote/src/lib.rs`

Keep this file as the startup bridge only.

### Responsibilities

- declare Unix-only compilation
- run `layer_pre_initialization()`
- initialize tracing and layer setup
- install the remote hook set
- catch startup panics and exit cleanly

### Keep

- `#[ctor] fn mirrord_layer_entry_point()`
- `remote_start(config: LayerConfig)`
- `DetourGuard`
- `init_layer_setup(...)`
- `enable_socket_hooks(...)`

### Add

If the remote layer is expected to support real incoming accept flow, `remote_start` must install the full incoming socket hook set, not only `bind`/`listen`.

---

## Phase 2: split the socket module into a small root and a real ops module

### 2. `mirrord/layer/remote/src/socket.rs`

Keep this as a module root only.

### Contents

```rust
pub mod hooks;
pub mod ops;
```

This mirrors the structure of the full layer and keeps the entrypoint uncluttered.

---

## Phase 3: implement the remote detour entrypoints

### 3. `mirrord/layer/remote/src/socket/hooks.rs`

This file should only contain detours and hook registration.

### Copy from `mirrord/layer/src/socket/hooks.rs`

Copy these functions:

- `socket_detour`
- `close_detour`
- `bind_detour`
- `listen_detour`
- `getsockname_detour`
- `accept_detour`
- `accept4_detour`
- `accept_nocancel_detour`
- `uv__accept4_detour`
- `enable_socket_hooks`

### Keep the remote hook set focused on incoming support

The remote layer should register:

- `socket`
- `bind`
- `listen`
- `close`
- `getsockname`
- `getpeername`
- `accept`
- `accept4`
- `accept$NOCANCEL`
- `uv__accept4` on Linux

### Do not copy

Do not bring in outgoing or DNS hooks:

- `connect`
- `connect$NOCANCEL`
- `getaddrinfo`
- `freeaddrinfo`
- `gethostbyname`
- `fcntl`
- `dup`
- `send_to`
- `recv_from`
- `recvmsg`
- `sendmsg`

Those are not part of the incoming mirror path.

---

## Phase 4: implement the remote socket ops

### 4. `mirrord/layer/remote/src/socket/ops.rs`

This file should contain the actual incoming socket behavior.

### Copy from `mirrord/layer/src/socket/ops.rs`

Copy these functions and any tiny helpers they require:

- `bind_similar_address`
- `is_ignored_tcp_port`
- `bind`
- `listen`
- `getsockname`
- `getpeername`
- `accept`
- `close_layer_fd`

### Likely helpers to keep

Depending on how you structure the module split, you may also need:

- `fill_address`
- `is_ignored_port`
- any address-mapping helper used by `getpeername`
- any small conversion helpers used by `accept`

### What `bind()` must do

- validate the socket domain
- preserve normal conflict behavior
- apply the incoming `listen_ports` mapping if configured
- fall back to a random port when allowed
- record both requested and actual bound addresses in `SOCKETS`

### What `listen()` must do

- reject if incoming is disabled
- reject or bypass in targetless mode as appropriate
- call the real `listen`
- send the upstream port-subscribe request
- transition the socket to `Listening`

### What `getsockname()` must do

- return the synthetic local address for bound/listening sockets
- preserve the actual assigned port when the user bound port `0`

### What `getpeername()` must do

- return the synthetic peer address for accepted sockets
- bypass or error for non-connected sockets

### What `accept()` must do

This is the key step for fully mirrored incoming connections.

1. verify the accepted fd belongs to a socket we manage
2. ensure the listener is in `Listening(Bound { ... })`
3. create a `TcpStream` from the accepted fd
4. read the actual peer address from the stream
5. send:

```rust
make_proxy_request_with_response(ConnMetadataRequest {
    listener_address,
    peer_address,
})
```

6. receive `ConnMetadataResponse`
7. create a `Connected` socket state for the accepted fd
8. fill the returned sockaddr with the synthetic remote source
9. insert the new socket into `SOCKETS`

That is what makes the remote layer present a believable accepted connection to the application.

---

## Phase 5: keep the remote layer bootstrap aligned with the incoming socket surface

### 5. `mirrord/layer/remote/src/lib.rs`

The startup path should:

- call `init_tracing()`
- call `init_layer_setup(config, false)`
- create a `HookManager`
- install the full remote incoming socket hook set
- create `DetourGuard`
- set up proxy/session state if needed for the remote layer startup path

The remote layer should remain focused on incoming support and not pull in the full-layer feature surface.

---

## Phase 6: use the existing protocol for incoming connection metadata

The current protocol already contains the messages needed for the accept path.

### Keep using these types in `mirrord/intproxy/protocol/src/lib.rs`

- `IncomingRequest::PortSubscribe`
- `IncomingRequest::PortUnsubscribe`
- `IncomingRequest::ConnMetadata`
- `IncomingResponse::PortSubscribe`
- `IncomingResponse::ConnMetadata`
- `ConnMetadataRequest`
- `ConnMetadataResponse`

### Why

This avoids introducing a new protocol when the required request/response shape already exists.

---

## Phase 7: add remote-agent subscription bookkeeping

If the remote side does not talk to intproxy directly, the remote agent needs the equivalent of the current intproxy subscription manager.

### 6. `mirrord/agent/src/incoming/subscriptions.rs` 

Add a new module, or a similarly named module under `mirrord/agent/src/incoming`, that owns the subscription ownership and teardown logic.

This is the agent-side equivalent of:

- `mirrord/intproxy/src/proxies/incoming/subscriptions.rs`
- `mirrord/intproxy/src/proxies/incoming/metadata_store.rs`

### Responsibilities

- track active subscriptions by session/layer identity and port
- remember which listener address a subscription belongs to
- support both mirror and steal mode if both are needed
- dedupe repeated subscriptions
- keep the active subscription source for each port
- remove or replace subscriptions on unsubscribe and close
- clone subscription ownership on fork, if required
- maintain any metadata needed for `ConnMetadataRequest`

### Suggested API shape

Mirror the intproxy-side manager shape as closely as possible:

- `layer_subscribed(...)`
- `layer_unsubscribed(...)`
- `layer_closed(...)`
- `layer_forked(...)`
- `get(port)` or similar lookup helpers
- metadata expectation / lookup helpers for accept-time conn metadata

### Data it should track

At minimum:

- remote layer/session id
- port
- listening address
- subscription mode / filter
- confirmation state
- pending sources for duplicate subscriptions
- pending connection metadata lookups for accepted connections

---

## Phase 8: wire the remote agent to the existing redirector machinery

### 7. `mirrord/agent/src/incoming.rs`

This module already owns the redirector components.

### Keep using

- `MirrorHandle`
- `StealHandle`
- `RedirectorTask`
- the existing incoming connection transport in `mirrord/agent/src/incoming/connection/*`

### Add

Hook the new subscription manager into the incoming event path so it can:

- start redirection on subscribe
- stop redirection on unsubscribe
- remove all state on layer close
- clone state on fork
- answer accept-time metadata requests

### Why

The redirector implementation already knows how to forward traffic. The missing piece is the ownership and lifecycle tracking that determines when to start and stop those redirectors.

---

## Phase 9: agent-side event handling

### On `PortSubscribe`

- normalize the port / mode
- start mirroring or stealing through `MirrorHandle` or `StealHandle`
- register the subscription in the new manager
- acknowledge the subscription

### On `PortUnsubscribe`

- remove the layer’s source from the subscription table
- stop redirection if this was the last source for that port
- clean up any related metadata

### On `LayerClosed`

- drop every subscription owned by that layer/session
- stop any remaining active redirectors owned by that layer
- clear pending metadata expectations

### On `LayerForked`

- clone the active subscription ownership from parent to child
- keep routing consistent across forks

### On inbound accepted connection

- assign or look up the connection metadata entry
- keep enough state to answer `ConnMetadataRequest`
- ensure the remote layer gets the synthetic source/local address pair it expects

---

## Phase 10: expected file ownership after implementation

### `mirrord-layer-remote`

- `src/lib.rs`
- `src/socket.rs`
- `src/socket/hooks.rs`
- `src/socket/ops.rs`

### `mirrord-agent`

- `src/incoming.rs`
- `src/incoming/subscriptions.rs` or equivalent new module
- `src/incoming/metadata_store.rs` or equivalent new module
- existing redirector modules under `src/incoming/*`

### `mirrord/intproxy`

- keep the existing incoming protocol and routing path intact unless the remote architecture forces a protocol change

---

## Minimal implementation order

If you want the safest incremental path, implement in this order:

1. remote layer `accept` / `getpeername`
2. remote layer socket ops split into `ops.rs`
3. agent-side subscription manager
4. agent-side connection metadata bookkeeping
5. fork / close / unsubscribe handling
6. only then consider whether protocol changes are needed

---

## Summary

The remote layer becomes fully incoming-capable when it can:

- bind and listen like the full layer
- subscribe the port upstream
- accept connections and ask for connection metadata
- materialize accepted sockets with believable peer/local addresses
- clean up subscriptions on close

The remote agent needs the equivalent of `SubscriptionsManager` to own subscription lifecycle and metadata bookkeeping, while reusing the existing redirector machinery for the actual traffic forwarding.
