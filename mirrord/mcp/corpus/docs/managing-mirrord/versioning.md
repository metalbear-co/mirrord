---
title: Versioning
date: 2026-01-14T00:00:00.000Z
lastmod: 2026-01-14T00:00:00.000Z
draft: false
images: []
linktitle: Versioning
menu:
  docs:
    teams: null
weight: 545
toc: true
description: Versioning and compatibility between mirrord components
---

mirrord is composed of multiple components, including the CLI, Operator, Agent, and License Server. These components may be deployed and upgraded independently, and mirrord is designed to support safe, incremental upgrades.

This page describes how versioning and compatibility work across mirrord components.

## Component Compatibility

mirrord components communicate over well-defined APIs and do not require lockstep upgrades. In most cases, components can be upgraded independently as long as they remain within the same major version. 

Note: The only caveat here is that the mirrord Operator helm chart runs its own major versions, but the helm chart just delivers the Operator. The Operator version is what should be considered for compatibility.

## License Server and Operator Compatibility

The mirrord License Server and mirrord Operator are backward compatible with each other within the same major version.

This means that a newer License Server can serve older Operators, and a newer Operator can communicate with older License Servers. Minor and patch upgrades do not require coordinated rollouts between these components.

Breaking changes between the License Server and Operator only occur on major version upgrades.

## Operator and Agent Compatibility

Agents are deployed and managed by the mirrord Operator.

The Operator selects and manages the Agent version automatically. Users should not manually manage Agent image versions when using the Operator.

Operator and Agent compatibility is guaranteed within the same Operator release.

## Upgrade Order

When upgrading mirrord components, the following order is recommended:

* Upgrade the License Server
* Verify that the License Server is healthy and reachable
* Upgrade the mirrord Operator
* Upgrade client tooling (CLI) as needed

## Semantic Versioning

mirrord follows semantic versioning for all components.

Patch releases include bug fixes only. Minor releases are backward compatible across components. Major releases may introduce breaking changes.

Cross-component compatibility is guaranteed within the same major version. Any breaking changes will be documented in release notes and upgrade documentation.
