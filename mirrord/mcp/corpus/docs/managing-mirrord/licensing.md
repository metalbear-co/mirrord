---
title: Licensing
date: 2026-06-08T00:00:00.000Z
lastmod: 2026-06-08T00:00:00.000Z
draft: false
images: []
linktitle: Licensing
menu: null
docs: null
teams: null
weight: 560
toc: true
description: How mirrord for Teams licensing works — seats, tracking modes, and key validation
tags:
  - team
  - enterprise
---

## License Types

mirrord for Teams is available in two license variants:

| | **Teams** | **Enterprise** |
|---|---|---|
| **Delivery** | License key (online) | Certificate file (offline-capable) |
| **Seat enforcement** | Strict — sessions blocked when limit is reached | Configurable — overages can be allowed |
| **Session type** | Ephemeral only | Ephemeral and persistent |
| **Telemetry** | Required | Optional |
| **License server** | ❌ | ✅ |

**Teams** licenses are subscription-based and validated against MetalBear's servers using a license key. The operator must be able to reach MetalBear's license endpoint.

**Enterprise** licenses use a certificate file (`license.pem`) and can operate without an internet connection. They also support a [self-hosted License Server](license-server.md) for air-gapped or multi-cluster environments.

## Seats

A seat represents one active user. The number of available seats is set by your Teams subscription and enforced by the Operator.

### What counts as a seat?

By default, seats are tracked per **client machine** — specifically, by the public key of the TLS client certificate that the mirrord CLI presents to the Operator. Each distinct developer machine consumes one seat.

Optionally, seat tracking can be switched to **Kubernetes user** — the username from the kubeconfig used to authenticate to the cluster. In this mode, a single developer using multiple machines still counts as one seat.

The active tracking mode is configured by MetalBear when the license is issued and is visible in `mirrord operator status`.

### What happens when all seats are taken?

When a new user attempts to start a session and no seats are available, the Operator rejects the request with a `MaxSeatCountReached` error. Existing active sessions are not affected.

Seats are not released when a session ends. Once a seat is assigned to a user, that user retains it until the billing period resets. Seats are reset on the billing date each month, at which point the full seat count becomes available again.

### Checking seat usage

```bash
mirrord operator status
```

This command shows active sessions and seat information for your cluster.

## License Key Validation

Teams licenses require the Operator to validate the license key against MetalBear's servers on startup and periodically during operation. The Operator must have outbound internet access to reach the license endpoint.

File-based licenses (`.pem` certificates) are not supported for Teams subscriptions — those are only valid for Enterprise licenses.

## Telemetry

Teams licenses require telemetry to be sent back to MetalBear. Telemetry is used for seat tracking and subscription management.

### What is sent

Every telemetry batch includes the following HTTP headers:

- **Operator version** — the version of the Operator binary
- **Instance ID** — a random identifier generated at Operator startup (not tied to the cluster or any persistent identity)
- **License identifiers** — a hash of the license key, plus customer, subscription, and organization IDs from the license

The following event types are sent:

- **Startup** — emitted once when the Operator starts
- **Alive** — a periodic heartbeat sent every hour (escalates to every minute if the telemetry endpoint has been unreachable for over an hour)
- **Session** — emitted when a session ends, containing: session duration, target workload kind, a `ci` flag, and anonymized identifiers for the user and target (see below)

### Anonymization

Session events contain user-identifying fields (Kubernetes username, client username, client hostname, target namespace, target name, target container). These are **stripped before the event is forwarded to MetalBear's analytics pipeline**. They are retained only if you use an [Enterprise License Server](license-server.md) that receives telemetry directly.

### What is not collected

The Operator does **not** collect Kubernetes version, node count, cloud provider, or any other cluster infrastructure metadata.

### Connectivity requirement

If the Operator cannot reach MetalBear's telemetry endpoint for more than one hour, new sessions will be blocked until connectivity is restored. Ongoing sessions are not terminated.

If you operate in a network-restricted environment and cannot allow outbound telemetry, an [Enterprise license](license-server.md) is required.

## Sessions

Teams licenses support only **ephemeral sessions** — a session is tied to an active mirrord process and is automatically cleaned up when that process exits. The Operator does not retain any persistent session state between runs.

Enterprise licenses additionally support persistent sessions, which survive operator restarts and are used by features like [preview environments](../use-cases/preview-environments.md).

## Getting a License

Register at [app.metalbear.com](https://app.metalbear.com) to get a Teams license key. For Enterprise licensing, [contact us](mailto:hi@metalbear.com).

An AI coding agent working on an unlicensed cluster can also start a trial itself and hand you a link to claim the organization. See [Agent-Started Trials](../using-mirrord-with-ai/agent-started-trials.md).

Once you have a key, see the [Operator installation guide](operator.md) for setup instructions.
