---
title: Feature Support Matrix
description: "See which platforms and infrastructure support mirrord's advanced features — DB Branching, Queue Splitting, Preview Environments, and Multi-cluster."
---

Some of mirrord's advanced features - DB Branching, Queue Splitting, Preview Environments, and Multi-cluster - work across a range of platforms and infrastructure setups.
Support levels vary by platform. The table below shows the current status for each combination.

### The columns reflect the different contexts in which a feature can be used:
- Preview Env: using the feature inside a [preview environment](use-cases/preview-environments.md). Each developer or agent gets an isolated slice of the dependency - for example, their own queue partition or database branch — within a shared staging cluster, without affecting others.
- Multi-cluster: using the feature across [multiple clusters](using-mirrord/multi-cluster.md). Traffic, queue messages, or database connections are routed across cluster boundaries, useful when your infrastructure spans more than one cluster.
- mirrord up: using the feature when running multiple services together via [mirrord up](using-mirrord/multiple-concurrent-sessions.md). Similar to how `docker compose` orchestrates containers, mirrord up lets you define and launch several interdependent microservices in a single `mirrord-up` file, managing their lifecycle as a group.

 | Feature         | Platform          | Preview Env | Multi-cluster | mirrord up |
  |-----------------|-------------------|-------------|---------------|------------|
  | **[DB Branching](../sharing-the-cluster/db-branching.md)**    | **[PostgreSQL](../sharing-the-cluster/db-branching/postgresql.md)**        | `Alpha`       | `Alpha`         | Planned    |
  |                 | **[MongoDB](../sharing-the-cluster/db-branching/mongodb.md)**           | `Alpha`       | `Alpha`         | Planned    |
  |                 | **[MySQL](../sharing-the-cluster/db-branching/mysql.md)**             | `Alpha`       | `Alpha`         | Planned    |
  |                 | **[MariaDB](../sharing-the-cluster/db-branching/mariadb.md)**           | `Alpha`       | `Alpha`         | Planned    |
  |                 | **[MSSQL](../sharing-the-cluster/db-branching/mssql.md)**             | `Alpha`       | `Alpha`         | Planned    |
  |                 | **[Redis](../sharing-the-cluster/db-branching/redis.md)** (both local and remote) | `Alpha`       | `Alpha`         | Planned    |
  |                 | **[DynamoDB](../sharing-the-cluster/db-branching/dynamodb.md)**          | `Alpha`       | `Alpha`         | Planned    |
  |                 | **[ClickHouse](../sharing-the-cluster/db-branching/clickhouse.md)**        | `Alpha`       | `Alpha`         | Planned    |
  |                 | **[CockroachDB](../sharing-the-cluster/db-branching/cockroachdb.md)**      | `Alpha`       | `Alpha`         | Planned    |
  |                 | **[Google Spanner](../sharing-the-cluster/db-branching/spanner.md)**      | `Alpha`       | `Alpha`         | Planned    |
  |                 | **[turbopuffer](../sharing-the-cluster/db-branching/turbopuffer.md)**     | `Alpha`       | `Alpha`         | Planned    |
  |                 | **[Generic](../sharing-the-cluster/db-branching/generic.md)** (user-supplied image) | `Alpha`       | `Alpha`         | Planned    |
  | **[Queue Splitting](../sharing-the-cluster/queue-splitting.md)** | **[SQS](../sharing-the-cluster/queue-splitting/sqs.md)**               | `Alpha`       | `Alpha`         | Planned    |
  |                 | **[Kafka](../sharing-the-cluster/queue-splitting/kafka.md)**             | `Alpha`       | `Alpha`         | Planned    |
  |                 | **[RabbitMQ](../sharing-the-cluster/queue-splitting/rabbitmq.md)**          | `Alpha`       | `Alpha`         | Planned    |
  |                 | **[Temporal](../sharing-the-cluster/queue-splitting/temporal.md)**          | `Alpha` | `Alpha`         | Planned |
  |                 | **[Google Pub/Sub](../sharing-the-cluster/queue-splitting/gcp-pubsub.md)**    | `Alpha`       | `Alpha`         | Planned    |
  |                 | **[Azure Service Bus](../sharing-the-cluster/queue-splitting/azure-service-bus.md)** | `Alpha`       | `Alpha`         | Planned    |
  |                 | **[NATS](../sharing-the-cluster/queue-splitting/nats.md)**              | `Alpha`     | `Alpha`       | Planned    |
  |                 | **[NATS Pub/Sub](../sharing-the-cluster/queue-splitting/nats.md#core-nats-pubsub-no-jetstream)** | `Alpha`     | `Alpha`       | Planned    |
  |                 | **[Redis Pub/Sub](../sharing-the-cluster/queue-splitting/redis-pubsub.md)**     | `Alpha`     | `Alpha`       | Planned |
  |                 | **[BullMQ](../sharing-the-cluster/queue-splitting/bullmq.md)**     | `Alpha`     | `Alpha`       | Planned |
  | **[Preview Env](../use-cases/preview-environments.md)**     | —                 | —           | `Alpha`         | Planned    |
  | **[Multi-cluster](../using-mirrord/multi-cluster.md)**   | —                 | `Alpha`       | —             | Planned    |
  | **[mirrord up](../using-mirrord/multiple-concurrent-sessions.md)**      | —                 | `Alpha`       | `Alpha`         | —          |


*Not sure what Alpha, Beta, or GA means? See [Release Status](release-stages.md)*

### Didn't find what you are looking for?
[Open a GitHub issue](https://github.com/metalbear-co/mirrord/issues) or reach out in the [mirrord Slack community](https://metalbearcommunity.slack.com/ssb/redirect) and tell us what you're working with, we prioritize based on demand.
