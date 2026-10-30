---
title: Feature Support Matrix
description: "See which platforms and infrastructure support mirrord's advanced features — DB Branching, Queue Splitting, Preview Environments, and Multi-cluster."
---

# Feature Support Matrix

Some of mirrord's advanced features - DB Branching, Queue Splitting, Preview Environments, and Multi-cluster - work across a range of platforms and infrastructure setups.
Support levels vary by platform. The table below shows the current status for each combination.

The columns reflect the different contexts in which a feature can be used:
- Preview Env: using the feature inside a [preview environment](use-cases/preview-environments.md). Each developer or agent gets an isolated slice of the dependency - for example, their own queue partition or database branch — within a shared staging cluster, without affecting others.
- Multi-cluster: using the feature across [multiple clusters](using-mirrord/multi-cluster.md). Traffic, queue messages, or database connections are routed across cluster boundaries, useful when your infrastructure spans more than one cluster.
- mirrord up: using the feature when running multiple services together via [mirrord up](using-mirrord/multiple-concurrent-sessions.md). Similar to how `docker compose` orchestrates containers, mirrord up lets you define and launch several interdependent microservices in a single `mirrord-up` file, managing their lifecycle as a group.

 | Feature         | Platform          | Preview Env | Multi-cluster | mirrord up |
  |-----------------|-------------------|-------------|---------------|------------|
  | **[DB Branching](../sharing-the-cluster/db-branching.md)**    | **[PostgreSQL](../sharing-the-cluster/db-branching/postgresql.md)**        | `Alpha`       | `Alpha`         | `Alpha`    |
  |                 | **[MongoDB](../sharing-the-cluster/db-branching/mongodb.md)**           | `Alpha`       | `Alpha`         | `Alpha`    |
  |                 | **[MySQL](../sharing-the-cluster/db-branching/mysql.md)**             | `Alpha`       | `Alpha`         | `Alpha`    |
  |                 | **[MariaDB](../sharing-the-cluster/db-branching/mariadb.md)**           | `Alpha`       | `Alpha`         | `Alpha`    |
  |                 | **[MSSQL](../sharing-the-cluster/db-branching/mssql.md)**             | `Alpha`       | `Alpha`         | `Alpha`    |
  |                 | **[Redis](../sharing-the-cluster/db-branching/redis.md)** (both local and remote) | `Alpha`       | `Alpha`         | `Alpha`    |
  |                 | **[DynamoDB](../sharing-the-cluster/db-branching/dynamodb.md)**          | `Alpha`       | `Alpha`         | `Alpha`    |
  |                 | **[ClickHouse](../sharing-the-cluster/db-branching/clickhouse.md)**        | `Alpha`       | `Alpha`         | `Alpha`    |
  |                 | **[CockroachDB](../sharing-the-cluster/db-branching/cockroachdb.md)**      | `Alpha`       | `Alpha`         | `Alpha`    |
  |                 | **[Google Spanner](../sharing-the-cluster/db-branching/spanner.md)**      | `Alpha`       | `Alpha`         | `Alpha`    |
  |                 | **[S3](../sharing-the-cluster/db-branching/s3.md)**                 | `Alpha`       | `Alpha`         | `Alpha`    |
  |                 | **[turbopuffer](../sharing-the-cluster/db-branching/turbopuffer.md)**     | `Alpha`       | `Alpha`         | `Alpha`    |
  |                 | **[Generic](../sharing-the-cluster/db-branching/generic.md)** (user-supplied image) | `Alpha`       | `Alpha`         | `Alpha`    |
  | **[Queue Splitting](../sharing-the-cluster/queue-splitting.md)** | **[SQS](../sharing-the-cluster/queue-splitting/sqs.md)**               | `Alpha`       | `Alpha`         | `Alpha`    |
  |                 | **[Kafka](../sharing-the-cluster/queue-splitting/kafka.md)**             | `Alpha`       | `Alpha`         | `Alpha`    |
  |                 | **[RabbitMQ](../sharing-the-cluster/queue-splitting/rabbitmq.md)**          | `Alpha`       | `Alpha`         | `Alpha`    |
  |                 | **[Temporal](../sharing-the-cluster/queue-splitting/temporal.md)**          | `Alpha` | `Alpha`         | `Alpha` |
  |                 | **[Google Pub/Sub](../sharing-the-cluster/queue-splitting/gcp-pubsub.md)**    | `Alpha`       | `Alpha`         | `Alpha`    |
  |                 | **[Azure Service Bus](../sharing-the-cluster/queue-splitting/azure-service-bus.md)** | `Alpha`       | `Alpha`         | `Alpha`    |
  |                 | **[NATS](../sharing-the-cluster/queue-splitting/nats.md)**              | `Alpha`     | `Alpha`       | `Alpha`    |
  |                 | **[NATS Pub/Sub](../sharing-the-cluster/queue-splitting/nats.md#core-nats-pubsub-no-jetstream)** | `Alpha`     | `Alpha`       | `Alpha`    |
  |                 | **[Redis Pub/Sub](../sharing-the-cluster/queue-splitting/redis-pubsub.md)**     | `Alpha`     | `Alpha`       | `Alpha` |
  |                 | **[BullMQ](../sharing-the-cluster/queue-splitting/bullmq.md)**     | `Alpha`     | `Alpha`       | `Alpha` |
  | **[Preview Env](../use-cases/preview-environments.md)**     | —                 | —           | `Alpha`         | `Alpha`    |
  | **[Multi-cluster](../using-mirrord/multi-cluster.md)**   | —                 | `Alpha`       | —             | Planned    |
  | **[mirrord up](../using-mirrord/multiple-concurrent-sessions.md)**      | —                 | `Alpha`       | `Alpha`         | —          |


*Not sure what Alpha, Beta, or GA means? See [Release Status](release-stages.md)*

## Didn't find what you are looking for?
[Open a GitHub issue](https://github.com/metalbear-co/mirrord/issues) or reach out in the [mirrord Slack community](https://metalbearcommunity.slack.com/ssb/redirect) and tell us what you're working with, we prioritize based on demand.
