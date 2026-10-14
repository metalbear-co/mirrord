---
title: Topology
description: See which services your mirrord sessions call, drawn as a map of the cluster
tags:
  - alpha
  - team
  - enterprise
---

# Topology

The **Topology** tab in the dashboard shows the services in your cluster and the connections between them. The map is built from mirrord sessions: when a session opens a connection to a Kubernetes Service, or receives one, the operator records it. You don't declare dependencies anywhere; the map shows what sessions actually connected to, so it fills in as your team uses mirrord.

{% hint style="info" %}
The map only includes connections that went through a mirrord session. To fill it in for a whole namespace at once, see [Map the whole cluster at once](#map-the-whole-cluster-at-once). If a connection you expect is missing, see [Why a connection is missing](#why-a-connection-is-missing).
{% endhint %}

The map below is from [MetalMart](https://github.com/metalbear-co/playground/tree/main/apps/shop), our open-source demo shop: its services run in the `shop` namespace, and the databases and message brokers they use run in `infra`.

![Topology tab for the MetalMart demo app, showing its services, data stores, message queues and preview environments](../../.gitbook/assets/topology-map.png)

## Requirements

- Operator chart 3.211.0 or newer.
- A dashboard, set up with either [Cloud Setup](cloud.md) or [License Server Setup](license-server.md). With a license server, the license server also needs to be 3.211.0 or newer.
- For [Cloud Setup](cloud.md): an API key created with identity sharing on (see [Cloud Setup](cloud.md#new-customers-set-up-the-cloud-dashboard)), and `cloud.anonymizeData` not set to `true`. Service names only leave the cluster with identity, so without it the tab stays empty.

## Turn it on

Topology is off by default.

1. Set it in the operator's Helm values:

    ```yaml
    # values.yaml
    operator:
      topology: true
    ```

2. Upgrade the operator:

    ```bash
    helm upgrade mirrord-operator metalbear/mirrord-operator -f values.yaml
    ```

3. Run a few mirrord sessions, then open the **Topology** tab. A session reports its connections when it ends, so each new connection shows up shortly after the session that made it stops.

{% hint style="info" %}
With topology on, the operator watches every Service and EndpointSlice in the cluster so it can tell which Service an address belongs to. The chart makes these RBAC changes for it:

- The operator's ClusterRole gets `get`, `list` and `watch` on `endpointslices`.
- The `mirrord-operator-user`, `mirrord-operator-user-basic` and `mirrord-operator-ci` ClusterRoles get `get` and `list` on `mirrordclusterservicegraphs`, the operator's read-only view of the map.
{% endhint %}

## Map the whole cluster at once

The map fills in as your team uses mirrord, but you don't have to wait for that. A coding agent like Claude Code or Codex can run a session against every workload for you. For example:

```
List the deployments in the shop namespace. For each one, run the service locally
with mirrord targeting that deployment, and exercise its main endpoints so the
calls it makes go through mirrord.
```

Once the sessions end, refresh the **Topology** tab. The connections those sessions made are on the map.

{% hint style="warning" %}
Exercising endpoints sends real requests from the agent's sessions into your cluster, for example creating orders against a shared staging database. Point the agent at an environment where that's acceptable.
{% endhint %}

## What gets recorded

A connection is recorded in three cases:

- **Outgoing**: the local process connects through mirrord to an address in the cluster, the connection succeeds, and the address belongs to a Service (its cluster IP, or a pod behind it). The connection runs from the session's target to that Service. For example, a session targeting `order-service` that calls `inventory-service` adds `order-service` → `inventory-service`.
- **Incoming**: a request reaches the session's target from a pod behind a Service. The connection runs from the caller to the target. mirrord only receives traffic on ports the local process listens on (the target's port, or a local port mapped to it), so a request on any other port isn't seen.
- **Preview environments**: connections a preview accepts from other Services. They are reported when the preview stops and drawn on the workload the preview stands in for. Preview pods don't run mirrord, so their own outgoing calls aren't recorded by the preview; a preview shows up as its own node, labelled with its key, when it is the other end of a session's connection, for example when it calls a workload a session targets.

Each connection keeps the number of sessions that produced it, a user count, and when it was last reported. The user count is per side: when the same connection is reported from both ends, the map shows the higher of the two counts rather than adding them, so it never counts someone twice but can show fewer users than there were. What's recorded is connection metadata: the Service's name and namespace, the direction, and the port for outgoing connections. Request and response contents are never recorded.

## Reading the map

Services are grouped by namespace. On a large cluster the map opens with every namespace folded; click one to open it, or use **Expand all**.

Nodes are colored by category:

| Category | How it's decided |
| --- | --- |
| **Entry point** | A node marked **Discovered** (see below) that only ever calls other services and is never called |
| **Service** | Anything that fits none of the other categories |
| **Data store** | Reached on a well-known database port (see [Well-known ports](#well-known-ports)) |
| **Queue** | Reached on a well-known message broker port (see [Well-known ports](#well-known-ports)) |
| **Infrastructure** | Reached on a well-known infrastructure port (see [Well-known ports](#well-known-ports)) |
| **Preview env** | A preview environment's Service, seen as the other end of a session's connection. Each one is its own node, labelled with the preview's key, and its replicas share that node. A preview environment covering three services shows as up to three nodes, one for each that appears in a connection |

A service marked **Discovered** wasn't targeted by any session. The map only knows it as the other end of a connection. The map matches a Service to a workload by name and namespace. If a Service's name differs from its workload's, for example the Service `web-svc` in front of the Deployment `web`, they show up as two separate items: the workload, and the Service marked **Discovered**. Its session and user counts come from the connections around it.

Categories come from ports and from which end of a connection a service was on. Service names are never used to guess them, so a Postgres served on a custom port shows up as a plain **Service**. Only the lowest port a workload reached on a Service is kept, so a database that is also reached on a lower port, such as 80, can show up as a plain **Service** too. Click a chip in the legend to hide that category.

Other controls:

- **Find a service** searches by name. Press `/` to jump to it.
- **Busiest paths** keeps the busiest quarter of the connections lit and dims the rest.
- Click a node to open its details: when it was last seen, and its incoming and outgoing connections with sessions and users for each. **Copy link** copies a URL that opens the map with that node selected.
- **List** shows the same connections as a table of caller, callee, sessions, users and last seen.
- **Export as PNG** saves the map as an image.
- Press `f` to fit the map to the window and `Esc` to clear the focus.

The time range selector applies here too. It filters by session, not by individual connection: the cloud dashboard uses the time the session started, and the license server uses the time the session was reported. The report holds at most 500 connection records for the range, busiest first. A connection reported from both ends is two records, which the map merges into one.

## Why a connection is missing

The map only knows about traffic that went through a mirrord session. If two services talk to each other but no session was part of that conversation, there's no connection on the map. For example, `order-service` calling `payment-service` shows up only when:

- a session targeting `order-service` made that call from the local process, or
- a session targeting `payment-service` received the call from `order-service`, on a port the local process was listening on (or had a local port mapped to).

Some other cases that leave gaps:

- The address doesn't belong to a Kubernetes Service: an external host, or a pod no Service selects.
- An outgoing connection failed. Only outgoing connections that succeed are recorded.
- The address belongs to more than one Service with different pods behind them, so it can't be attributed to one.
- The connection used UDP. Only TCP is recorded.
- The session targeted pods by label selector instead of a workload.
- The service called itself. Connections to a Service with the same name and namespace as the session's target aren't recorded.
- The connection happened in the first moments after the operator started, before it finished loading the cluster's Services.
- The session connected to a very large number of Services. Each session reports a limited list, keeping the most recent ones.
- The session is still running. Connections are reported when the session ends.
- On the cloud dashboard, the session ran with identity sharing off or `cloud.anonymizeData: true`.

To see a service's connections, run a session against the workload and exercise the connections you want to inspect.

## Well-known ports

The ports that put a service in the **Data store**, **Queue** or **Infrastructure** category:

| Category | Products (ports) |
| --- | --- |
| **Data store** | Postgres (5432), PgBouncer (6432), MySQL (3306, 33060), MongoDB (27017-27019), Redis (6379), Redis Sentinel (26379), Memcached (11211), Cassandra (9042), Elasticsearch (9200, 9300), ClickHouse (8123, 9440), CockroachDB (26257), SQL Server (1433), Oracle (1521), CouchDB (5984), ArangoDB (8529), Neo4j (7687), InfluxDB (8086), Qdrant (6333), Milvus (19530) |
| **Queue** | Kafka (9092), RabbitMQ (5671, 5672, 15672), NATS (4222), Temporal (7233), ActiveMQ (61616), MQTT (1883, 8883), NSQ (4150), Pulsar (6650) |
| **Infrastructure** | Vault (8200), Consul (8500), Prometheus (9090), Jaeger (14250, 14268, 16686), OpenTelemetry (4317, 4318), Zipkin (9411), StatsD (8125), Datadog APM (8126), etcd (2379), DNS (53) |
