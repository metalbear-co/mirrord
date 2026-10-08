# mirrord-header-propagation

Make your services forward the W3C `baggage` header (carrying `mirrord-session=<key>`) across every hop: HTTP, gRPC, message queues, and object-storage uploads. mirrord HTTP filters, preview environments, and queue splitting can then route a whole request chain, not just the first service, to one developer's session.

## What it does

This skill helps AI agents:
- **Map** every flow in the codebase: HTTP/gRPC ingress and egress, producers and consumers for Kafka, RabbitMQ, SQS/SNS, GCP Pub/Sub, Azure Service Bus, NATS, Redis Pub/Sub, BullMQ and Temporal, and storage-triggered flows
- **Detect** OpenTelemetry, Datadog, Micrometer/Spring, or other tracing that can already propagate baggage, and find the hops it misses (propagator list, uninstrumented messaging clients, prefixed attribute names)
- **Enable** propagation in that existing stack, or **add** a minimal OpenTelemetry baggage-only layer where there isn't one
- **Restore** context per message in consumers, so downstream calls and publishes carry the session on
- **Carry** baggage through storage hand-offs: GCS signed URL → Pub/Sub and S3 presigned URL → SQS, via signed object metadata
- **Report** every flow as already covered, fixed, or not covered, with what it would take to close each remaining gap. When propagation already works end to end, the report says so and the skill changes nothing.

## Example prompts

```
"Make our services propagate the mirrord session baggage header across Kafka and HTTP"

"Our mirrord filter matches the API but not the worker that consumes from SQS — fix propagation"

"Check whether our OpenTelemetry setup already forwards baggage to Pub/Sub consumers"

"Uploads go through a GCS signed URL and a Pub/Sub notification — keep the session through that"

"Which flows in this repo would drop the baggage header?"
```

## Prerequisites

- Source access to the services in the request path
- Nothing mirrord-specific to install. Verification with a live session uses mirrord, and queue splitting needs the mirrord Operator (Team / Enterprise)

## Learn more

- [Queue splitting](https://metalbear.com/mirrord/docs/sharing-the-cluster/queue-splitting)
- [HTTP filtering](https://metalbear.com/mirrord/docs/using-mirrord/incoming-traffic/filter-incoming-traffic)
- [OpenTelemetry baggage](https://opentelemetry.io/docs/concepts/signals/baggage/)
