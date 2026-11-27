---
title: Queue and Workflow Tutorials
description: Tutorials that test queue consumers and workflow workers in Kubernetes against a shared staging queue, with mirrord queue splitting and no code changes.
tags:
  - team
  - enterprise
  - alpha
---

# Queue and Workflow Tutorials

These tutorials show how to test a changed consumer against the real queue in a shared staging cluster. Each tutorial deploys a small sample app, starts a session with `mirrord up` and a session key, and shows which messages go to your local process and which go to the deployed consumer.

## The problem that queue splitting solves

When you run a second consumer on a shared queue, the two consumers compete for the messages. Your local consumer gets some of the messages, and your colleagues lose them. Another option is a separate queue for each developer. Then each developer also needs a separate copy of each service that sends messages to the queue.

mirrord queue splitting solves this in the operator. You start `mirrord up` with a session key, for example `mirrord up --key alice`. `mirrord up` makes a filter that matches messages that contain `mirrord-session=alice`, for example in a `baggage` header. The operator sends the messages that match the filter to your local consumer, and it sends all other messages to the deployed consumer. Many developers and coding agents can use the same queue at the same time.

Your consumer code does not change. You do not add an SDK, an interceptor, or a routing check to the consumer. The operator makes all the changes, and it removes them when the session ends.

## Tutorials

| System | Tutorial | Where the filter of `mirrord up` looks for `mirrord-session=<key>` |
|---|---|---|
| Temporal | [How to Test Temporal Workers](how-to-test-temporal-workers.md) | Temporal headers. The tutorial uses a workflow ID filter in `config_patch` instead. |
| RabbitMQ | [How to Test RabbitMQ Consumers](how-to-test-rabbitmq-consumers.md) | Message headers |
| Google Cloud Pub/Sub | [How to Test GCP Pub/Sub Consumers](how-to-test-gcp-pubsub-consumers.md) | Message attributes |
| Redis Pub/Sub | [How to Test Redis Pub/Sub Consumers](how-to-test-redis-pubsub-consumers.md) | The JSON payload |
| NATS | [How to Test NATS Consumers](how-to-test-nats-consumers.md) | JetStream message headers. Core NATS needs a filter in `config_patch`. |
| Azure Service Bus | [How to Test Azure Service Bus Consumers](how-to-test-azure-service-bus-consumers.md) | Application properties |
| BullMQ | [How to Test BullMQ Workers](how-to-test-bullmq-workers.md) | The job data |
| Apache Kafka | [How to Debug Kafka Consumers](https://metalbear.com/mirrord/docs/guides/queue-guides/how-to-debug-kafka-consumers) | Message headers |
| Amazon SQS | [How to Debug SQS Consumers](https://metalbear.com/mirrord/docs/guides/queue-guides/how-to-debug-sqs-consumers) | Message attributes |

To use a different filter, for example a regex on one header or a jq program on the body, set `feature.split_queues` in the `config_patch` of the service. The tutorials on this page show examples.

## Before you start

All tutorials need these items:

- A Kubernetes cluster for development or staging, version `1.30` or later. Do not use a production cluster.
- `kubectl`, configured for that cluster.
- The mirrord CLI. Each tutorial gives the version that it needs.
- The mirrord operator, with queue splitting turned on for your queue system. Queue splitting is a mirrord for Teams feature. Each tutorial gives the Helm value that it needs.

## See also

- [Queue splitting overview](../../sharing-the-cluster/queue-splitting.md)
- [Queue splitting status](../../sharing-the-cluster/queue-splitting/status.md)
- [Sessions](../../sharing-the-cluster/sessions.md)
