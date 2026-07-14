## Overview

The intproxy sits between layer processes and the agent. One `IntProxy` instance serves multiple layer processes
(`LayerConnection` tasks) and maintains a single agent connection (`AgentConnection` task). Feature-specific logic lives
in separate background tasks: `FilesProxy`, `IncomingProxy`, `OutgoingProxy`, `SimpleProxy` (DNS + env), and `PingPong`.

### Operator vs OSS

The CLI decides how to connect before starting intproxy. In operator mode, intproxy joins an existing operator-managed
session via `OperatorApi::connect_in_existing_session`. In OSS mode (`operator = false` in the config or no operator
installed), it talks to the agent directly through a Kubernetes port-forward.

Reconnect behavior differs: operator sessions may be reconnectable (`allow_reconnect`), while direct Kubernetes and
external proxy modes are not (`ReconnectFlow::Break`).

### How Messages Flow

1. Layer sends `LocalMessage<LayerToProxyMessage>` over the local TCP channel.
2. `LayerConnection` converts that into `ProxyMessage::FromLayer`.
3. `IntProxy` routes it to the right feature proxy.
4. The feature proxy may send a `ClientMessage` to the agent via `MessageBus::send_agent`.
5. `AgentConnection` receives a `DaemonMessage` and emits `ProxyMessage::FromAgent`.
6. `IntProxy` routes the response back to the correct feature proxy or directly to the layer.
7. Responses to the layer are sent as `ProxyMessage::ToLayer`.

### Layer-Intproxy Protocol

The `mirrord-intproxy-protocol` crate is intentionally **not** backward compatible across releases, because layer and
intproxy are always shipped together.
