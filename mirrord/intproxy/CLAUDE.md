## Quick Reference

```bash
cargo check -p mirrord-intproxy --keep-going
cargo test -p mirrord-intproxy
cargo check -p mirrord-intproxy-protocol --keep-going
```

## Architecture

### What the Intproxy Does

The intproxy sits between layer processes and the agent. One `IntProxy` instance serves multiple layer processes (`LayerConnection` tasks) and maintains a single agent connection (`AgentConnection` task). Feature-specific logic lives in separate background tasks: `FilesProxy`, `IncomingProxy`, `OutgoingProxy`, `SimpleProxy` (DNS + env), and `PingPong`.

### Operator vs OSS

The CLI decides how to connect before starting intproxy. In operator mode, intproxy joins an existing operator-managed session via `OperatorApi::connect_in_existing_session`. In OSS mode (`operator = false` or no operator installed), it talks to the agent directly through a Kubernetes port-forward.

Reconnect behavior differs: operator sessions may be reconnectable (`allow_reconnect`), while direct Kubernetes and external proxy modes are not (`ReconnectFlow::Break`).

`mirrord exec` always spawns intproxy regardless of mode. The operator changes _how_ intproxy reaches the agent, not whether intproxy exists.

### Startup and Version Negotiation

On startup, intproxy immediately sends `ClientMessage::SwitchProtocolVersion`. Layer messages are held back until `DaemonMessage::SwitchProtocolVersionResponse` comes in, so the proxy doesn't handle layer requests before it knows what protocol features are available.

### How Messages Flow

1. Layer sends `LocalMessage<LayerToProxyMessage>` over the local TCP channel.
2. `LayerConnection` converts that into `ProxyMessage::FromLayer`.
3. `IntProxy` routes it to the right feature proxy.
4. The feature proxy may send a `ClientMessage` to the agent via `MessageBus::send_agent`.
5. `AgentConnection` receives a `DaemonMessage` and emits `ProxyMessage::FromAgent`.
6. `IntProxy` routes the response back to the correct feature proxy or directly to the layer.
7. Responses to the layer are sent as `ProxyMessage::ToLayer`.

### How Requests Get Matched to Responses

Layer messages carry a `MessageId`; agent messages usually don't. Feature proxies use FIFO `RequestQueue` instances keyed by request type to recover the `(MessageId, LayerId)` for responses. This depends on the agent processing requests in order for each type of operation. New response paths must preserve this ordering or add explicit correlation IDs (like the outgoing V2 connect `Uid`).

### Reconnect Flow

**`ConnectionRefresh::Start`:** Broadcast to all feature proxies and ping-pong. Feature proxies flush pending requests with "agent lost" error responses and clear temporary state.
**`ConnectionRefresh::End(new_tx)`:** Broadcast new tx handle, replace `IntProxy.agent_tx`, re-negotiate protocol version, replay queued messages in order.

Any critical `ProxyRuntimeError` switches to `FailoverStrategy`. Pending requests get failure responses, new connections are still accepted, most messages are ignored.

### Layer-Intproxy Protocol

The `mirrord-intproxy-protocol` crate is intentionally **not** backward compatible across releases, because layer and intproxy are always shipped together.

## Rules to Follow

- Keep `IntProxy::handle_agent_message` and `IntProxy::handle_layer_message` handling every variant.
- Keep `pending_layers` accounting correct: add when a request expects a response, remove on `ToLayer`.
- Preserve request queue ordering assumptions when changing agent-side behavior.
- Any reconnect path must flush pending responses to avoid hanging hooked syscalls in the layer.
- Any new layer fork behavior should be mirrored across `FilesProxy`, `IncomingProxy`, and `OutgoingProxy`.
