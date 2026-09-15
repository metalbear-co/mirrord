# `mirrord-operator-websocket`

## Overview

`mirrord-operator-websocket` owns the WebSocket transport shared by operator API consumers:

- `connection`: adapts an upgraded WebSocket to `mirrord-protocol` streams and sinks.
- `upgrade`: establishes WebSocket connections through the Kubernetes API server or a direct HTTPS endpoint.

Keep this crate limited to transport concerns. It is used by the agent, so it must not depend on operator CRDs, credentials, configuration, analytics, progress reporting, or other high-level mirrord client packages.

## Command Reference

```bash
cargo clippy -p mirrord-operator-websocket --all-targets --keep-going -- --deny warnings
```

## Compatibility

The WebSocket handshake and binary message framing are shared wire behavior. Preserve the Kubernetes API-server upgrade contract and the mirrord protocol encoding when making changes.
